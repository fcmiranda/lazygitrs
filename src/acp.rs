use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use futures_util::Stream;
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// The full agent-context payload pushed by AI CLIs (matches hunk's schema).
#[derive(Deserialize, Debug, Clone)]
pub struct AgentContext {
    pub version: usize,
    pub summary: Option<String>,
    pub files: Vec<AgentFileContext>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AgentFileContext {
    pub path: String,
    pub summary: Option<String>,
    pub annotations: Vec<AgentAnnotation>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct AgentAnnotation {
    pub summary: String,
    pub rationale: Option<String>,
    #[serde(rename = "oldRange")]
    pub old_range: Option<(usize, usize)>,
    #[serde(rename = "newRange")]
    pub new_range: Option<(usize, usize)>,
    /// Optional unique id (if the AI assigns one).
    pub id: Option<String>,
    /// Categorization tags.
    pub tags: Option<Vec<String>>,
    /// Agent confidence: "low", "medium", "high".
    pub confidence: Option<String>,
    /// Who authored this annotation.
    pub author: Option<String>,
    /// ISO 8601 creation timestamp.
    #[serde(rename = "createdAt")]
    pub created_at: Option<String>,
}

/// Shared session ID registered by an AI CLI so the TUI can target the
/// correct session when spawning the notify command.
pub type SessionId = Arc<Mutex<Option<String>>>;

/// Sender for SSE events — used by the GUI to push "note sent" notifications
/// to any connected AI CLI listening on `GET /session-api/events`.
pub type SseSender = broadcast::Sender<String>;

/// Shared server URL of the AI CLI (e.g. opencode's `http://127.0.0.1:4096`).
/// When set, lazygitrs pushes prompts directly to the running TUI.
pub type ServerUrl = Arc<Mutex<Option<String>>>;

/// Shared notify command template registered by an AI CLI.
pub type NotifyCommand = Arc<Mutex<Option<String>>>;

#[derive(Clone)]
struct AppState {
    tx: Arc<mpsc::Sender<AcpEvent>>,
    repo_path: Arc<std::path::PathBuf>,
    session_id: SessionId,
    server_url: ServerUrl,
    notify_command: NotifyCommand,
    sse_tx: SseSender,
}

pub enum AcpEvent {
    ApplyNotes(AgentContext),
    Navigate(NavigateContext),
}

#[derive(Deserialize, Debug, Clone)]
pub struct NavigateContext {
    #[serde(rename = "filePath")]
    pub file_path: String,
    #[serde(rename = "hunkNumber")]
    pub hunk_number: Option<usize>,
    pub side: Option<String>,
    pub line: Option<usize>,
    #[serde(rename = "commentDirection")]
    pub comment_direction: Option<String>,
    #[serde(rename = "combinedView")]
    pub combined_view: Option<bool>,
}

/// Spawn the ACP HTTP server. Returns the SSE broadcast sender so the GUI
/// can push events to connected AI CLIs. The `server_url` is shared so the
/// register handler can update it when an AI CLI provides its URL.
pub fn spawn_server(
    tx: mpsc::Sender<AcpEvent>,
    repo_path: std::path::PathBuf,
    session_id: SessionId,
    server_url: ServerUrl,
    notify_command: NotifyCommand,
) -> SseSender {
    let (sse_tx, _) = broadcast::channel::<String>(16);

    let state = AppState {
        tx: Arc::new(tx),
        repo_path: Arc::new(repo_path),
        session_id,
        server_url,
        notify_command,
        sse_tx: sse_tx.clone(),
    };

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async move {
            let app = Router::new()
                .route("/session-api", post(handle_acp_post))
                .route("/session-api/notes", get(handle_get_notes))
                .route("/session-api/notes/{file}", get(handle_get_notes_for_file))
                .route("/session-api/session", get(handle_get_session))
                .route("/session-api/server", get(handle_get_server))
                .route("/session-api/events", get(handle_sse_events))
                .with_state(state);

            // Retry binding in case the previous process hasn't released
            // the port yet (TIME_WAIT or slow shutdown).
            let mut bound: Option<TcpListener> = None;
            for attempt in 0..10u32 {
                match TcpListener::bind("127.0.0.1:47657").await {
                    Ok(listener) => {
                        bound = Some(listener);
                        break;
                    }
                    Err(e) if attempt < 9 => {
                        eprintln!(
                            "lazygitrs: ACP port 47657 busy (attempt {}), retrying in 500ms: {}",
                            attempt + 1,
                            e
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    Err(e) => {
                        eprintln!("lazygitrs: Failed to bind ACP server after retries: {}", e);
                    }
                }
            }
            if let Some(listener) = bound {
                let _ = axum::serve(listener, app).await;
            }
        });
    });

    sse_tx
}

/// `GET /session-api/session` — returns the currently registered AI session
/// ID (if any).
async fn handle_get_session(State(state): State<AppState>) -> Json<serde_json::Value> {
    let id = state.session_id.lock().map(|g| g.clone()).unwrap_or(None);
    Json(serde_json::json!({"sessionId": id}))
}

/// `GET /session-api/server` — returns the AI CLI's server URL (if any).
async fn handle_get_server(State(state): State<AppState>) -> Json<serde_json::Value> {
    let url = state.server_url.lock().map(|g| g.clone()).unwrap_or(None);
    Json(serde_json::json!({"serverUrl": url}))
}

/// `GET /session-api/notes` — returns the full `.lines.json` content so AI
/// agents can fetch all review notes.
async fn handle_get_notes(State(state): State<AppState>) -> Json<serde_json::Value> {
    let lines_file = crate::pager::notes_store::load(&state.repo_path);
    match serde_json::to_value(&lines_file) {
        Ok(v) => Json(v),
        Err(_) => Json(serde_json::json!({"error": "failed to serialize notes"})),
    }
}

/// `GET /session-api/notes/{file}` — returns notes filtered to a single file.
/// The `{file}` path segment should be URL-encoded if it contains slashes.
async fn handle_get_notes_for_file(
    State(state): State<AppState>,
    Path(file): Path<String>,
) -> Json<serde_json::Value> {
    let lines_file = crate::pager::notes_store::load(&state.repo_path);
    let filtered: Vec<_> = lines_file
        .notes
        .into_iter()
        .filter(|n| n.file == file)
        .collect();
    Json(serde_json::json!({
        "version": lines_file.version,
        "revision": lines_file.revision,
        "file": file,
        "notes": filtered,
    }))
}

/// `GET /session-api/events` — Server-Sent Events stream.
///
/// AI CLIs connect to this endpoint with a long-lived connection
/// (e.g. `curl -N http://127.0.0.1:47657/session-api/events`). When the
/// user presses `S` on a note in lazygitrs, an event is pushed containing
/// the review prompt as JSON.
///
/// Event format: `data: {"type":"note-sent","file":"...","line":N,"note":"...","prompt":"..."}`
async fn handle_sse_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.sse_tx.subscribe();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    yield Ok(Event::default().data(msg));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// Query params for `POST /session-api` with `action: "list"`.
#[derive(Deserialize)]
#[allow(dead_code)]
struct ListQuery {
    #[serde(rename = "type")]
    list_type: Option<String>,
    file: Option<String>,
}

/// `POST /session-api` — main entry point for AI agents.
///
/// Supported actions:
/// - `action: "register", sessionId: "..."` — register the AI session ID so
///   the TUI can target it when the user presses `S`.
/// - `action: "unregister"` — clear the registered session ID.
/// - `action: "list"` — return current notes (optionally filtered by `?file=`).
/// - AgentContext payload (with `files` and `annotations`): pushes AI review
///   notes into the TUI.
async fn handle_acp_post(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    body: axum::body::Bytes,
) -> Json<serde_json::Value> {
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return Json(serde_json::json!({"error": "invalid json"}));
    };

