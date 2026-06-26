use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::pager::{CommentNote, NoteSource, NoteStatus, now_iso8601};

/// The on-disk `.lines.json` structure (new wrapped format).
///
/// ```json
/// {
///   "version": 1,
///   "revision": 5,
///   "session": { "sessionId": "...", "cli": "opencode" },
///   "notes": [ { "id": ..., "file": ..., "line": ..., "panel": ..., "comment": ..., ... } ]
/// }
/// ```
///
/// Backward compat: if the file is a flat JSON array, it's treated as the
/// `notes` field with `revision: 0`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinesFile {
    pub version: usize,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionInfo>,
    pub notes: Vec<LinesEntry>,
}

/// Persisted AI session info so lazygitrs can fall back to the last
/// registered session ID if the in-memory state is lost (e.g. after
/// restart).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// Which AI CLI registered (e.g. "opencode", "codex", "gemini").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cli: String,
    /// Base URL of the AI CLI's HTTP server (e.g. "http://127.0.0.1:4096").
    /// When set, lazygitrs pushes prompts directly to the running TUI via
    /// `POST /tui/append-prompt` + `POST /tui/submit-prompt` instead of
    /// spawning a subprocess.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server_url: String,
}

impl Default for LinesFile {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            session: None,
            notes: Vec::new(),
        }
    }
}

/// A single entry in `.lines.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinesEntry {
    pub id: String,
    pub file: String,
    pub line: usize,
    pub panel: String,
    pub comment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub source: NoteSource,
    #[serde(default = "default_author", skip_serializing_if = "is_default_author")]
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created_at: String,
    #[serde(default, skip_serializing_if = "is_default_status")]
    pub status: NoteStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
}

fn default_author() -> String {
    "user".to_string()
}

fn is_default(v: &NoteSource) -> bool {
    *v == NoteSource::User
}

fn is_default_author(s: &str) -> bool {
    s == "user"
}

fn is_default_status(s: &NoteStatus) -> bool {
    *s == NoteStatus::New
}

impl LinesEntry {
    /// Build a `CommentNote` (in-memory struct used by the diff view) from
    /// this on-disk entry.
    pub fn to_comment_note(&self) -> CommentNote {
        CommentNote {
            id: self.id.clone(),
            text: self.comment.clone(),
            is_old: self.panel == "Old",
            source: self.source,
            author: self.author.clone(),
            created_at: self.created_at.clone(),
            status: self.status,
            tags: self.tags.clone(),
            confidence: self.confidence.clone(),
            rationale: self.rationale.clone(),
        }
    }

    /// Create a new user entry with a unique id.
    pub fn new_user(id: String, file: String, line: usize, panel: &str, comment: String) -> Self {
        Self {
            id,
            file,
            line,
            panel: panel.to_string(),
            comment,
            rationale: None,
            source: NoteSource::User,
            author: "user".to_string(),
            created_at: now_iso8601(),
            status: NoteStatus::New,
            tags: Vec::new(),
            confidence: None,
        }
    }
}

/// Load `.lines.json` from the repo root. Handles both the new wrapped
/// format and the legacy flat-array format.
pub fn load(repo_path: &Path) -> LinesFile {
    let target = repo_path.join(".lines.json");
    if !target.exists() {
        return LinesFile::default();
    }
    let content = match std::fs::read_to_string(&target) {
        Ok(c) => c,
        Err(_) => return LinesFile::default(),
    };

    // Try new wrapped format first.
    if let Ok(wrapped) = serde_json::from_str::<LinesFile>(&content) {
        return wrapped;
    }

    // Fall back to legacy flat array.
    if let Ok(flat) = serde_json::from_str::<Vec<serde_json::Value>>(&content) {
        let notes = flat
            .into_iter()
            .filter_map(|v| serde_json::from_value::<LinesEntry>(v).ok())
            .collect();
        return LinesFile {
            version: 1,
            revision: 0,
            session: None,
            notes,
        };
    }

    LinesFile::default()
}

/// Save `.lines.json` in the new wrapped format, incrementing the revision
/// counter so AI agents can poll for changes.
pub fn save(repo_path: &Path, mut file: LinesFile) {
    file.version = 1;
    file.revision = file.revision.wrapping_add(1);
    let target = repo_path.join(".lines.json");
    if let Ok(json) = serde_json::to_string_pretty(&file) {
        let _ = std::fs::write(target, json);
    }
}
