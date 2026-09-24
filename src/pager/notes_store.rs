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
    #[serde(
        rename = "workspacePath",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub workspace_path: Option<String>,
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
    /// Command template with `{{session_id}}` and `{{prompt}}` placeholders
    /// to wake up the AI CLI if `server_url` is not provided.
    #[serde(
        rename = "notifyCommand",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub notify_command: String,
}

impl Default for LinesFile {
    fn default() -> Self {
        Self {
            version: 1,
            revision: 0,
            session: None,
            workspace_path: None,
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

/// Locate the Git metadata directory for a repository or worktree.
///
/// Handles:
/// 1. Standard git repos (`repo_path/.git` is a directory).
/// 2. Git worktrees and submodules (`repo_path/.git` is a file containing `gitdir: <path>`).
pub fn resolve_git_dir(repo_path: &Path) -> Option<std::path::PathBuf> {
    let dot_git = repo_path.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    if dot_git.is_file() {
        if let Ok(content) = std::fs::read_to_string(&dot_git) {
            for line in content.lines() {
                let trimmed = line.trim();
                if let Some(rest) = trimmed.strip_prefix("gitdir:") {
                    let gitdir_str = rest.trim();
                    let gitdir_path = std::path::PathBuf::from(gitdir_str);
                    if gitdir_path.is_absolute() {
                        if gitdir_path.exists() {
                            return Some(gitdir_path);
                        }
                    } else {
                        let combined = repo_path.join(gitdir_path);
                        if combined.exists() {
                            return Some(combined);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Fallback state path under $XDG_STATE_HOME or ~/.local/state/lazygitrs/notes/
fn resolve_xdg_state_path(repo_path: &Path) -> std::path::PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let base = std::env::var("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            std::path::PathBuf::from(home).join(".local").join("state")
        });

    let canonical = repo_path.canonicalize().unwrap_or_else(|_| repo_path.to_path_buf());
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let hash = hasher.finish();

    base.join("lazygitrs")
        .join("notes")
        .join(format!("{:016x}.json", hash))
}

/// The canonical path where notes should be saved.
///
/// Priority:
/// 1. `<git_dir>/info/lines.json` (inside the git metadata directory; completely ignored by git).
/// 2. If not a git repository, fallback to `$XDG_STATE_HOME/lazygitrs/notes/<repo_hash>.json`.
pub fn canonical_notes_path(repo_path: &Path) -> std::path::PathBuf {
    if let Some(git_dir) = resolve_git_dir(repo_path) {
        git_dir.join("info").join("lines.json")
    } else {
        resolve_xdg_state_path(repo_path)
    }
}

/// The path from which notes should be loaded.
///
/// Checks canonical path first, then `<git_dir>/lines.json`, then legacy repo root `.lines.json`,
/// and finally XDG state fallback.
pub fn resolve_load_path(repo_path: &Path) -> std::path::PathBuf {
    if let Some(git_dir) = resolve_git_dir(repo_path) {
        let canonical = git_dir.join("info").join("lines.json");
        if canonical.exists() {
            return canonical;
        }
        let alt_git = git_dir.join("lines.json");
        if alt_git.exists() {
            return alt_git;
        }
    }

    // Check legacy location in repository root
    let legacy = repo_path.join(".lines.json");
    if legacy.exists() {
        return legacy;
    }

    // Check XDG fallback
    let xdg = resolve_xdg_state_path(repo_path);
    if xdg.exists() {
        return xdg;
    }

    // Default to canonical target
    canonical_notes_path(repo_path)
}

/// Load `lines.json` from the repository's git metadata directory or legacy root.
/// Handles both the new wrapped format and the legacy flat-array format.
pub fn load(repo_path: &Path) -> LinesFile {
    let target = resolve_load_path(repo_path);
    if !target.exists() {
        let mut lf = LinesFile::default();
        lf.workspace_path = Some(repo_path.to_string_lossy().to_string());
        return lf;
    }
    let content = match std::fs::read_to_string(&target) {
        Ok(c) => c,
        Err(_) => {
            let mut lf = LinesFile::default();
            lf.workspace_path = Some(repo_path.to_string_lossy().to_string());
            return lf;
        }
    };

    // Try new wrapped format first.
    if let Ok(mut wrapped) = serde_json::from_str::<LinesFile>(&content) {
        wrapped.workspace_path = Some(repo_path.to_string_lossy().to_string());
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
            workspace_path: Some(repo_path.to_string_lossy().to_string()),
            notes,
        };
    }

    let mut lf = LinesFile::default();
    lf.workspace_path = Some(repo_path.to_string_lossy().to_string());
    lf
}

/// Save `lines.json` in the canonical git metadata location (`.git/info/lines.json` or worktree),
/// incrementing the revision counter so AI agents can poll for changes.
///
/// Automatically removes any legacy `.lines.json` file in the repository root to keep `git status` clean.
pub fn save(repo_path: &Path, mut file: LinesFile) {
    file.version = 1;
    file.revision = file.revision.wrapping_add(1);
    let target = canonical_notes_path(repo_path);
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(&file) {
        if std::fs::write(&target, json).is_ok() {
            // Clean up legacy .lines.json in repository working tree to avoid untracked git status pollution
            let legacy = repo_path.join(".lines.json");
            if legacy.exists() && legacy != target {
                let _ = std::fs::remove_file(legacy);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lazygitrs-test-{prefix}-{unique}-{}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn test_resolve_git_dir_standard() {
        let tmp = TempDir::new("notes-std");
        let dot_git = tmp.path().join(".git");
        std::fs::create_dir_all(&dot_git).unwrap();

        let resolved = resolve_git_dir(tmp.path());
        assert_eq!(resolved, Some(dot_git));
        let canonical = canonical_notes_path(tmp.path());
        assert_eq!(canonical, tmp.path().join(".git/info/lines.json"));
    }

    #[test]
    fn test_resolve_git_dir_worktree() {
        let tmp = TempDir::new("notes-wt");
        let worktree_gitdir = tmp.path().join("bare/worktrees/feat");
        std::fs::create_dir_all(&worktree_gitdir).unwrap();

        let wt_root = tmp.path().join("feat");
        std::fs::create_dir_all(&wt_root).unwrap();

        let dot_git_file = wt_root.join(".git");
        std::fs::write(&dot_git_file, format!("gitdir: {}\n", worktree_gitdir.display())).unwrap();

        let resolved = resolve_git_dir(&wt_root);
        assert_eq!(resolved, Some(worktree_gitdir.clone()));
        let canonical = canonical_notes_path(&wt_root);
        assert_eq!(canonical, worktree_gitdir.join("info/lines.json"));
    }

    #[test]
    fn test_legacy_migration_on_save() {
        let tmp = TempDir::new("notes-mig");
        let dot_git = tmp.path().join(".git");
        std::fs::create_dir_all(&dot_git).unwrap();

        // Create legacy .lines.json in root
        let legacy = tmp.path().join(".lines.json");
        std::fs::write(&legacy, r#"{"version":1,"revision":0,"notes":[]}"#).unwrap();
        assert!(legacy.exists());

        // Load loads from legacy
        let mut lf = load(tmp.path());
        assert_eq!(lf.revision, 0);

        // Save writes to .git/info/lines.json and removes legacy file
        lf.notes.push(LinesEntry::new_user(
            "1".to_string(),
            "foo.rs".to_string(),
            10,
            "New",
            "test".to_string(),
        ));
        save(tmp.path(), lf);

        let canonical = tmp.path().join(".git/info/lines.json");
        assert!(canonical.exists());
        assert!(!legacy.exists(), "Legacy .lines.json in root must be removed on save");

        let reloaded = load(tmp.path());
        assert_eq!(reloaded.notes.len(), 1);
        assert_eq!(reloaded.notes[0].comment, "test");
    }
}
