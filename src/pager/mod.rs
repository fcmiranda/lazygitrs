pub mod diff_algo;
pub mod highlight;
pub mod notes_store;
pub mod side_by_side;
pub mod word_diff;

use serde::{Deserialize, Serialize};

/// Types shared across the pager module.

/// Represents a single line in a side-by-side diff.
#[derive(Debug, Clone)]
pub struct DiffLine {
    /// Left side: (line_number, text). None if this line only exists on the right.
    pub old_line: Option<(usize, String)>,
    /// Right side: (line_number, text). None if this line only exists on the left.
    pub new_line: Option<(usize, String)>,
    /// What kind of change this line represents.
    pub change_type: ChangeType,
    /// Word-level diff segments for the old (left) side.
    pub old_segments: Option<Vec<InlineSegment>>,
    /// Word-level diff segments for the new (right) side.
    pub new_segments: Option<Vec<InlineSegment>>,
    /// If set, this line is a file header separator (multi-file diffs).
    pub file_header: Option<String>,
    /// Inline notes attached to this diff line (one entry per saved note).
    pub comment_notes: Vec<CommentNote>,
    /// Index of the file section this line belongs to (for multi-file highlighting).
    pub section_index: usize,
}

/// Who authored a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NoteSource {
    /// Created by the human user inside the TUI.
    #[default]
    User,
    /// Created by an AI agent via the ACP HTTP endpoint.
    Agent,
}

/// Workflow status of a note in the bidirectional review cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum NoteStatus {
    /// User created the note but hasn't sent it to the AI yet.
    #[default]
    New,
    /// Note has been sent to the AI (subprocess spawned).
    Sent,
    /// AI has responded with annotations on the same line.
    Addressed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentNote {
    /// Unique id matching the `.lines.json` entry.
    pub id: String,
    /// Note text (may contain multiple lines).
    pub text: String,
    /// true = old side, false = new side.
    #[serde(skip)]
    pub is_old: bool,
    /// Who authored this note (user or agent).
    #[serde(default)]
    pub source: NoteSource,
    /// Author identifier (e.g. "user", "sonnet", "prism").
    #[serde(default = "default_author")]
    pub author: String,
    /// ISO 8601 creation timestamp.
    #[serde(default)]
    pub created_at: String,
    /// Workflow status for the bidirectional review cycle.
    #[serde(default)]
    pub status: NoteStatus,
    /// Categorization tags (e.g. ["security", "performance"]).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Agent confidence level: "low", "medium", "high" (agent notes only).
    pub confidence: Option<String>,
    /// Detailed explanation separate from the summary text.
    pub rationale: Option<String>,
}

impl CommentNote {
    /// Create a new user note with sensible defaults.
    pub fn new_user(id: String, text: String, is_old: bool) -> Self {
        Self {
            id,
            text,
            is_old,
            source: NoteSource::User,
            author: "user".to_string(),
            created_at: now_iso8601(),
            status: NoteStatus::New,
            tags: Vec::new(),
            confidence: None,
            rationale: None,
        }
    }
}

fn default_author() -> String {
    "user".to_string()
}

/// Current time as an ISO 8601 string (UTC, millisecond precision).
pub fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let millis = dur.subsec_millis();
    // 2026-06-26T10:00:00.000Z
    let (y, mo, d, h, mi, s) = unix_to_ymdhms(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, mo, d, h, mi, s, millis
    )
}

/// Convert unix seconds to (year, month, day, hour, minute, second) in UTC.
fn unix_to_ymdhms(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    // Days since 1970-01-01 → calendar date (proleptic Gregorian, UTC)
    // Algorithm from Howard Hinnant's date algorithms
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let mo = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if mo <= 2 { y + 1 } else { y };
    (year as u64, mo, d, h, m, s)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeType {
    Equal,
    Delete,
    Insert,
    Modified,
}

#[derive(Debug, Clone)]
pub struct InlineSegment {
    pub text: String,
    pub emphasized: bool,
}

/// Expand tabs to spaces.
pub fn expand_tabs(s: &str, tab_width: usize) -> String {
    let mut result = String::with_capacity(s.len());
    let mut col = 0;
    for c in s.chars() {
        if c == '\t' {
            let spaces = tab_width - (col % tab_width);
            for _ in 0..spaces {
                result.push(' ');
            }
            col += spaces;
        } else {
            result.push(c);
            col += 1;
        }
    }
    result
}
