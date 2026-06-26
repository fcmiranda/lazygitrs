use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use serde::Deserialize;
use std::sync::{Arc, Mutex, mpsc};
use tokio::net::TcpListener;

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

#[derive(Clone)]
struct AppState {
    tx: Arc<mpsc::Sender<AcpEvent>>,
    repo_path: Arc<std::path::PathBuf>,
    session_id: SessionId,
}

pub enum AcpEvent {
    ApplyNotes(AgentContext),
}

pub fn spawn_server(
    tx: mpsc::Sender<AcpEvent>,
    repo_path: std::path::PathBuf,
    session_id: SessionId,
) {
    let state = AppState {
        tx: Arc::new(tx),
        repo_path: Arc::new(repo_path),
        session_id,
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
                .with_state(state);

            match TcpListener::bind("127.0.0.1:47657").await {
                Ok(listener) => {
                    let _ = axum::serve(listener, app).await;
                }
                Err(e) => {
                    eprintln!("lazygitrs: Failed to bind ACP server: {}", e);
                }
            }
        });
    });
}

/// `GET /session-api/session` — returns the currently registered AI session
/// ID (if any).
async fn handle_get_session(State(state): State<AppState>) -> Json<serde_json::Value> {
    let id = state.session_id.lock().map(|g| g.clone()).unwrap_or(None);
    Json(serde_json::json!({"sessionId": id}))
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
                    if let Ok(mut guard) = state.session_id.lock() {
                        *guard = Some(id.to_string());
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
                // Clear from .lines.json too.
                let mut lines_file = crate::pager::notes_store::load(&state.repo_path);
                lines_file.session = None;
                crate::pager::notes_store::save(&state.repo_path, lines_file);
                return Json(serde_json::json!({"status": "ok"}));
            }
            // Return notes (optionally filtered by file).
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