    if let Some(action) = payload.get("action").and_then(|a| a.as_str()) {
        match action {
            // Register the AI session ID for the notify command.
            "register" => {
                if let Some(id) = payload.get("sessionId").and_then(|v| v.as_str()) {
                    // Refuse to overwrite an existing session unless force is set.
                    let force = payload
                        .get("force")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let existing = state.session_id.lock().map(|g| g.clone()).unwrap_or(None);
                    if let Some(ref current) = existing {
                        if current != id && !force {
                            return Json(serde_json::json!({
                                "status": "conflict",
                                "error": "A session is already registered",
                                "currentSessionId": current,
                                "hint": "Send \"force\": true to overwrite, or unregister first"
                            }));
                        }
                    }
                    if let Ok(mut guard) = state.session_id.lock() {
                        *guard = Some(id.to_string());
                    }
                    let server_url_str = payload
                        .get("serverUrl")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Update shared server URL state.
                    if let Ok(mut guard) = state.server_url.lock() {
                        *guard = if server_url_str.is_empty() {
                            None
                        } else {
                            Some(server_url_str.clone())
                        };
                    }
                    let notify_command_str = payload
                        .get("notifyCommand")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // Update shared notify command state.
                    if let Ok(mut guard) = state.notify_command.lock() {
                        *guard = if notify_command_str.is_empty() {
                            None
                        } else {
                            Some(notify_command_str.clone())
                        };
                    }
                    // Persist to .lines.json as fallback for restarts.
                    let cli = payload
                        .get("cli")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let mut lines_file = crate::pager::notes_store::load(&state.repo_path);
                    lines_file.session = Some(crate::pager::notes_store::SessionInfo {
                        session_id: id.to_string(),
                        cli,
                        server_url: server_url_str,
                        notify_command: notify_command_str,
                    });
                    crate::pager::notes_store::save(&state.repo_path, lines_file);
                    return Json(serde_json::json!({"status": "ok", "sessionId": id}));
                }
                return Json(serde_json::json!({"error": "missing sessionId field"}));
            }
            // Clear the registered session ID.
            "unregister" => {
                if let Ok(mut guard) = state.session_id.lock() {
                    *guard = None;
                }
                if let Ok(mut guard) = state.server_url.lock() {
                    *guard = None;
                }
                // Clear from .lines.json too.
                let mut lines_file = crate::pager::notes_store::load(&state.repo_path);
                lines_file.session = None;
                crate::pager::notes_store::save(&state.repo_path, lines_file);
                return Json(serde_json::json!({"status": "ok"}));
            }
            "list" | "comment-list" => {
                let lines_file = crate::pager::notes_store::load(&state.repo_path);
                let mut notes = lines_file.notes;
                if let Some(file) = query.file.as_ref() {
                    notes.retain(|n| &n.file == file);
                }
                return Json(serde_json::json!({
                    "version": lines_file.version,
                    "revision": lines_file.revision,
                    "notes": notes,
                }));
            }
            "navigate" => {
                let Ok(nav) = serde_json::from_value::<NavigateContext>(payload.clone()) else {
                    return Json(serde_json::json!({"error": "invalid navigate payload"}));
                };
                if nav.comment_direction.is_none()
                    && nav.hunk_number.is_none()
                    && (nav.side.is_none() || nav.line.is_none())
                {
                    return Json(
                        serde_json::json!({"error": "navigate requires either hunkNumber or both side and line."}),
                    );
                }
                let _ = state.tx.send(AcpEvent::Navigate(nav));
                return Json(serde_json::json!({"status": "ok"}));
            }
            _ => {}
        }
    }

    // Try to parse as AgentContext (the primary push path).
    if let Ok(ctx) = serde_json::from_value::<AgentContext>(payload.clone()) {
        let _ = state.tx.send(AcpEvent::ApplyNotes(ctx));
        return Json(serde_json::json!({"status": "ok"}));
    }

    // Fallback: nested "context" object.
    if let Some(context) = payload.get("context") {
        if let Ok(ctx) = serde_json::from_value::<AgentContext>(context.clone()) {
            let _ = state.tx.send(AcpEvent::ApplyNotes(ctx));
            return Json(serde_json::json!({"status": "ok"}));
        }
    }

    Json(serde_json::json!({"error": "unknown action or invalid payload"}))
}
