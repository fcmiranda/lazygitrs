pub mod context;
pub mod controller;
pub mod layout;
pub mod modes;
pub mod popup;
pub mod presentation;
pub mod scroll;
pub mod views;

use std::collections::{HashMap, HashSet};
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::config::keybindings::parse_key;
use crate::config::{AppConfig, AppState};
use crate::git::{DEFAULT_COMMIT_LIMIT, GitCommands, MODEL_PART_COUNT, ModelPart};
use crate::model::Model;
use crate::model::file_tree::{CommitFileTreeNode, FileTreeNode, build_file_tree};
use crate::os::platform::Platform;
use crate::pager::side_by_side::{
    DiffPanel, DiffPanelLayout, DiffViewLayout, DiffViewState, TextSelection,
};

use self::context::{ContextId, ContextManager, SideWindow};
use self::layout::LayoutState;
use self::modes::diff_mode::DiffModeState;
use self::modes::patch_building::PatchBuildingState;
use self::modes::rebase_mode::{EntryStatus, RebaseModeState, RebasePhase};
use self::popup::{HelpEntry, HelpSection};
use self::popup::{ListPickerItem, MessageKind, PopupState};

/// Compute the display row index for a given item selection,
/// accounting for category header rows inserted between groups.
fn list_picker_display_idx(items: &[ListPickerItem], sel: usize) -> usize {
    let mut di = 0usize;
    let mut last_cat = String::new();
    for (ei, item) in items.iter().enumerate() {
        if !item.category.is_empty() && item.category != last_cat {
            di += 1; // header row
            last_cat = item.category.clone();
        }
        if ei == sel {
            return di;
        }
        di += 1;
    }
    di
}

/// Compute the visible list height for a list picker popup, given terminal height.
/// Must match the rendering formula: popup 60% height, minus borders (2), search bar + sep + hint (3).
fn list_picker_visible_height(terminal_height: usize) -> usize {
    let popup_h = (terminal_height * 60 / 100)
        .max(10)
        .min(terminal_height.saturating_sub(4));
    popup_h.saturating_sub(2).saturating_sub(3)
}

pub type Term = Terminal<CrosstermBackend<Stdout>>;
const EVENT_DRAIN_LIMIT: usize = 256;

fn has_command_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::META)
}

pub(crate) fn textarea_input(
    textarea: &mut tui_textarea::TextArea<'static>,
    key: KeyEvent,
) -> bool {
    let cmd = has_command_modifier(key.modifiers);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Left if cmd => textarea.move_cursor(tui_textarea::CursorMove::Head),
        KeyCode::Right if cmd => textarea.move_cursor(tui_textarea::CursorMove::End),
        KeyCode::Backspace if cmd => {
            textarea.delete_line_by_head();
        }
        KeyCode::Char('a') if ctrl => textarea.move_cursor(tui_textarea::CursorMove::Head),
        KeyCode::Char('e') if ctrl => textarea.move_cursor(tui_textarea::CursorMove::End),
        KeyCode::Char('u') if ctrl => {
            textarea.delete_line_by_head();
        }
        _ => return textarea.input(key),
    };
    true
}

fn drain_pending_terminal_events(idle_timeout: Duration) {
    for _ in 0..EVENT_DRAIN_LIMIT {
        match event::poll(idle_timeout) {
            Ok(true) => {
                if event::read().is_err() {
                    break;
                }
            }
            Ok(false) | Err(_) => break,
        }
    }
}

/// A completed diff result from the background thread.
pub(crate) struct DiffResult {
    /// Generation counter to discard stale results.
    pub generation: u64,
    /// The diff key this result corresponds to.
    pub diff_key: String,
    /// The computed diff data: (filename, old_content, new_content) or None for empty.
    pub payload: DiffPayload,
}

pub(crate) enum DiffPayload {
    /// Side-by-side diff from old/new content.
    Content {
        filename: String,
        old: String,
        new: String,
    },
    /// Unified diff output from git.
    UnifiedDiff {
        filename: String,
        diff_output: String,
    },
    /// Pre-parsed diff ready to apply (parsing done on background thread).
    Parsed(crate::pager::side_by_side::ParsedDiff),
    /// No diff to show.
    Empty,
}

struct AiCommitJob {
    generation: u64,
    cancel: Arc<AtomicBool>,
    cancel_armed_at: Option<Instant>,
}

struct AiCommitResult {
    generation: u64,
    result: Result<Option<String>>,
}

struct CommitPageResult {
    generation: u64,
    result: Result<Vec<crate::model::Commit>>,
}

const COMMIT_PAGE_PREFETCH_THRESHOLD: usize = 100;

pub struct Gui {
    pub config: Arc<AppConfig>,
    pub git: Arc<GitCommands>,
    pub model: Arc<Mutex<Model>>,
    pub context_mgr: ContextManager,
    pub layout: LayoutState,
    pub popup: PopupState,
    pub diff_view: DiffViewState,
    pub command_log: crate::os::cmd::CommandLog,
    pub show_command_log: bool,
    pub should_quit: bool,
    pub needs_refresh: bool,
    pub needs_files_refresh: bool,
    pub needs_diff_refresh: bool,
    pub pending_navigation: Option<crate::acp::NavigateContext>,
    pub start_in_diff: bool,
    pub start_file_filter: Option<String>,
    pub search_query: String,
    /// Whether search input mode is active (typing into search bar).
    pub search_active: bool,
    /// Indices of items matching the current search in the active panel.
    pub search_matches: Vec<usize>,
    /// Current position within search_matches.
    pub search_match_idx: usize,
    pub screen_mode: ScreenMode,
    pub show_file_tree: bool,
    /// Cached file tree nodes — rebuilt on refresh when tree view is active.
    pub file_tree_nodes: Vec<FileTreeNode>,
    /// Set of collapsed directory paths in the file tree.
    pub collapsed_dirs: HashSet<String>,
    /// Whether the diff/main panel is focused (entered via Enter on a file).
    pub diff_focused: bool,
    /// Whether a diff is currently being loaded on a background thread.
    pub diff_loading: bool,
    /// When the current diff load started (for delayed "Loading..." display).
    pub(crate) diff_loading_since: Option<Instant>,
    /// Track what we last loaded a diff for, to avoid reloading on every frame.
    last_diff_key: String,
    /// Generation counter — incremented on each diff request, used to discard stale results.
    pub(crate) diff_generation: Arc<AtomicU64>,
    /// Sender for background diff loading.
    diff_rx: mpsc::Receiver<DiffResult>,
    /// Keep sender around so we can clone it for background threads.
    pub(crate) diff_tx: mpsc::Sender<DiffResult>,
    /// Receiver for AI commit message generation results.
    ai_commit_rx: mpsc::Receiver<AiCommitResult>,
    /// Sender cloned into background threads for AI commit generation.
    ai_commit_tx: mpsc::Sender<AiCommitResult>,
    /// Receiver for ACP annotations
    acp_rx: mpsc::Receiver<crate::acp::AcpEvent>,
    /// Shared AI session ID registered via `POST /session-api {action:"register"}`.
    /// Used to expand `{{session_id}}` in the `notifyCommand` template.
    acp_session_id: crate::acp::SessionId,
    /// SSE broadcast sender — pushes "note sent" events to connected AI CLIs
    /// listening on `GET /session-api/events`.
    sse_tx: crate::acp::SseSender,
    /// Base URL of the AI CLI's HTTP server (e.g. opencode's
    /// `http://127.0.0.1:4096`). When set, prompts are pushed directly to
    /// the running TUI instead of spawning a subprocess.
    acp_server_url: Arc<Mutex<Option<String>>>,
    /// Notify command template registered by the AI CLI via
    /// `POST /session-api {"action":"register"}`.
    acp_notify_command: Arc<Mutex<Option<String>>>,
    /// Receiver for incremental commit pages loaded after the first capped page.
    commit_page_rx: mpsc::Receiver<CommitPageResult>,
    /// Sender cloned into background threads for incremental commit loading.
    commit_page_tx: mpsc::Sender<CommitPageResult>,
    /// True while a background commit page is in flight.
    commit_page_loading: bool,
    /// True when the last commit page was shorter than the requested page size.
    commit_history_complete: bool,
    /// Generation counter used to discard stale commit-page results after refresh.
    commit_page_generation: u64,
    /// Active AI commit generation job, if one is running.
    ai_commit_job: Option<AiCommitJob>,
    /// Generation counter used to discard stale AI results after cancellation.
    ai_commit_generation: u64,
    /// Receiver for background remote operations (push, pull, fetch).
    remote_op_rx: mpsc::Receiver<Result<()>>,
    /// Sender cloned into background threads for remote operations.
    remote_op_tx: mpsc::Sender<Result<()>>,
    /// Receiver for silent auto-fetch results. Kept separate from remote_op
    /// so auto-fetch failures don't show error popups or clobber a
    /// user-initiated push/pull.
    auto_fetch_rx: mpsc::Receiver<Result<()>>,
    /// Sender cloned into background threads for auto-fetch.
    auto_fetch_tx: mpsc::Sender<Result<()>>,
    /// When the last auto-fetch started. `None` means we haven't fetched yet;
    /// the main loop kicks off an immediate fetch on startup.
    last_auto_fetch_at: Option<Instant>,
    /// True while a background auto-fetch is in flight, so we don't stack them.
    auto_fetch_in_flight: bool,
    /// Receiver for background menu item operations (e.g. fetching PR URLs).
    menu_async_rx: mpsc::Receiver<Result<popup::MenuAsyncResult>>,
    /// Sender cloned into background threads for menu async operations.
    pub(crate) menu_async_tx: mpsc::Sender<Result<popup::MenuAsyncResult>>,
    /// Undo stack: stores reflog hashes for undo/redo.
    undo_reflog_idx: usize,
    /// Patch building mode state.
    pub patch_building: PatchBuildingState,
    /// Diff/compare mode state.
    pub diff_mode: DiffModeState,
    /// Interactive rebase mode state.
    pub rebase_mode: RebaseModeState,
    /// Stashed commit editor popup while commit menu or AI generation is shown.
    pending_commit_popup: Option<PopupState>,
    /// Persists the commit editor across Esc so re-opening doesn't lose typed text.
    /// Cleared on successful commit or explicit Clear from the commit menu.
    pub(crate) saved_commit_popup: Option<PopupState>,
    /// Temporarily holds a menu popup during action execution so async actions can restore it.
    pending_menu_popup: Option<PopupState>,
    /// Search bar textarea (1-line editor for search input).
    search_textarea: Option<tui_textarea::TextArea<'static>>,
    /// Last time a refresh occurred (for 10s background auto-refresh interval).
    last_refresh_at: Instant,
    /// Active branch filter for commits panel. When non-empty, only commits from these branches are shown.
    pub commit_branch_filter: Vec<String>,
    /// Hash of the commit whose files are being viewed in CommitFiles context.
    pub commit_files_hash: String,
    /// First line of the commit message for the commit being viewed.
    pub commit_files_message: String,
    /// Cached commit file tree nodes for the CommitFiles view.
    pub commit_file_tree_nodes: Vec<CommitFileTreeNode>,
    /// Set of collapsed directory paths in the commit file tree.
    pub commit_files_collapsed_dirs: HashSet<String>,
    /// Whether to show tree view for commit files (mirrors show_file_tree).
    pub show_commit_file_tree: bool,
    /// Name of the branch/tag whose commits are being viewed in BranchCommits context.
    pub branch_commits_name: String,
    /// Name of the remote whose branches are being viewed in RemoteBranches context.
    pub remote_branches_name: String,
    /// Parent context to return to when pressing Esc from BranchCommits.
    pub sub_commits_parent_context: context::ContextId,
    /// Parent context to return to when pressing Esc from CommitFiles.
    pub commit_files_parent_context: Option<context::ContextId>,
    /// Receiver for streamed model parts during initial load. Each git data
    /// type arrives independently so the UI can waterfall-display results.
    /// Set to `None` once all parts have been received.
    initial_load_rx: Option<mpsc::Receiver<ModelPart>>,
    /// How many model parts have arrived so far (out of MODEL_PART_COUNT).
    initial_load_received: usize,
    /// Frame counter for the loading spinner animation.
    spinner_frame: usize,
    /// Label shown on the head branch during a remote operation (e.g. "Pushing", "Pulling").
    remote_op_label: Option<String>,
    /// Timestamp when the last remote operation succeeded (for showing a temporary ✓).
    remote_op_success_at: Option<Instant>,
    /// Copied commit hashes for cherry-pick paste (newest first).
    pub cherry_pick_clipboard: Vec<String>,
    /// Anchor index for range selection in commits list (None = not in range mode).
    pub range_select_anchor: Option<usize>,
    /// History of previously submitted commit messages (most recent first).
    pub commit_message_history: Vec<String>,
    /// Current index into commit_message_history when cycling (None = not cycling).
    pub commit_history_idx: Option<usize>,
    /// Stashed current draft when cycling through history.
    commit_history_draft: String,
    /// Current color theme index into COLOR_THEMES.
    pub current_theme_index: usize,
    /// Cache of shortstat summaries per commit hash.  Populated asynchronously
    /// by background threads so the render path never blocks on git.
    pub commit_stats_cache:
        std::sync::Arc<std::sync::Mutex<HashMap<String, crate::model::commit::CommitStat>>>,
    /// Set of commit hashes with an in-flight stat fetch, so we don't spawn
    /// duplicate workers on each frame.
    pub commit_stats_inflight: std::sync::Arc<std::sync::Mutex<HashSet<String>>>,
    /// Cache of full commit messages (subject + body) per hash, fetched
    /// asynchronously so the details panel can render the full description.
    pub commit_messages_cache: std::sync::Arc<std::sync::Mutex<HashMap<String, String>>>,
    /// In-flight guard for full-message fetches.
    pub commit_messages_inflight: std::sync::Arc<std::sync::Mutex<HashSet<String>>>,
    /// Vertical scroll offset (rows) for the commit-details box.  Reset
    /// whenever the selected commit hash changes.
    pub commit_details_scroll: u16,
    /// Hash the current `commit_details_scroll` value corresponds to.  When
    /// render sees a different hash, it resets the scroll.
    pub commit_details_scroll_hash: String,
    /// Whether the commit-details box is visible.  Toggled with `.` in any
    /// commit-related context.
    pub show_commit_details: bool,
    /// Whether the mouse is currently hovering the AI-generate button (✦)
    /// in the commit message popup. Drives tooltip visibility.
    pub commit_ai_button_hovered: bool,
    /// Whether we started directly in diff view (e.g. lazygit --diff)
    pub started_in_diff_mode: bool,
    /// Whether this is a popup instance (popup.yaml config)
    pub is_popup: bool,
    pub grab_column_hovered: bool,
    pub grab_column_dragging: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenMode {
    Normal,
    Half,
    Full,
}

/// Synthesize a unified diff for a new (untracked) file from its raw content.
/// This allows untracked files to be included in combined multi-file diffs.
fn synthesize_new_file_diff(filename: &str, content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let count = lines.len();
    let mut diff = String::new();
    diff.push_str(&format!("diff --git a/{f} b/{f}\n", f = filename));
    diff.push_str("new file mode 100644\n");
    diff.push_str(&format!("--- /dev/null\n"));
    diff.push_str(&format!("+++ b/{}\n", filename));
    diff.push_str(&format!("@@ -0,0 +1,{} @@\n", count));
    for line in &lines {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

impl Gui {
    fn show_error(&mut self, title: &str, err: anyhow::Error) {
        self.popup = PopupState::Message {
            title: title.to_string(),
            message: format!("{:#}", err),
            kind: MessageKind::Error,
        };
    }

    pub fn new(
        config: AppConfig,
        git: GitCommands,
        start_in_diff: bool,
        filter_file: Option<String>,
        is_popup: bool,
    ) -> Result<Self> {
        let (diff_tx, diff_rx) = mpsc::channel();
        let (ai_commit_tx, ai_commit_rx) = mpsc::channel();
        let (commit_page_tx, commit_page_rx) = mpsc::channel();
        let (remote_op_tx, remote_op_rx) = mpsc::channel();
        let (auto_fetch_tx, auto_fetch_rx) = mpsc::channel();
        let (menu_async_tx, menu_async_rx) = mpsc::channel();
        let (acp_tx, acp_rx) = mpsc::channel();
        // Restore session ID and server URL from .lines.json if available
        // (survives restarts).
        let persisted = crate::pager::notes_store::load(git.repo_path()).session;
        let persisted_session = persisted.as_ref().map(|s| s.session_id.clone());
        let persisted_server_url = persisted
            .as_ref()
            .map(|s| s.server_url.clone())
            .filter(|u| !u.is_empty());
        let persisted_notify_cmd = persisted
            .as_ref()
            .map(|s| s.notify_command.clone())
            .filter(|c| !c.is_empty());
        let acp_session_id: crate::acp::SessionId = Arc::new(Mutex::new(persisted_session));
        let acp_server_url: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(persisted_server_url));
        let acp_notify_command: Arc<Mutex<Option<String>>> =
            Arc::new(Mutex::new(persisted_notify_cmd));
        let sse_tx = if is_popup {
            let (tx, _) = tokio::sync::broadcast::channel(16);
            tx
        } else {
            crate::acp::spawn_server(
                acp_tx,
                git.repo_path().to_path_buf(),
                acp_session_id.clone(),
                acp_server_url.clone(),
                acp_notify_command.clone(),
            )
        };
        let show_file_tree = config
            .app_state
            .show_file_tree
            .unwrap_or(config.user_config.gui.show_file_tree);
        let show_command_log_default = config
            .app_state
            .show_command_log
            .unwrap_or(config.user_config.gui.show_command_log);
        let diff_line_wrap = config.app_state.diff_line_wrap.unwrap_or(false);
        let diff_view_layout = config
            .app_state
            .diff_view
            .as_deref()
            .and_then(DiffViewLayout::from_state_value)
            .unwrap_or_else(|| {
                if config.user_config.gui.side_by_side {
                    DiffViewLayout::SideBySide
                } else {
                    DiffViewLayout::Unified
                }
            });
        let show_commit_details = config.app_state.show_commit_details.unwrap_or(true);
        let command_log = crate::os::cmd::new_command_log();
        crate::os::cmd::set_thread_command_log(command_log.clone());

        // Start with an empty model — each piece of data loads in the
        // background and streams in as it becomes ready, so the UI can
        // paint immediately and waterfall-display results.
        let git = Arc::new(git);
        let mut model = Model::default();
        model.repo_name = git.repo_name();
        model.head_hash = git.head_hash().unwrap_or_default();
        model.head_branch_name = git.current_branch_name().unwrap_or_default();

        let (initial_load_tx, initial_load_rx) = mpsc::channel();
        git.load_model_streaming(&initial_load_tx);

        let commit_history = Self::load_commit_history(&config);

        // Resolve saved color theme
        let current_theme_index = config
            .app_state
            .color_theme
            .as_deref()
            .and_then(|id| crate::config::COLOR_THEMES.iter().position(|t| t.id == id))
            .unwrap_or(0);

        Ok(Self {
            config: Arc::new(config),
            git,
            model: Arc::new(Mutex::new(model)),
            initial_load_rx: Some(initial_load_rx),
            initial_load_received: 0,
            context_mgr: ContextManager::new(),
            layout: LayoutState::default(),
            popup: PopupState::None,
            diff_view: {
                let mut dv = DiffViewState::new();
                dv.wrap = diff_line_wrap;
                dv.view_layout = diff_view_layout;
                dv
            },
            command_log,
            show_command_log: show_command_log_default,
            should_quit: false,
            needs_refresh: false,
            needs_files_refresh: false,
            needs_diff_refresh: true,
            pending_navigation: None,
            start_in_diff,
            start_file_filter: filter_file,
            search_query: String::new(),
            search_active: false,
            search_matches: Vec::new(),
            search_match_idx: 0,
            screen_mode: ScreenMode::Normal,
            show_file_tree,
            file_tree_nodes: Vec::new(),
            collapsed_dirs: HashSet::new(),
            diff_focused: false,
            diff_loading: false,
            diff_loading_since: None,
            last_diff_key: String::new(),
            diff_generation: Arc::new(AtomicU64::new(0)),
            diff_rx,
            diff_tx,
            ai_commit_rx,
            ai_commit_tx,
            acp_rx,
            acp_session_id,
            sse_tx,
            acp_server_url,
            acp_notify_command,
            commit_page_rx,
            commit_page_tx,
            commit_page_loading: false,
            commit_history_complete: false,
            commit_page_generation: 0,
            ai_commit_job: None,
            ai_commit_generation: 0,
            remote_op_rx,
            remote_op_tx,
            auto_fetch_rx,
            auto_fetch_tx,
            last_auto_fetch_at: None,
            auto_fetch_in_flight: false,
            menu_async_rx,
            menu_async_tx,
            undo_reflog_idx: 0,
            patch_building: PatchBuildingState::new(),
            diff_mode: DiffModeState::new(),
            rebase_mode: RebaseModeState::new(),
            pending_commit_popup: None,
            saved_commit_popup: None,
            pending_menu_popup: None,
            search_textarea: None,
            last_refresh_at: Instant::now(),
            commit_branch_filter: Vec::new(),
            commit_files_hash: String::new(),
            commit_files_message: String::new(),
            commit_file_tree_nodes: Vec::new(),
            commit_files_collapsed_dirs: HashSet::new(),
            show_commit_file_tree: show_file_tree,
            branch_commits_name: String::new(),
            remote_branches_name: String::new(),
            sub_commits_parent_context: context::ContextId::Branches,
            commit_files_parent_context: None,
            spinner_frame: 0,
            remote_op_label: None,
            remote_op_success_at: None,
            cherry_pick_clipboard: Vec::new(),
            range_select_anchor: None,
            commit_message_history: commit_history,
            commit_history_idx: None,
            commit_history_draft: String::new(),
            current_theme_index,
            commit_stats_cache: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            commit_stats_inflight: std::sync::Arc::new(std::sync::Mutex::new(HashSet::new())),
            commit_messages_cache: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            commit_messages_inflight: std::sync::Arc::new(std::sync::Mutex::new(HashSet::new())),
            commit_details_scroll: 0,
            commit_details_scroll_hash: String::new(),
            show_commit_details,
            commit_ai_button_hovered: false,
            started_in_diff_mode: start_in_diff,
            is_popup,
            grab_column_hovered: false,
            grab_column_dragging: None,
        })
    }

    /// Get the currently active theme.
    pub fn active_theme(&self) -> crate::config::Theme {
        let mut theme = crate::config::COLOR_THEMES
            .get(self.current_theme_index)
            .map(|ct| ct.to_theme())
            .unwrap_or_default();

        let global_border = match &self.config.user_config.gui.border {
            crate::config::user_config::BorderConfig::Global(s) => s.clone(),
            crate::config::user_config::BorderConfig::Granular(map) => map
                .get("default")
                .cloned()
                .unwrap_or_else(|| "rounded".to_string()),
        };

        let (b_type, b_all) = crate::config::Theme::parse_border_type(&global_border);
        theme.border_type = b_type;
        theme.borders = b_all;

        if let crate::config::user_config::BorderConfig::Granular(map) =
            &self.config.user_config.gui.border
        {
            for (k, v) in map {
                let (bt, ba) = crate::config::Theme::parse_border_type(v);
                theme.panel_border_types.insert(k.clone(), bt);
                theme.panel_borders.insert(k.clone(), ba);
            }
        }

        theme
    }

    pub fn run(&mut self) -> Result<()> {
        let (mut terminal, keyboard_enhanced) = setup_terminal()?;

        // Sync layout dimensions with actual terminal size so mouse handling
        // uses the correct geometry from the very first frame.
        let size = terminal.size()?;
        self.layout.update_size(size.width, size.height);

        let result = self.main_loop(&mut terminal);

        restore_terminal(&mut terminal, keyboard_enhanced)?;
        result
    }

    fn main_loop(&mut self, terminal: &mut Term) -> Result<()> {
        loop {
            // Drain any model parts that have arrived from the background load.
            if let Some(rx) = &self.initial_load_rx {
                let mut got_files = false;
                let mut got_rebase_in_progress = false;
                while let Ok(part) = rx.try_recv() {
                    let mut model = self.model.lock().unwrap();
                    match part {
                        ModelPart::Files(v) => {
                            model.set_files(v);
                            got_files = true;
                        }
                        ModelPart::Branches(v) => model.branches = v,
                        ModelPart::Commits(v) => {
                            self.commit_history_complete = v.len() < DEFAULT_COMMIT_LIMIT;
                            model.commits = v;
                        }
                        ModelPart::Stash(v) => model.stash_entries = v,
                        ModelPart::Remotes(v) => model.remotes = v,
                        ModelPart::Tags(v) => model.tags = v,
                        ModelPart::Worktrees(v) => model.worktrees = v,
                        ModelPart::Submodules(v) => model.submodules = v,
                        ModelPart::Reflog(v) => model.reflog_commits = v,
                        ModelPart::DiffStats { added, deleted } => {
                            model.total_additions = added;
                            model.total_deletions = deleted;
                        }
                        ModelPart::RepoStatus {
                            is_rebasing,
                            is_merging,
                            is_cherry_picking,
                            is_bisecting,
                            rebase_onto_hash,
                        } => {
                            model.is_rebasing = is_rebasing;
                            model.is_merging = is_merging;
                            model.is_cherry_picking = is_cherry_picking;
                            model.is_bisecting = is_bisecting;
                            model.rebase_onto_hash = rebase_onto_hash;
                            if is_rebasing {
                                got_rebase_in_progress = true;
                            }
                        }
                        ModelPart::RepoUrl(url) => model.repo_url = url,
                        ModelPart::Contributors(c) => model.contributors = c,
                    }
                    self.initial_load_received += 1;
                }
                // Enter the InProgress rebase view as soon as we know a rebase
                // is on disk — don't wait for a future `refresh()` tick (focus
                // event / auto-refresh interval), which is what made the view
                // pop in ~0.8s after the default screen appeared on startup.
                if got_rebase_in_progress
                    && !self.rebase_mode.active
                    && !self.rebase_mode.in_progress_dismissed
                {
                    self.sync_rebase_progress_view();
                }
                // Rebuild file tree if files arrived this frame.
                if got_files {
                    if self.show_file_tree {
                        let model = self.model.lock().unwrap();
                        self.file_tree_nodes = build_file_tree(&model.files, &self.collapsed_dirs);
                        self.context_mgr.files_list_len_override = Some(self.file_tree_nodes.len());
                    }
                    if self.start_in_diff {
                        self.start_in_diff = false;
                        self.diff_focused = true;
                        self.screen_mode = ScreenMode::Full;

                        if let Some(ref target_file) = self.start_file_filter {
                            let model = self.model.lock().unwrap();
                            if self.show_file_tree {
                                if let Some(idx) = self
                                    .file_tree_nodes
                                    .iter()
                                    .position(|n| !n.is_dir && n.path == *target_file)
                                {
                                    self.context_mgr
                                        .set_selected(crate::gui::context::ContextId::Files, idx);
                                }
                            } else {
                                if let Some(idx) =
                                    model.files.iter().position(|f| f.name == *target_file)
                                {
                                    self.context_mgr.set_selected(
                                        crate::gui::context::ContextId::Files,
                                        idx + 1,
                                    );
                                }
                            }
                        }
                    }
                }
                // Trigger a diff load once any data arrives.
                if self.initial_load_received > 0 {
                    self.needs_diff_refresh = true;
                }
                // All parts received — done loading.
                if self.initial_load_received >= MODEL_PART_COUNT {
                    self.initial_load_rx = None;
                }
            }

            // Request diff loading on background thread if selection changed
            self.maybe_request_diff();

            // Check for completed background diff results
            self.receive_diff_results();
            self.receive_acp_results();

            // Check for AI commit message generation results
            self.receive_ai_commit_results();

            // Check for completed incremental commit page loads
            self.receive_commit_page_results();
            self.maybe_request_more_commits();

            // Check for completed background remote operations
            self.receive_remote_op_results();

            // Check for completed auto-fetch and kick off a new one if due
            self.receive_auto_fetch_results();
            self.maybe_start_auto_fetch();

            // Check for completed background menu item operations
            self.receive_menu_async_results();

            // Advance spinner animation
            self.spinner_frame = self.spinner_frame.wrapping_add(1);

            // Render
            let theme = self.active_theme();
            terminal.draw(|frame| {
                if self.rebase_mode.active {
                    presentation::rebase_mode::render(frame, &mut self.rebase_mode, &theme);
                    // Render popup overlay on top of rebase mode
                    if self.popup != PopupState::None {
                        views::render_popup(
                            frame,
                            &self.popup,
                            frame.area(),
                            self.spinner_frame,
                            &theme,
                            self.commit_ai_button_hovered,
                            !self
                                .config
                                .user_config
                                .git
                                .commit
                                .generate_command
                                .trim()
                                .is_empty(),
                        );
                    } else if self.ai_commit_generation_active() {
                        views::render_loading_overlay(
                            frame,
                            frame.area(),
                            self.spinner_frame,
                            &theme,
                            "AI Commit",
                            "Generating commit message...",
                            Some(("Esc esc", "cancel")),
                        );
                    }
                } else if self.diff_mode.active {
                    let diff_loading_show = self.diff_loading
                        && self
                            .diff_loading_since
                            .map(|t| t.elapsed() >= std::time::Duration::from_millis(50))
                            .unwrap_or(false);
                    presentation::diff_mode::render(
                        frame,
                        &mut self.diff_mode,
                        &mut self.diff_view,
                        &theme,
                        self.diff_loading,
                        diff_loading_show,
                        self.spinner_frame,
                    );
                    // Render popup overlay on top of diff mode (for ? help, errors, etc.)
                    if self.popup != PopupState::None {
                        views::render_popup(
                            frame,
                            &self.popup,
                            frame.area(),
                            self.spinner_frame,
                            &theme,
                            self.commit_ai_button_hovered,
                            !self
                                .config
                                .user_config
                                .git
                                .commit
                                .generate_command
                                .trim()
                                .is_empty(),
                        );
                    } else if self.ai_commit_generation_active() {
                        views::render_loading_overlay(
                            frame,
                            frame.area(),
                            self.spinner_frame,
                            &theme,
                            "AI Commit",
                            "Generating commit message...",
                            Some(("Esc esc", "cancel")),
                        );
                    }
                } else {
                    let model = self.model.lock().unwrap();
                    let search_state = if self.search_active || !self.search_query.is_empty() {
                        Some((
                            self.search_query.as_str(),
                            self.search_matches.len(),
                            self.search_match_idx,
                        ))
                    } else {
                        None
                    };
                    let cmd_log = self.command_log.lock().unwrap();
                    views::render(
                        frame,
                        &model,
                        &mut self.context_mgr,
                        &self.layout,
                        &self.popup,
                        &self.config,
                        &theme,
                        &mut self.diff_view,
                        self.screen_mode,
                        self.show_file_tree,
                        &self.file_tree_nodes,
                        &self.collapsed_dirs,
                        self.diff_focused,
                        search_state,
                        self.search_textarea.as_ref(),
                        &cmd_log,
                        self.show_command_log,
                        &self.commit_branch_filter,
                        self.show_commit_file_tree,
                        &self.commit_file_tree_nodes,
                        &self.commit_files_collapsed_dirs,
                        &self.commit_files_hash,
                        &self.commit_files_message,
                        &self.branch_commits_name,
                        &self.remote_branches_name,
                        self.sub_commits_parent_context,
                        self.spinner_frame,
                        self.remote_op_label.as_deref(),
                        self.remote_op_success_at
                            .map(|t| t.elapsed() < std::time::Duration::from_secs(5))
                            .unwrap_or(false),
                        &self.cherry_pick_clipboard,
                        self.range_select_anchor,
                        self.diff_loading,
                        // Only show "Loading diff..." text after a short delay to avoid jitter on fast loads
                        self.diff_loading
                            && self
                                .diff_loading_since
                                .map(|t| t.elapsed() >= std::time::Duration::from_millis(50))
                                .unwrap_or(false),
                        &self.commit_stats_cache,
                        &self.commit_stats_inflight,
                        &self.commit_messages_cache,
                        &self.commit_messages_inflight,
                        &self.git,
                        &mut self.commit_details_scroll,
                        &mut self.commit_details_scroll_hash,
                        self.show_commit_details,
                        self.commit_ai_button_hovered,
                        !self
                            .config
                            .user_config
                            .git
                            .commit
                            .generate_command
                            .trim()
                            .is_empty(),
                        self.grab_column_hovered,
                    );
                    if self.popup == PopupState::None && self.ai_commit_generation_active() {
                        views::render_loading_overlay(
                            frame,
                            frame.area(),
                            self.spinner_frame,
                            &theme,
                            "AI Commit",
                            "Generating commit message...",
                            Some(("Esc esc", "cancel")),
                        );
                    }
                }
            })?;

            // Handle events
            if event::poll(std::time::Duration::from_millis(16))? {
                match event::read()? {
                    Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Press => {
                        if let Err(err) = self.handle_key(key) {
                            self.show_error("Command failed", err);
                        }
                    }
                    Event::Mouse(mouse) => self.handle_mouse(mouse),
                    Event::Resize(w, h) => {
                        self.layout.update_size(w, h);
                        // Re-flow any active commit-message textarea to the new width so
                        // wrapping stays consistent with what the user sees.
                        let popup_width = (w * 60 / 100).min(60).max(30).min(w);
                        let popup_inner = popup_width.saturating_sub(4) as usize;
                        let config_width = self.config.user_config.git.commit.auto_wrap_width;
                        let effective_width = if config_width > 0 {
                            popup_inner.min(config_width)
                        } else {
                            popup_inner
                        };
                        match &mut self.popup {
                            PopupState::Input {
                                textarea,
                                is_commit: true,
                                ..
                            } => {
                                if effective_width > 0 {
                                    auto_wrap_textarea(textarea, effective_width);
                                }
                            }
                            PopupState::Input {
                                textarea,
                                is_commit: false,
                                ..
                            } => {
                                // Single-line input: re-flow the soft wrap to the new width.
                                let raw: String = textarea.lines().join("");
                                if popup_inner > 0 && !raw.is_empty() {
                                    let mut new_ta = popup::make_textarea("");
                                    new_ta.insert_str(&raw);
                                    soft_wrap_textarea(&mut new_ta, popup_inner);
                                    *textarea = new_ta;
                                }
                            }
                            PopupState::CommitInput {
                                body_textarea,
                                body_state,
                                ..
                            } => {
                                if effective_width > 0 {
                                    body_state.render_into(body_textarea, effective_width);
                                }
                            }
                            _ => {}
                        }
                    }
                    Event::FocusGained if self.config.user_config.git.auto_refresh => {
                        self.needs_refresh = true;
                    }
                    Event::Paste(data) => {
                        self.handle_paste(data);
                    }
                    _ => {}
                }
            }

            // Background auto-refresh on refresher.refreshInterval (0 = disabled).
            let refresh_interval = self.config.user_config.refresher.refresh_interval;
            if self.config.user_config.git.auto_refresh
                && refresh_interval > 0
                && self.last_refresh_at.elapsed().as_secs() >= refresh_interval
            {
                self.needs_refresh = true;
            }

            // Refresh data if needed
            if self.needs_refresh {
                match self.refresh() {
                    Ok(()) => {
                        self.needs_refresh = false;
                        self.needs_files_refresh = false;
                        self.needs_diff_refresh = true;
                        self.last_refresh_at = Instant::now();
                    }
                    Err(err) => {
                        self.needs_refresh = false;
                        self.show_error("Refresh failed", err);
                    }
                }
            } else if self.needs_files_refresh {
                match self.refresh_files_only() {
                    Ok(()) => {
                        self.needs_files_refresh = false;
                        self.needs_diff_refresh = true;
                    }
                    Err(err) => {
                        self.needs_files_refresh = false;
                        self.show_error("Refresh failed", err);
                    }
                }
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }

    fn receive_acp_results(&mut self) {
        while let Ok(event) = self.acp_rx.try_recv() {
            match event {
                crate::acp::AcpEvent::ApplyNotes(ctx) => {
                    self.apply_acp_notes(ctx);
                }
                crate::acp::AcpEvent::Navigate(nav) => {
                    // NOTE: Implemented the navigation endpoint (inspired by hunk).
                    let mut found_idx = None;

                    if nav.combined_view == Some(true) {
                        found_idx = Some(0);
                    } else if self.show_file_tree {
                        found_idx = self
                            .file_tree_nodes
                            .iter()
                            .position(|n| !n.is_dir && n.path == nav.file_path);
                    } else {
                        let model = self.model.lock().unwrap();
                        if let Some(idx) = model.files.iter().position(|f| f.name == nav.file_path)
                        {
                            found_idx = Some(idx + 1);
                        }
                    }

                    if let Some(idx) = found_idx {
                        self.context_mgr
                            .set_selected(crate::gui::context::ContextId::Files, idx);
                        self.needs_diff_refresh = true;
                        self.diff_focused = true;
                        self.pending_navigation = Some(nav.clone());
                    }
                    self.command_log.lock().unwrap().push(format!(
                        "Navigating to {} (hunk: {:?}, line: {:?}, combined: {:?})",
                        nav.file_path, nav.hunk_number, nav.line, nav.combined_view
                    ));
                }
            }
        }
    }

    fn apply_acp_notes(&mut self, ctx: crate::acp::AgentContext) {
        let mut lines_file = crate::pager::notes_store::load(self.git.repo_path());
        let mut last_added_note_id = None;

        for file_ctx in ctx.files {
            for ann in file_ctx.annotations {
                let note_text = if let Some(r) = &ann.rationale {
                    format!("{}\n{}", ann.summary, r)
                } else {
                    ann.summary.clone()
                };

                let id = ann.id.clone().unwrap_or_else(|| {
                    let line = ann
                        .new_range
                        .map(|(s, _)| s)
                        .or_else(|| ann.old_range.map(|(s, _)| s))
                        .unwrap_or(0);
                    format!("{}-{}-agent", file_ctx.path, line)
                });

                let (line, panel) = if let Some((start_line, _)) = ann.new_range {
                    (start_line, "New")
                } else if let Some((start_line, _)) = ann.old_range {
                    (start_line, "Old")
                } else {
                    continue;
                };

                // Mark any existing user notes on the same line as addressed.
                for entry in lines_file.notes.iter_mut() {
                    if entry.file == file_ctx.path
                        && entry.line == line
                        && entry.source == crate::pager::NoteSource::User
                        && entry.status != crate::pager::NoteStatus::Addressed
                    {
                        entry.status = crate::pager::NoteStatus::Addressed;
                    }
                }

                // Remove old agent note with same id, then push the new one.
                lines_file.notes.retain(|c| c.id != id);
                lines_file
                    .notes
                    .push(crate::pager::notes_store::LinesEntry {
                        id: id.clone(),
                        file: file_ctx.path.clone(),
                        line,
                        panel: panel.to_string(),
                        comment: note_text,
                        rationale: ann.rationale.clone(),
                        source: crate::pager::NoteSource::Agent,
                        author: ann.author.unwrap_or_else(|| "agent".to_string()),
                        created_at: ann.created_at.unwrap_or_else(crate::pager::now_iso8601),
                        status: crate::pager::NoteStatus::Addressed,
                        tags: ann.tags.unwrap_or_default(),
                        confidence: ann.confidence,
                    });

                last_added_note_id = Some(id);
            }
        }

        crate::pager::notes_store::save(self.git.repo_path(), lines_file);
        self.diff_view.load_notes(self.git.repo_path());

        if let Some(ref note_id) = last_added_note_id {
            self.diff_view.selected_note = Some(note_id.clone());
            self.diff_focused = true;
            self.ensure_note_visible(note_id);
        }
    }

    /// Receive completed diff results from the background thread (non-blocking).
    fn receive_diff_results(&mut self) {
        // Drain all available results, keeping only the latest valid one
        let current_gen = self.diff_generation.load(Ordering::Relaxed);
        while let Ok(result) = self.diff_rx.try_recv() {
            // Discard stale results from older generations
            if result.generation != current_gen {
                continue;
            }
            self.diff_loading = false;
            self.diff_loading_since = None;
            match result.payload {
                DiffPayload::Content { filename, old, new } => {
                    self.diff_view.load(&filename, &old, &new);
                    self.diff_view.file_exists_on_disk =
                        self.git.repo_path().join(&filename).exists();
                    self.diff_view.load_notes(self.git.repo_path());
                }
                DiffPayload::UnifiedDiff {
                    filename,
                    diff_output,
                } => {
                    self.diff_view
                        .load_from_diff_output(&filename, &diff_output);
                    self.diff_view.file_exists_on_disk =
                        self.git.repo_path().join(&filename).exists();
                    self.diff_view.load_notes(self.git.repo_path());
                }
                DiffPayload::Parsed(parsed) => {
                    self.diff_view.apply_parsed(parsed);
                    self.diff_view.load_notes(self.git.repo_path());
                }
                DiffPayload::Empty => {
                    self.diff_view.reset_keep_prefs();
                }
            }

            self.inject_hunk_comments();

            // Handle pending navigation after diff is loaded
            if let Some(nav) = self.pending_navigation.take() {
                if let Some(line) = nav.line {
                    // Note: `self.diff_view.load_notes` populated `self.diff_view.lines` with notes if they exist.
                    // Let's find the matching note ID or just scroll to the line.
                    // Scroll so the line is somewhat centered.
                    let mut current_file = None;
                    let content_offset = self.diff_view.lines.iter().position(|l| {
                        if let Some(header) = &l.file_header {
                            current_file = Some(header.clone());
                        }
                        let file_matches =
                            current_file.as_ref().map_or(true, |f| f == &nav.file_path);

                        file_matches
                            && (l.new_line.as_ref().map(|(n, _)| *n) == Some(line)
                                || l.old_line.as_ref().map(|(n, _)| *n) == Some(line))
                    });

                    if let Some(offset) = content_offset {
                        self.diff_view.scroll_offset = offset.saturating_sub(10);
                    }

                    // To select the note, we look for a note intersecting this line.
                    // We can just search the loaded `lines_file.notes` in `notes_store`.
                    let lines_file = crate::pager::notes_store::load(self.git.repo_path());
                    if let Some(note) = lines_file
                        .notes
                        .iter()
                        .find(|n| n.file == nav.file_path && n.line == line)
                    {
                        self.diff_view.selected_note = Some(note.id.clone());
                    }
                }
            }
        }
    }

    fn inject_hunk_comments(&mut self) {
        if self.diff_view.lines.is_empty() {
            return;
        }
        let target_path = self.git.repo_path().join(".hunks.json");
        if !target_path.exists() {
            return;
        }
        let content = match std::fs::read_to_string(&target_path) {
            Ok(c) => c,
            Err(_) => return,
        };
        let comments: Vec<serde_json::Value> = serde_json::from_str(&content).unwrap_or_default();
        if comments.is_empty() {
            return;
        }

        let mut insertions = Vec::new();

        for i in 0..self.diff_view.hunk_starts.len() {
            let start = self.diff_view.hunk_starts[i];
            let end = if i + 1 < self.diff_view.hunk_starts.len() {
                self.diff_view.hunk_starts[i + 1]
            } else {
                self.diff_view.lines.len()
            };

            let mut hunk_text = String::new();
            for line in &self.diff_view.lines[start..end] {
                if let Some((_, ref old)) = line.old_line {
                    if let Some((_, ref new)) = line.new_line {
                        if old == new {
                            hunk_text.push_str("  ");
                            hunk_text.push_str(old);
                            hunk_text.push('\n');
                            continue;
                        }
                    }
                }
                if let Some((_, ref old)) = line.old_line {
                    hunk_text.push_str("- ");
                    hunk_text.push_str(old);
                    hunk_text.push('\n');
                } else if let Some((_, ref new)) = line.new_line {
                    hunk_text.push_str("+ ");
                    hunk_text.push_str(new);
                    hunk_text.push('\n');
                }
            }

            let file_path = self.diff_view.file_at_line(start).to_string();

            let hunk_comments: Vec<_> = comments
                .iter()
                .filter(|c| {
                    c["file"].as_str() == Some(&file_path) && c["hunk"].as_str() == Some(&hunk_text)
                })
                .collect();

            for comment in hunk_comments {
                if let Some(note) = comment["comment"].as_str() {
                    let is_old = comment["panel"].as_str() == Some("Old");
                    insertions.push((
                        start,
                        is_old,
                        note.to_string(),
                        self.diff_view.lines[start].section_index,
                    ));
                }
            }
        }

        if !insertions.is_empty() {
            insertions.sort_by_key(|k| k.0);
            for (idx, is_old, note, section_index) in insertions.into_iter().rev() {
                self.diff_view.lines.insert(
                    idx,
                    crate::pager::DiffLine {
                        old_line: None,
                        new_line: None,
                        change_type: crate::pager::ChangeType::Equal,
                        old_segments: None,
                        new_segments: None,
                        file_header: None,
                        comment_notes: vec![crate::pager::CommentNote::new_user(
                            String::new(),
                            note,
                            is_old,
                        )],
                        section_index,
                    },
                );
            }
            self.diff_view.hunk_starts =
                crate::pager::diff_algo::find_hunk_starts(&self.diff_view.lines);
        }
    }

    /// Check for completed AI commit message generation results.
    fn receive_ai_commit_results(&mut self) {
        while let Ok(result) = self.ai_commit_rx.try_recv() {
            let active_generation = self.ai_commit_job.as_ref().map(|job| job.generation);
            if active_generation != Some(result.generation) {
                continue;
            }
            self.ai_commit_job = None;

            match result.result {
                Ok(Some(message)) => {
                    let popup_width = (self.layout.width * 60 / 100).min(60).max(30);
                    let popup_inner = popup_width.saturating_sub(4) as usize;
                    let config_width = self.config.user_config.git.commit.auto_wrap_width;
                    let wrap = if config_width > 0 {
                        popup_inner.min(config_width)
                    } else {
                        popup_inner
                    };

                    // Split AI message into summary (first line) and body (rest).
                    // The AI usually emits a hard-wrapped body (~72-char lines); strip those
                    // wrap-induced breaks so they don't read as user paragraph breaks in the
                    // soft-wrapped editor.
                    let (summary, body) = match message.find('\n') {
                        Some(idx) => {
                            let s = message[..idx].to_string();
                            let raw_body = message[idx + 1..].trim_start_matches('\n').to_string();
                            (s, popup::unwrap_commit_body(&raw_body))
                        }
                        None => (message.clone(), String::new()),
                    };

                    // Helper to populate the two textareas
                    let fill_commit = |stashed: &mut PopupState| {
                        if let PopupState::CommitInput {
                            summary_textarea,
                            body_textarea,
                            body_state,
                            ..
                        } = stashed
                        {
                            summary_textarea.select_all();
                            summary_textarea.cut();
                            summary_textarea.insert_str(&summary);
                            body_state.set_text(body.clone());
                            body_state.render_into(body_textarea, wrap);
                        }
                    };

                    // Restore the stashed commit editor, replacing its textarea content.
                    // This intentionally steals focus when generation completes.
                    if let Some(mut stashed) = self.pending_commit_popup.take() {
                        fill_commit(&mut stashed);
                        self.popup = stashed;
                    } else {
                        let mut summary_ta = popup::make_commit_summary_textarea();
                        summary_ta.insert_str(&summary);
                        let mut body_ta = popup::make_commit_body_textarea();
                        let body_state = popup::BodySoftWrap::from_text(body.clone());
                        if !body.is_empty() {
                            body_state.render_into(&mut body_ta, wrap);
                        }
                        self.popup = PopupState::CommitInput {
                            summary_textarea: summary_ta,
                            body_textarea: body_ta,
                            body_state,
                            focus: popup::CommitInputFocus::Summary,
                            on_confirm: Box::new(|gui, msg| {
                                if !msg.is_empty() {
                                    gui.git.create_commit(msg, false)?;
                                    gui.needs_refresh = true;
                                }
                                Ok(())
                            }),
                        };
                    }
                }
                Ok(None) => {
                    if let Some(stashed) = self.pending_commit_popup.take() {
                        self.saved_commit_popup = Some(stashed);
                    }
                }
                Err(e) => {
                    if let Some(stashed) = self.pending_commit_popup.take() {
                        self.saved_commit_popup = Some(stashed);
                    }
                    self.popup = PopupState::Message {
                        title: "AI generation failed".to_string(),
                        message: format!(
                            "{}\n\nYour commit draft was saved. Open the commit prompt again to restore it.",
                            e
                        ),
                        kind: MessageKind::Error,
                    };
                }
            }
        }
    }

    fn receive_commit_page_results(&mut self) {
        while let Ok(result) = self.commit_page_rx.try_recv() {
            if result.generation != self.commit_page_generation {
                continue;
            }

            self.commit_page_loading = false;
            match result.result {
                Ok(commits) => {
                    let page_len = commits.len();
                    let mut model = self.model.lock().unwrap();
                    let mut seen: HashSet<String> =
                        model.commits.iter().map(|c| c.hash.clone()).collect();
                    model
                        .commits
                        .extend(commits.into_iter().filter(|c| seen.insert(c.hash.clone())));
                    self.commit_history_complete = page_len < DEFAULT_COMMIT_LIMIT;
                    self.context_mgr.clamp_selections(&model);
                }
                Err(e) => {
                    self.commit_history_complete = true;
                    if self.popup == PopupState::None {
                        self.popup = PopupState::Message {
                            title: "Commits".to_string(),
                            message: format!("Could not load more commits: {}", e),
                            kind: MessageKind::Error,
                        };
                    }
                }
            }
        }
    }

    fn maybe_request_more_commits(&mut self) {
        if self.context_mgr.active() != ContextId::Commits
            || self.commit_page_loading
            || self.commit_history_complete
        {
            return;
        }

        let len = {
            let model = self.model.lock().unwrap();
            model.commits.len()
        };
        if len < DEFAULT_COMMIT_LIMIT {
            self.commit_history_complete = true;
            return;
        }

        let selected = self.context_mgr.selected(ContextId::Commits);
        let viewport_end = self
            .context_mgr
            .scroll_offset(ContextId::Commits)
            .saturating_add(self.sidebar_visible_height());
        let near_loaded_tail = selected.saturating_add(COMMIT_PAGE_PREFETCH_THRESHOLD) >= len
            || viewport_end.saturating_add(COMMIT_PAGE_PREFETCH_THRESHOLD) >= len;
        if !near_loaded_tail {
            return;
        }

        self.commit_page_loading = true;
        let generation = self.commit_page_generation;
        let git = Arc::clone(&self.git);
        let tx = self.commit_page_tx.clone();
        let branches = self.commit_branch_filter.clone();

        std::thread::spawn(move || {
            let result = if branches.is_empty() {
                git.load_commits_page(DEFAULT_COMMIT_LIMIT, len)
            } else {
                git.load_commits_for_branches_page(&branches, DEFAULT_COMMIT_LIMIT, len)
            };
            let _ = tx.send(CommitPageResult { generation, result });
        });
    }

    fn reset_commit_pagination(&mut self) {
        self.commit_page_generation = self.commit_page_generation.wrapping_add(1);
        self.commit_page_loading = false;
        self.commit_history_complete = false;
    }

    /// Kick off a silent background `git fetch --all` if auto-fetch is enabled
    /// and the configured interval has elapsed since the last one. No popup,
    /// no status on the head branch — the user shouldn't be interrupted.
    fn maybe_start_auto_fetch(&mut self) {
        if !self.config.user_config.git.auto_fetch {
            return;
        }
        let interval = self.config.user_config.refresher.fetch_interval;
        if interval == 0 {
            return;
        }
        if self.auto_fetch_in_flight {
            return;
        }
        let due = match self.last_auto_fetch_at {
            None => true, // first fetch happens immediately after startup
            Some(t) => t.elapsed().as_secs() >= interval,
        };
        if !due {
            return;
        }
        self.last_auto_fetch_at = Some(Instant::now());
        self.auto_fetch_in_flight = true;
        let git = Arc::clone(&self.git);
        let tx = self.auto_fetch_tx.clone();
        let cmd_log = self.command_log.clone();
        std::thread::spawn(move || {
            crate::os::cmd::set_thread_command_log(cmd_log);
            let result = git.fetch_all_background();
            let _ = tx.send(result);
        });
    }

    /// Collect auto-fetch completions. Success triggers a full refresh so the
    /// branches/commits panes reflect any new upstream commits. Failures
    /// (offline, auth prompt suppressed, etc.) are intentionally silent —
    /// surfacing them as popups every 60s would be worse than missing data.
    fn receive_auto_fetch_results(&mut self) {
        while let Ok(result) = self.auto_fetch_rx.try_recv() {
            self.auto_fetch_in_flight = false;
            if result.is_ok() {
                self.needs_refresh = true;
            }
        }
    }

    /// Check for completed background remote operations (push, pull, fetch).
    fn receive_remote_op_results(&mut self) {
        if let Ok(result) = self.remote_op_rx.try_recv() {
            self.remote_op_label = None;
            match result {
                Ok(()) => {
                    self.needs_refresh = true;
                    self.remote_op_success_at = Some(Instant::now());
                }
                Err(e) => {
                    self.popup = PopupState::Message {
                        title: "Error".to_string(),
                        message: format!("{}", e),
                        kind: MessageKind::Error,
                    };
                }
            }
        }
    }

    /// Execute a menu item action. If `override_idx` is Some, use that index;
    /// otherwise use the currently selected index.
    fn execute_menu_action(&mut self, override_idx: Option<usize>) {
        let popup = std::mem::replace(&mut self.popup, PopupState::None);
        if let PopupState::Menu {
            ref items,
            selected,
            ..
        } = popup
        {
            let idx = override_idx.unwrap_or(selected);
            let has_action = items.get(idx).and_then(|i| i.action.as_ref()).is_some();
            if has_action {
                // Stash the menu so async actions can restore it via start_menu_async.
                self.pending_menu_popup = Some(popup);
                // Call the action from the stashed popup.
                let action_result = {
                    let menu = self.pending_menu_popup.as_ref().unwrap();
                    if let PopupState::Menu { items, .. } = menu {
                        let action = items[idx].action.as_ref().unwrap();
                        // SAFETY: We hold a shared ref to pending_menu_popup while calling
                        // action(self). The action may move the popup out of pending_menu_popup
                        // via start_menu_async (which calls .take()), but it won't invalidate
                        // the action pointer because the action is inside items which are moved
                        // as a whole. We use a raw pointer to avoid the borrow conflict.
                        let action_ptr = action as *const dyn Fn(&mut Gui) -> Result<()>;
                        unsafe { (*action_ptr)(self) }
                    } else {
                        Ok(())
                    }
                };
                match action_result {
                    Err(e) => {
                        self.pending_menu_popup = None;
                        self.popup = PopupState::Message {
                            title: "Error".to_string(),
                            message: format!("{}", e),
                            kind: MessageKind::Error,
                        };
                    }
                    Ok(()) => {
                        if self.pending_menu_popup.is_some() {
                            // Action didn't call start_menu_async — it was synchronous.
                            // Discard the stashed menu (popup stays None = menu closed).
                            self.pending_menu_popup = None;
                        }
                    }
                }
            }
        }
    }

    /// Handle results from background menu item operations.
    fn receive_menu_async_results(&mut self) {
        if let Ok(result) = self.menu_async_rx.try_recv() {
            // Only process if the popup is still a menu with loading state.
            // If the user pressed Esc, the menu is already gone — discard the result.
            let is_menu_loading = matches!(
                &self.popup,
                PopupState::Menu {
                    loading_index: Some(_),
                    ..
                }
            );
            if !is_menu_loading {
                return;
            }
            match result {
                Ok(outcome) => {
                    // Close the menu
                    self.popup = PopupState::None;
                    match outcome {
                        popup::MenuAsyncResult::CopyToClipboard(url) => {
                            if let Err(e) = Platform::copy_to_clipboard(&url) {
                                self.popup = PopupState::Message {
                                    title: "Error".to_string(),
                                    message: format!("{}", e),
                                    kind: MessageKind::Error,
                                };
                            }
                        }
                        popup::MenuAsyncResult::OpenUrl(url) => {
                            if let Err(e) = Platform::open_file(&url) {
                                self.popup = PopupState::Message {
                                    title: "Error".to_string(),
                                    message: format!("{}", e),
                                    kind: MessageKind::Error,
                                };
                            }
                        }
                    }
                }
                Err(e) => {
                    self.popup = PopupState::Message {
                        title: "No PR found".to_string(),
                        message: format!("{}", e),
                        kind: MessageKind::Info,
                    };
                }
            }
        }
    }

    /// Run a remote operation (push/pull/fetch) on a background thread.
    pub fn start_remote_op<F>(&mut self, title: &str, _message: &str, op: F)
    where
        F: FnOnce(&GitCommands) -> Result<()> + Send + 'static,
    {
        if self.remote_op_label.is_some() {
            return;
        }

        // Show operation label on the head branch in the sidebar (e.g. "Pushing", "Pulling").
        let label = match title {
            "Push" => "Pushing",
            "Pull" => "Pulling",
            "Fetch" => "Fetching",
            other => other,
        };
        self.remote_op_label = Some(label.to_string());
        self.remote_op_success_at = None;
        let git = Arc::clone(&self.git);
        let tx = self.remote_op_tx.clone();
        std::thread::spawn(move || {
            let result = op(&git);
            let _ = tx.send(result);
        });
    }

    /// Start an async operation for a menu item. Restores the menu popup with a
    /// loading spinner on the item at `index` and spawns a background thread.
    pub fn start_menu_async<F>(&mut self, index: usize, op: F)
    where
        F: FnOnce(&crate::git::GitCommands) -> Result<popup::MenuAsyncResult> + Send + 'static,
    {
        // Restore the menu popup (stashed by execute_menu_action) with loading_index set.
        if let Some(menu) = self.pending_menu_popup.take() {
            if let PopupState::Menu {
                title,
                items,
                selected,
                ..
            } = menu
            {
                self.popup = PopupState::Menu {
                    title,
                    items,
                    selected,
                    loading_index: Some(index),
                };
            }
        }
        let git = Arc::clone(&self.git);
        let tx = self.menu_async_tx.clone();
        std::thread::spawn(move || {
            let result = op(&git);
            let _ = tx.send(result);
        });
    }

    pub(crate) fn ai_commit_generation_active(&self) -> bool {
        self.ai_commit_job.is_some()
    }

    /// Start AI commit message generation on a background thread.
    pub fn start_ai_commit_generation(&mut self) {
        if self.ai_commit_generation_active() {
            return;
        }

        let git = Arc::clone(&self.git);
        let tx = self.ai_commit_tx.clone();
        let cmd = self.config.user_config.git.commit.generate_command.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        self.ai_commit_generation = self.ai_commit_generation.wrapping_add(1);
        let generation = self.ai_commit_generation;
        self.ai_commit_job = Some(AiCommitJob {
            generation,
            cancel,
            cancel_armed_at: None,
        });

        std::thread::spawn(move || {
            let result = crate::git::ai_commit::generate_commit_message_cancellable(
                git.repo_path(),
                &cmd,
                worker_cancel,
            );
            let _ = tx.send(AiCommitResult { generation, result });
        });
    }

    fn begin_ai_commit_generation_ui(&mut self) {
        if self.ai_commit_generation_active() {
            return;
        }
        self.start_ai_commit_generation();
    }

    pub fn trigger_ai_commit_generation_from_editor(&mut self) {
        let generate_cmd = self.config.user_config.git.commit.generate_command.trim();
        if self.ai_commit_generation_active() {
            return;
        }
        if generate_cmd.is_empty() {
            self.popup = PopupState::Message {
                title: "AI generation unavailable".to_string(),
                message: "Set git.commit.generateCommand in your config first.".to_string(),
                kind: MessageKind::Error,
            };
            return;
        }

        let stashed = std::mem::replace(&mut self.popup, PopupState::None);
        self.pending_commit_popup = Some(stashed);
        self.begin_ai_commit_generation_ui();
    }

    fn handle_ai_commit_cancel_key(&mut self, key: KeyEvent) -> bool {
        if key.code != KeyCode::Esc {
            return false;
        }

        let Some(job) = &mut self.ai_commit_job else {
            return false;
        };

        let now = Instant::now();
        let armed = job
            .cancel_armed_at
            .map(|armed_at| now.duration_since(armed_at) <= Duration::from_millis(900))
            .unwrap_or(false);

        if armed {
            job.cancel.store(true, Ordering::Relaxed);
            self.ai_commit_job = None;
            if let Some(stashed) = self.pending_commit_popup.take() {
                self.saved_commit_popup = Some(stashed);
            }
            true
        } else {
            job.cancel_armed_at = Some(now);
            false
        }
    }

    /// Request diff loading on a background thread if selection changed.
    fn maybe_request_diff(&mut self) {
        // Rebase mode has no diff to load
        if self.rebase_mode.active {
            return;
        }

        // Diff mode has its own diff loading
        if self.diff_mode.active {
            let diff_key = format!("diffmode:{}", self.diff_mode.diff_files_selected);
            if diff_key == self.last_diff_key && !self.needs_diff_refresh {
                return;
            }
            let selection_changed = diff_key != self.last_diff_key;
            self.last_diff_key = diff_key.clone();
            self.needs_diff_refresh = false;

            // Bump generation to invalidate any in-flight results
            let generation = self.diff_generation.fetch_add(1, Ordering::Relaxed) + 1;

            // Clear stale diff when selection changes
            if selection_changed {
                self.diff_view.reset_keep_prefs();
                self.diff_view.load_notes(self.git.repo_path());
            }

            self.diff_loading = true;
            self.diff_loading_since = Some(Instant::now());

            controller::diff_mode::maybe_request_diff(self, generation, diff_key);
            return;
        }

        let active = self.context_mgr.active();
        let selected = self.context_mgr.selected_active();
        let diff_key = format!("{:?}:{}", active, selected);

        if diff_key == self.last_diff_key && !self.needs_diff_refresh {
            return;
        }
        let selection_changed = diff_key != self.last_diff_key;
        self.last_diff_key = diff_key.clone();
        self.needs_diff_refresh = false;

        // Bump generation to invalidate any in-flight results
        let generation = self.diff_generation.fetch_add(1, Ordering::Relaxed) + 1;

        // Clear stale diff when selection changes so user sees "Loading..." instead of old content
        if selection_changed {
            self.diff_view.reset_keep_prefs();
            self.diff_view.load_notes(self.git.repo_path());
        }

        let model = self.model.lock().unwrap();
        match active {
            ContextId::Files => {
                // Files panel: load and parse async on background thread
                let file_idx = if self.show_file_tree {
                    self.file_tree_nodes
                        .get(selected)
                        .and_then(|n| n.file_index)
                } else {
                    Some(selected)
                };
                if let Some(file) = file_idx.and_then(|i| model.files.get(i)) {
                    let name = file.name.clone();
                    let current_path = file.current_path().to_string();
                    let diff_paths: Vec<String> =
                        file.diff_paths().into_iter().map(str::to_string).collect();
                    let has_staged = file.has_staged_changes;
                    let has_unstaged = file.has_unstaged_changes;
                    let tracked = file.tracked;
                    drop(model);

                    let git = Arc::clone(&self.git);
                    let tx = self.diff_tx.clone();
                    let gen_counter = Arc::clone(&self.diff_generation);

                    self.diff_loading = true;
                    self.diff_loading_since = Some(Instant::now());
                    std::thread::spawn(move || {
                        if gen_counter.load(Ordering::Relaxed) != generation {
                            return;
                        }
                        let path_refs: Vec<&str> = diff_paths.iter().map(String::as_str).collect();
                        let diff_result = if has_unstaged {
                            git.diff_file_paths(&path_refs)
                        } else if has_staged {
                            git.diff_file_staged_paths(&path_refs)
                        } else {
                            Ok(String::new())
                        };

                        let exists = git.repo_path().join(&current_path).exists();
                        let payload = match diff_result {
                            Ok(diff) if diff.is_empty() && !tracked => {
                                match git.file_content(&current_path) {
                                    Ok(content) if !content.is_empty() => {
                                        DiffPayload::Parsed(DiffViewState::parse_content(
                                            &current_path,
                                            "",
                                            &content,
                                            4,
                                            exists,
                                        ))
                                    }
                                    _ => DiffPayload::Empty,
                                }
                            }
                            Ok(diff) if diff.is_empty() => DiffPayload::Empty,
                            Ok(diff) => DiffPayload::Parsed(DiffViewState::parse_diff_output(
                                &name, &diff, 4, exists,
                            )),
                            Err(_) => DiffPayload::Empty,
                        };
                        let _ = tx.send(DiffResult {
                            generation,
                            diff_key,
                            payload,
                        });
                    });
                } else if self.show_file_tree {
                    // Directory node: show combined diff of all child files (async)
                    if let Some(node) = self.file_tree_nodes.get(selected) {
                        if node.is_dir && !node.child_file_indices.is_empty() {
                            let child_names: Vec<(String, Vec<String>, bool, bool, bool)> = node
                                .child_file_indices
                                .iter()
                                .filter_map(|&i| model.files.get(i))
                                .map(|f| {
                                    (
                                        f.current_path().to_string(),
                                        f.diff_paths().into_iter().map(str::to_string).collect(),
                                        f.has_unstaged_changes,
                                        f.has_staged_changes,
                                        f.tracked,
                                    )
                                })
                                .collect();
                            let dir_name = node.name.clone();
                            drop(model);

                            let git = Arc::clone(&self.git);
                            let tx = self.diff_tx.clone();
                            let gen_counter = Arc::clone(&self.diff_generation);

                            self.diff_loading = true;
                            self.diff_loading_since = Some(Instant::now());
                            std::thread::spawn(move || {
                                if gen_counter.load(Ordering::Relaxed) != generation {
                                    return;
                                }
                                let mut combined_diff = String::new();
                                for (current_path, diff_paths, has_unstaged, has_staged, tracked) in
                                    &child_names
                                {
                                    if gen_counter.load(Ordering::Relaxed) != generation {
                                        return;
                                    }
                                    let diff = if !tracked {
                                        // Untracked file: synthesize a unified diff from raw content
                                        let content =
                                            git.file_content(current_path).unwrap_or_default();
                                        if content.is_empty() {
                                            String::new()
                                        } else {
                                            synthesize_new_file_diff(current_path, &content)
                                        }
                                    } else if *has_unstaged {
                                        let path_refs: Vec<&str> =
                                            diff_paths.iter().map(String::as_str).collect();
                                        git.diff_file_paths(&path_refs).unwrap_or_default()
                                    } else if *has_staged {
                                        let path_refs: Vec<&str> =
                                            diff_paths.iter().map(String::as_str).collect();
                                        git.diff_file_staged_paths(&path_refs).unwrap_or_default()
                                    } else {
                                        String::new()
                                    };
                                    if !diff.is_empty() {
                                        if !combined_diff.is_empty() {
                                            combined_diff.push('\n');
                                        }
                                        combined_diff.push_str(&diff);
                                    }
                                }

                                let payload = if combined_diff.is_empty() {
                                    DiffPayload::Empty
                                } else {
                                    DiffPayload::Parsed(DiffViewState::parse_diff_output(
                                        &dir_name,
                                        &combined_diff,
                                        4,
                                        true,
                                    ))
                                };
                                let _ = tx.send(DiffResult {
                                    generation,
                                    diff_key,
                                    payload,
                                });
                            });
                        } else {
                            drop(model);
                            self.diff_view.reset_keep_prefs();
                            self.diff_view.load_notes(self.git.repo_path());
                        }
                    } else {
                        drop(model);
                        self.diff_view.reset_keep_prefs();
                        self.diff_view.load_notes(self.git.repo_path());
                    }
                } else {
                    drop(model);
                    self.diff_view.reset_keep_prefs();
                    self.diff_view.load_notes(self.git.repo_path());
                }
            }
            ContextId::Commits => {
                // Commits: load and parse async on background thread
                if let Some(commit) = model.commits.get(selected) {
                    let hash = commit.hash.clone();
                    drop(model);

                    let git = Arc::clone(&self.git);
                    let tx = self.diff_tx.clone();
                    let gen_counter = Arc::clone(&self.diff_generation);

                    self.diff_loading = true;
                    self.diff_loading_since = Some(Instant::now());
                    std::thread::spawn(move || {
                        if gen_counter.load(Ordering::Relaxed) != generation {
                            return;
                        }
                        let payload = if let Ok(diff) = git.diff_commit(&hash) {
                            let filename = format!("commit:{}", &hash[..7.min(hash.len())]);
                            DiffPayload::Parsed(DiffViewState::parse_diff_output(
                                &filename, &diff, 4, false,
                            ))
                        } else {
                            DiffPayload::Empty
                        };
                        let _ = tx.send(DiffResult {
                            generation,
                            diff_key,
                            payload,
                        });
                    });
                }
            }
            ContextId::Reflog => {
                // Reflog: load and parse commit diff async
                if let Some(commit) = model.reflog_commits.get(selected) {
                    let hash = commit.hash.clone();
                    drop(model);

                    let git = Arc::clone(&self.git);
                    let tx = self.diff_tx.clone();
                    let gen_counter = Arc::clone(&self.diff_generation);

                    self.diff_loading = true;
                    self.diff_loading_since = Some(Instant::now());
                    std::thread::spawn(move || {
                        if gen_counter.load(Ordering::Relaxed) != generation {
                            return;
                        }
                        let payload = if let Ok(diff) = git.diff_commit(&hash) {
                            let filename = format!("reflog:{}", &hash[..7.min(hash.len())]);
                            DiffPayload::Parsed(DiffViewState::parse_diff_output(
                                &filename, &diff, 4, false,
                            ))
                        } else {
                            DiffPayload::Empty
                        };
                        let _ = tx.send(DiffResult {
                            generation,
                            diff_key,
                            payload,
                        });
                    });
                }
            }
            ContextId::Stash => {
                // Stash: load and parse async
                if let Some(entry) = model.stash_entries.get(selected) {
                    let index = entry.index;
                    drop(model);

                    let git = Arc::clone(&self.git);
                    let tx = self.diff_tx.clone();
                    let gen_counter = Arc::clone(&self.diff_generation);

                    self.diff_loading = true;
                    self.diff_loading_since = Some(Instant::now());
                    std::thread::spawn(move || {
                        if gen_counter.load(Ordering::Relaxed) != generation {
                            return;
                        }
                        let payload = if let Ok(diff) = git.stash_diff(index) {
                            if diff.is_empty() {
                                DiffPayload::Empty
                            } else {
                                let filename = format!("stash@{{{}}}", index);
                                let exists = git.repo_path().join(&filename).exists();
                                DiffPayload::Parsed(DiffViewState::parse_diff_output(
                                    &filename, &diff, 4, exists,
                                ))
                            }
                        } else {
                            DiffPayload::Empty
                        };
                        let _ = tx.send(DiffResult {
                            generation,
                            diff_key,
                            payload,
                        });
                    });
                } else {
                    drop(model);
                }
            }
            ContextId::BranchCommits => {
                // BranchCommits: load and parse commit diff async
                if let Some(commit) = model.sub_commits.get(selected) {
                    let hash = commit.hash.clone();
                    drop(model);

                    let git = Arc::clone(&self.git);
                    let tx = self.diff_tx.clone();
                    let gen_counter = Arc::clone(&self.diff_generation);

                    self.diff_loading = true;
                    self.diff_loading_since = Some(Instant::now());
                    std::thread::spawn(move || {
                        if gen_counter.load(Ordering::Relaxed) != generation {
                            return;
                        }
                        let payload = if let Ok(diff) = git.diff_commit(&hash) {
                            let filename = format!("commit:{}", &hash[..7.min(hash.len())]);
                            DiffPayload::Parsed(DiffViewState::parse_diff_output(
                                &filename, &diff, 4, false,
                            ))
                        } else {
                            DiffPayload::Empty
                        };
                        let _ = tx.send(DiffResult {
                            generation,
                            diff_key,
                            payload,
                        });
                    });
                } else {
                    drop(model);
                }
            }
            ContextId::CommitFiles | ContextId::StashFiles | ContextId::BranchCommitFiles => {
                // CommitFiles/StashFiles/BranchCommitFiles: load and parse diff async
                let file_idx = if self.show_commit_file_tree {
                    self.commit_file_tree_nodes
                        .get(selected)
                        .and_then(|n| n.file_index)
                } else {
                    Some(selected)
                };
                if let Some(commit_file) = file_idx.and_then(|i| model.commit_files.get(i)) {
                    let name = commit_file.name.clone();
                    let current_path = commit_file.current_path().to_string();
                    let hash = self.commit_files_hash.clone();
                    drop(model);

                    let git = Arc::clone(&self.git);
                    let tx = self.diff_tx.clone();
                    let gen_counter = Arc::clone(&self.diff_generation);

                    self.diff_loading = true;
                    self.diff_loading_since = Some(Instant::now());
                    std::thread::spawn(move || {
                        if gen_counter.load(Ordering::Relaxed) != generation {
                            return;
                        }
                        let payload = if let Ok(diff) = git.diff_commit_file(&hash, &name) {
                            if diff.is_empty() {
                                DiffPayload::Empty
                            } else {
                                let exists = git.repo_path().join(&current_path).exists();
                                DiffPayload::Parsed(DiffViewState::parse_diff_output(
                                    &name, &diff, 4, exists,
                                ))
                            }
                        } else {
                            DiffPayload::Empty
                        };
                        let _ = tx.send(DiffResult {
                            generation,
                            diff_key,
                            payload,
                        });
                    });
                } else if self.show_commit_file_tree {
                    // Directory node in tree view: show combined diff of all child files
                    if let Some(node) = self.commit_file_tree_nodes.get(selected) {
                        if node.is_dir && !node.child_file_indices.is_empty() {
                            let child_names: Vec<String> = node
                                .child_file_indices
                                .iter()
                                .filter_map(|&i| model.commit_files.get(i))
                                .map(|f| f.name.clone())
                                .collect();
                            let dir_name = node.name.clone();
                            let hash = self.commit_files_hash.clone();
                            drop(model);

                            let git = Arc::clone(&self.git);
                            let tx = self.diff_tx.clone();
                            let gen_counter = Arc::clone(&self.diff_generation);

                            self.diff_loading = true;
                            self.diff_loading_since = Some(Instant::now());
                            std::thread::spawn(move || {
                                if gen_counter.load(Ordering::Relaxed) != generation {
                                    return;
                                }
                                let mut combined_diff = String::new();
                                for name in &child_names {
                                    if gen_counter.load(Ordering::Relaxed) != generation {
                                        return;
                                    }
                                    if let Ok(diff) = git.diff_commit_file(&hash, name) {
                                        if !diff.is_empty() {
                                            if !combined_diff.is_empty() {
                                                combined_diff.push('\n');
                                            }
                                            combined_diff.push_str(&diff);
                                        }
                                    }
                                }
                                let payload = if combined_diff.is_empty() {
                                    DiffPayload::Empty
                                } else {
                                    DiffPayload::Parsed(DiffViewState::parse_diff_output(
                                        &dir_name,
                                        &combined_diff,
                                        4,
                                        true,
                                    ))
                                };
                                let _ = tx.send(DiffResult {
                                    generation,
                                    diff_key,
                                    payload,
                                });
                            });
                        } else {
                            drop(model);
                            self.diff_view.reset_keep_prefs();
                            self.diff_view.load_notes(self.git.repo_path());
                        }
                    } else {
                        drop(model);
                        self.diff_view.reset_keep_prefs();
                        self.diff_view.load_notes(self.git.repo_path());
                    }
                } else {
                    // No file selected — clear diff
                    drop(model);
                    self.diff_view.reset_keep_prefs();
                    self.diff_view.load_notes(self.git.repo_path());
                }
            }
            _ => {
                drop(model);
            }
        }
    }

    /// Repo-level keybindings that work regardless of which panel is focused
    /// (including the diff panel). Returns Ok(true) if the key was consumed.
    fn try_handle_global_repo_keys(&mut self, key: KeyEvent) -> Result<bool> {
        let kb = self.config.user_config.keybinding.clone();
        if matches_key(key, &kb.universal.push_files) || matches_key(key, &kb.universal.pull_files)
        {
            controller::remotes::handle_key(self, key, &kb)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if self.handle_ai_commit_cancel_key(key) {
            return Ok(());
        }

        // Popup takes priority
        if self.popup != PopupState::None {
            return self.handle_popup_key(key);
        }

        // Search input mode takes priority
        if self.search_active {
            return self.handle_search_key(key);
        }

        // Rebase mode takes priority over everything
        if self.rebase_mode.active {
            return controller::rebase_mode::handle_key(self, key);
        }

        // Diff mode takes priority over normal UI
        if self.diff_mode.active {
            return controller::diff_mode::handle_key(self, key);
        }

        let keybindings = &self.config.user_config.keybinding;

        // Side-panel resize: orientation-aware.
        // Portrait (vertical stack): side on top, diff on bottom.
        //   Alt+h/l → shrink/expand by step
        //   Alt+k → diff pane full (ratio 0.0), Alt+j → side pane full (ratio 1.0)
        // Landscape (horizontal split): side on left, diff on right.
        //   Alt+h/l → shrink/expand by step, Alt+k → side full, Alt+j → main full
        let portrait = self.screen_mode != ScreenMode::Full
            && self.layout.width <= 84
            && self.layout.height > 25;
        let shrink_key = matches_key(key, &keybindings.universal.shrink_side_panel);
        let expand_key = matches_key(key, &keybindings.universal.expand_side_panel);
        if shrink_key || expand_key {
            const STEP: f64 = 0.05;
            let delta = if shrink_key { -STEP } else { STEP };
            self.layout.side_panel_ratio = (self.layout.side_panel_ratio + delta).clamp(0.0, 1.0);
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.side_panel_full) {
            // Alt+k: diff full in portrait, side full in landscape
            self.layout.side_panel_ratio = if portrait { 0.0 } else { 1.0 };
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.main_panel_full) {
            // Alt+j: side full in portrait, main full in landscape
            self.layout.side_panel_ratio = if portrait { 1.0 } else { 0.0 };
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.reset_side_panel) {
            self.layout.side_panel_ratio = self.config.user_config.gui.side_panel_width;
            return Ok(());
        }

        // Inline edit takes priority
        if self.diff_view.inline_edit.is_some() {
            return self.handle_inline_edit_key(key);
        }

        // When diff panel is focused, handle diff-specific keys
        if self.diff_focused {
            return self.handle_diff_focused_key(key);
        }

        // Global keybindings
        if matches_key(key, &keybindings.universal.quit)
            || matches_key(key, &keybindings.universal.quit_alt1)
        {
            self.should_quit = true;
            return Ok(());
        }

        // Number keys 1-5 to jump to window (press again to cycle tabs)
        if let KeyCode::Char(c @ '1'..='5') = key.code {
            let n = c.to_digit(10).unwrap();
            if let Some(window) = SideWindow::from_number(n) {
                // If we're in a sub-context (CommitFiles), pressing the parent window's
                // number key should exit the sub-context first.
                if self.context_mgr.active() == ContextId::CommitFiles
                    && window == SideWindow::Commits
                {
                    self.context_mgr.set_active(ContextId::Commits);
                    return Ok(());
                }
                if self.context_mgr.active() == ContextId::StashFiles && window == SideWindow::Stash
                {
                    self.context_mgr.set_active(ContextId::Stash);
                    return Ok(());
                }
                if (self.context_mgr.active() == ContextId::BranchCommits
                    || self.context_mgr.active() == ContextId::BranchCommitFiles)
                    && window == SideWindow::Branches
                {
                    if self.context_mgr.active() == ContextId::BranchCommitFiles {
                        self.context_mgr.set_active(ContextId::BranchCommits);
                    } else {
                        self.context_mgr.set_active(ContextId::Branches);
                    }
                    return Ok(());
                }
                if self.context_mgr.active() == ContextId::RemoteBranches
                    && window == SideWindow::Branches
                {
                    self.context_mgr.set_active(ContextId::Remotes);
                    return Ok(());
                }
                self.context_mgr.jump_to_window(window);
                return Ok(());
            }
        }

        // Tab to switch windows
        if matches_key(key, &keybindings.universal.toggle_panel) {
            self.exit_sub_contexts();
            self.context_mgr.next_window();
            return Ok(());
        }

        // Shift+Tab to switch windows in reverse
        if matches_key(key, &keybindings.universal.toggle_panel_reverse) {
            self.exit_sub_contexts();
            self.context_mgr.prev_window();
            return Ok(());
        }

        // Arrow keys / h/l to switch windows
        if matches_key(key, &keybindings.universal.prev_block)
            || matches_key(key, &keybindings.universal.prev_block_alt)
        {
            self.exit_sub_contexts();
            self.context_mgr.prev_window();
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.next_block)
            || matches_key(key, &keybindings.universal.next_block_alt)
        {
            self.exit_sub_contexts();
            self.context_mgr.next_window();
            return Ok(());
        }

        // Navigation within current panel
        if matches_key(key, &keybindings.universal.prev_item)
            || matches_key(key, &keybindings.universal.prev_item_alt)
        {
            let model = self.model.lock().unwrap();
            self.context_mgr.move_selection(-1, &model);
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.next_item)
            || matches_key(key, &keybindings.universal.next_item_alt)
        {
            let model = self.model.lock().unwrap();
            self.context_mgr.move_selection(1, &model);
            return Ok(());
        }

        // Goto top/bottom
        if matches_key(key, &keybindings.universal.goto_top) {
            self.context_mgr.set_selection(0);
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.goto_bottom) {
            let model = self.model.lock().unwrap();
            let len = self.context_mgr.list_len(&model);
            if len > 0 {
                self.context_mgr.set_selection(len - 1);
            }
            return Ok(());
        }

        // Main panel scroll (J/K or shift+arrows for diff scrolling)
        if matches_key(key, &keybindings.universal.scroll_down_main_alt1) {
            self.diff_view.scroll_down(1);
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.scroll_up_main_alt1) {
            self.diff_view.scroll_up(1);
            return Ok(());
        }
        if key.code == KeyCode::PageDown {
            self.diff_view.scroll_down(20);
            return Ok(());
        }
        if key.code == KeyCode::PageUp {
            self.diff_view.scroll_up(20);
            return Ok(());
        }

        // Horizontal scroll (H/L)
        if matches_key(key, &keybindings.universal.scroll_left) {
            self.diff_view.scroll_left(4);
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.scroll_right) {
            self.diff_view.scroll_right(4);
            return Ok(());
        }

        // Next/prev hunk with { and }
        if key.code == KeyCode::Char('{') {
            self.diff_view.prev_hunk();
            return Ok(());
        }
        if key.code == KeyCode::Char('}') {
            self.diff_view.next_hunk();
            return Ok(());
        }

        // Refresh
        if matches_key(key, &keybindings.universal.refresh) {
            self.needs_refresh = true;
            return Ok(());
        }

        // Rebase options menu (global — when rebasing/merging)
        if matches_key(key, &keybindings.universal.create_rebase_options_menu) {
            let model = self.model.lock().unwrap();
            let is_rebasing = model.is_rebasing;
            let is_merging = model.is_merging;
            let is_cherry_picking = model.is_cherry_picking;
            drop(model);

            // If rebasing, re-enter the interactive rebase view
            if is_rebasing {
                if !self.rebase_mode.active {
                    self.rebase_mode.in_progress_dismissed = false;
                    self.sync_rebase_progress_view();
                }
                return Ok(());
            }

            if is_merging || is_cherry_picking {
                return self.show_rebase_options_menu(false, is_merging, is_cherry_picking);
            }
        }

        // Push/Pull (global)
        if self.try_handle_global_repo_keys(key)? {
            return Ok(());
        }
        let keybindings = &self.config.user_config.keybinding;

        // Screen mode toggle (+ to enlarge, _ to shrink, matching lazygit)
        if matches_key(key, &keybindings.universal.next_screen_mode) {
            self.next_screen_mode();
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.prev_screen_mode) {
            self.prev_screen_mode();
            return Ok(());
        }

        // Diff/Compare mode (W)
        if key.code == KeyCode::Char('W') {
            self.diff_mode.enter();
            self.diff_view.reset_keep_prefs();
            return Ok(());
        }

        // Toggle command log (;)
        if key.code == KeyCode::Char(';') {
            self.show_command_log = !self.show_command_log;
            self.persist_command_log_visibility();
            return Ok(());
        }

        // Undo (z)
        if matches_key(key, &keybindings.universal.undo) {
            return self.undo();
        }

        // Redo (ctrl-z)
        if matches_key(key, &keybindings.universal.redo) {
            return self.redo();
        }

        // Patch building mode (<c-p>)
        if matches_key(key, &keybindings.universal.create_patch_options_menu) {
            if self.context_mgr.active() == ContextId::Commits || self.patch_building.active {
                return controller::patch_building::show_patch_menu(self);
            }
        }

        // Help popup (?)
        if key.code == KeyCode::Char('?') {
            self.show_help();
            return Ok(());
        }

        // Start search
        if matches_key(key, &keybindings.universal.start_search) {
            self.search_active = true;
            self.search_query.clear();
            self.search_matches.clear();
            self.search_match_idx = 0;
            let mut ta = tui_textarea::TextArea::default();
            ta.set_cursor_line_style(ratatui::style::Style::default());
            self.search_textarea = Some(ta);
            return Ok(());
        }

        // Next/prev search match, or Esc to dismiss search results
        if !self.search_query.is_empty() {
            if key.code == KeyCode::Esc {
                self.search_query.clear();
                self.search_matches.clear();
                self.search_match_idx = 0;
                return Ok(());
            }
            if matches_key(key, &keybindings.universal.next_match) {
                self.goto_next_search_match();
                return Ok(());
            }
            if matches_key(key, &keybindings.universal.prev_match) {
                self.goto_prev_search_match();
                return Ok(());
            }
        }

        // Universal "I" key: interactive rebase picker
        if key.code == KeyCode::Char('I') {
            self.show_interactive_rebase_picker();
            return Ok(());
        }

        // `.` toggles the commit-details box when in any commit-related
        // context.  Kept outside per-context controllers so the binding is
        // consistent across Commits / BranchCommits / Reflog / CommitFiles.
        if key.code == KeyCode::Char('.')
            && self.context_has_commit_details()
            && !(self.show_commit_file_tree
                && matches!(
                    self.context_mgr.active(),
                    ContextId::CommitFiles | ContextId::BranchCommitFiles | ContextId::StashFiles
                ))
        {
            self.show_commit_details = !self.show_commit_details;
            self.persist_commit_details_visibility();
            return Ok(());
        }

        // Context-specific keybindings
        self.handle_context_key(key)?;

        // Custom commands (lowest priority — checked after built-in bindings)
        controller::custom_commands::try_handle_key(self, key)?;

        Ok(())
    }

    fn handle_inline_edit_key(&mut self, key: crossterm::event::KeyEvent) -> Result<()> {
        use crossterm::event::{KeyCode, KeyModifiers};
        let keybindings = &self.config.user_config.keybinding;

        if key.code == KeyCode::Esc {
            self.diff_view.inline_edit = None;
            self.diff_view.selected_note = None;
            return Ok(());
        }

        let is_save = matches_key(key, &keybindings.universal.return_key)
            || (key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL));

        if is_save {
            let edit = self.diff_view.inline_edit.as_ref().unwrap();
            let comment = edit.textarea.lines().join("\n");
            let file_path = self.diff_view.file_at_line(edit.line_idx).to_string();

            let is_modified = {
                let model = self.model.lock().unwrap();
                model.files.iter().any(|f| f.name == file_path)
            };

            if !is_modified {
                self.show_error(
                    "Erro",
                    anyhow::anyhow!("Não é permitido salvar notas em arquivos não modificados."),
                );
                self.diff_view.inline_edit = None;
                return Ok(());
            }

            let line_num = self
                .diff_view
                .file_line_number(edit.line_idx, edit.panel)
                .unwrap_or(0);

            let panel_str = match edit.panel {
                crate::pager::side_by_side::DiffPanel::Old => "Old",
                crate::pager::side_by_side::DiffPanel::New => "New",
            };

            let mut lines_file = crate::pager::notes_store::load(self.git.repo_path());

            // Editing existing note: update by id, or remove if emptied.
            // Track the note id so we can select it after saving.
            let saved_note_id = if !edit.editing_id.is_empty() {
                if comment.trim().is_empty() {
                    lines_file.notes.retain(|c| c.id != edit.editing_id);
                    None
                } else if let Some(entry) = lines_file
                    .notes
                    .iter_mut()
                    .find(|c| c.id == edit.editing_id)
                {
                    entry.comment = comment;
                    Some(edit.editing_id.clone())
                } else {
                    None
                }
            } else if !comment.trim().is_empty() {
                // New note: generate unique id.
                let id = format!(
                    "{}-{}-{}-{}",
                    file_path,
                    line_num,
                    panel_str,
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0)
                );
                lines_file
                    .notes
                    .push(crate::pager::notes_store::LinesEntry::new_user(
                        id.clone(),
                        file_path,
                        line_num,
                        panel_str,
                        comment,
                    ));
                Some(id)
            } else {
                None
            };

            crate::pager::notes_store::save(self.git.repo_path(), lines_file);
            self.diff_view.load_notes(self.git.repo_path());
            self.diff_view.inline_edit = None;
            self.diff_view.selected_note = saved_note_id;
            return Ok(());
        }

        if let Some(ref mut edit) = self.diff_view.inline_edit {
            edit.textarea.input(key);
        }
        Ok(())
    }

    fn handle_context_key(&mut self, key: KeyEvent) -> Result<()> {
        let keybindings = self.config.user_config.keybinding.clone();
        let active = self.context_mgr.active();

        match active {
            ContextId::Files => {
                controller::files::handle_key(self, key, &keybindings)?;
            }
            ContextId::Branches => {
                controller::branches::handle_key(self, key, &keybindings)?;
            }
            ContextId::Commits => {
                controller::commits::handle_key(self, key, &keybindings)?;
            }
            ContextId::Reflog => {
                controller::reflog::handle_key(self, key, &keybindings)?;
            }
            ContextId::Stash => {
                controller::stash::handle_key(self, key, &keybindings)?;
            }
            ContextId::Remotes => {
                controller::remotes::handle_key(self, key, &keybindings)?;
            }
            ContextId::Tags => {
                controller::tags::handle_key(self, key, &keybindings)?;
            }
            ContextId::Status => {
                controller::status::handle_key(self, key, &keybindings)?;
            }
            ContextId::Worktrees => {
                controller::worktrees::handle_key(self, key, &keybindings)?;
            }
            ContextId::Submodules => {
                controller::submodules::handle_key(self, key, &keybindings)?;
            }
            ContextId::RemoteBranches => {
                controller::remote_branches::handle_key(self, key, &keybindings)?;
            }
            ContextId::CommitFiles | ContextId::StashFiles | ContextId::BranchCommitFiles => {
                controller::commit_files::handle_key(self, key, &keybindings)?;
            }
            ContextId::BranchCommits => {
                controller::branch_commits::handle_key(self, key, &keybindings)?;
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_diff_focused_search_key(&mut self, key: KeyEvent) -> Result<()> {
        if let Some(ref mut ta) = self.diff_view.search_textarea {
            match key.code {
                KeyCode::Esc => {
                    self.diff_view.dismiss_search();
                }
                KeyCode::Enter => {
                    self.diff_view.dismiss_search();
                    if !self.diff_view.search_matches.is_empty() {
                        self.diff_view.search_match_idx = 0;
                        self.diff_view.scroll_to_current_match();
                    }
                }
                _ => {
                    textarea_input(ta, key);
                    self.diff_view.search_query = ta.lines().join("");
                    self.diff_view.update_search();
                }
            }
        }
        Ok(())
    }

    fn handle_diff_focused_key(&mut self, key: KeyEvent) -> Result<()> {
        // Diff search input mode takes priority
        if self.diff_view.search_active {
            return self.handle_diff_focused_search_key(key);
        }

        // Note actions take priority over text selection
        if self.diff_view.selected_note.is_some() {
            match key.code {
                KeyCode::Char('e') => {
                    if let Some(ref note_id) = self.diff_view.selected_note.clone() {
                        for (i, dl) in self.diff_view.lines.iter().enumerate() {
                            if let Some(note) = dl.comment_notes.iter().find(|n| &n.id == note_id) {
                                // Don't allow editing AI notes
                                if note.source == crate::pager::NoteSource::Agent {
                                    return Ok(());
                                }
                                let panel = if note.is_old {
                                    crate::pager::side_by_side::DiffPanel::Old
                                } else {
                                    crate::pager::side_by_side::DiffPanel::New
                                };
                                self.open_inline_note_editor(i, panel, Some(note.id.clone()));
                                break;
                            }
                        }
                    }
                    return Ok(());
                }
                KeyCode::Char('d') => {
                    if let Some(ref note_id) = self.diff_view.selected_note.clone() {
                        if self.ensure_note_visible(note_id) {
                            self.delete_note(note_id.clone());
                        }
                    }
                    return Ok(());
                }
                KeyCode::Char('S') => {
                    if let Some(ref note_id) = self.diff_view.selected_note.clone() {
                        self.notify_ai_for_note(note_id.clone());
                    }
                    return Ok(());
                }
                KeyCode::Char('c') => {
                    if let Some(ref note_id) = self.diff_view.selected_note.clone() {
                        for (i, dl) in self.diff_view.lines.iter().enumerate() {
                            if dl.comment_notes.iter().any(|n| &n.id == note_id) {
                                let panel = if dl.new_line.is_some() {
                                    crate::pager::side_by_side::DiffPanel::New
                                } else {
                                    crate::pager::side_by_side::DiffPanel::Old
                                };
                                self.open_inline_note_editor(i, panel, None);
                                break;
                            }
                        }
                    }
                    return Ok(());
                }
                _ => {}
            }
        }

        // Handle text selection keys first (y to copy, e to edit, Esc to dismiss)
        if self.diff_view.selection.is_some() {
            let is_click = self.diff_view.selection.as_ref().unwrap().is_click;
            let can_edit = self.diff_view.file_exists_on_disk;
            match key.code {
                KeyCode::Char('e') if can_edit => {
                    let sel_ref = self.diff_view.selection.as_ref().unwrap();
                    let line = sel_ref.edit_line_number;
                    // Compute column from terminal position using the same layout as the mouse handler
                    let (top_row, top_col, _, _) = sel_ref.normalized();
                    let main_panel = self.compute_main_panel_rect();
                    let pl = DiffPanelLayout::compute(main_panel, &self.diff_view);
                    let (content_start, _) = pl.content_range(sel_ref.panel);
                    let column = if top_col >= content_start {
                        (top_col - content_start) as usize + self.diff_view.horizontal_scroll + 1
                    } else {
                        1
                    };
                    // Resolve the actual filename for multi-file diffs
                    let (line_idx, line_panel) = if top_row >= pl.inner_y {
                        self.diff_view
                            .line_chunk_panel_at_row(top_row, &pl, sel_ref.panel)
                            .map(|(line_idx, _, panel)| (line_idx, panel))
                            .unwrap_or_else(|| {
                                (
                                    self.diff_view.scroll_offset + (top_row - pl.inner_y) as usize,
                                    sel_ref.panel,
                                )
                            })
                    } else {
                        (0, sel_ref.panel)
                    };
                    let filename = self.diff_view.file_at_line(line_idx).to_string();
                    self.diff_view.selection = None;
                    let abs_path = self.git.repo_path().join(&filename);
                    if !filename.is_empty() && abs_path.exists() {
                        let abs_path = abs_path.to_string_lossy().to_string();
                        let os = &self.config.user_config.os;
                        if let Some(ln) =
                            line.or_else(|| self.diff_view.file_line_number(line_idx, line_panel))
                        {
                            let tpl = if !os.edit_at_line.is_empty() {
                                &os.edit_at_line
                            } else {
                                &os.edit
                            };
                            let _ = crate::config::user_config::OsConfig::run_template_at_line(
                                tpl, &abs_path, ln, column,
                            );
                        } else {
                            let _ = crate::config::user_config::OsConfig::run_template(
                                &os.edit, &abs_path,
                            );
                        }
                    }
                    return Ok(());
                }
                KeyCode::Char('y') if !is_click => {
                    let text = self.diff_view.selection.as_ref().unwrap().text.clone();
                    self.diff_view.selection = None;
                    if !text.is_empty() {
                        crate::os::platform::Platform::copy_to_clipboard(&text)?;
                    }
                    return Ok(());
                }
                KeyCode::Esc => {
                    self.diff_view.selection = None;
                    return Ok(());
                }
                _ => {
                    self.diff_view.selection = None;
                    if is_click {
                        // Don't propagate click-state dismissal as a real keypress
                        return Ok(());
                    }
                }
            }
        }

        // Push/Pull are global — they fire even when the diff panel is focused.
        if self.try_handle_global_repo_keys(key)? {
            return Ok(());
        }

        let keybindings = &self.config.user_config.keybinding;

        // e / o on the diff panel (no active selection) mirror the Files tab:
        // open the working-tree file in the editor (at the first changed hunk)
        // or in the default program.
        if matches_key(key, &keybindings.universal.edit) {
            self.open_diff_file_in_editor();
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.open_file) {
            self.open_diff_file_in_default_program();
            return Ok(());
        }

        // Screen mode cycling works even when diff is focused
        if matches_key(key, &keybindings.universal.next_screen_mode) {
            self.next_screen_mode();
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.prev_screen_mode) {
            self.prev_screen_mode();
            return Ok(());
        }

        // Start diff content search (/)
        if matches_key(key, &keybindings.universal.start_search) {
            self.diff_view.start_search();
            return Ok(());
        }

        // n/N to navigate diff search matches
        if !self.diff_view.search_query.is_empty() {
            if matches_key(key, &keybindings.universal.next_match) {
                self.diff_view.next_search_match();
                return Ok(());
            }
            if matches_key(key, &keybindings.universal.prev_match) {
                self.diff_view.prev_search_match();
                return Ok(());
            }
        }

        if matches_key(key, &keybindings.universal.revert_block) {
            if self.context_mgr.active() == ContextId::Files {
                let hunk_idx = self
                    .diff_view
                    .selected_revert_hunk
                    .or(self.diff_view.hovered_revert_hunk);
                if let Some(hunk_idx) = hunk_idx {
                    self.diff_view.selected_revert_hunk = Some(hunk_idx);
                    self.show_hunk_context_menu(hunk_idx);
                }
            }
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.undo_revert_block) {
            if self.context_mgr.active() == ContextId::Files
                && !self.diff_view.revert_undo_stack.is_empty()
            {
                if let Err(err) = self.undo_last_revert_block() {
                    self.popup = PopupState::Message {
                        title: "Undo revert failed".to_string(),
                        message: format!("{}", err),
                        kind: MessageKind::Error,
                    };
                }
            }
            return Ok(());
        }

        // Toggle command log (;)
        if key.code == KeyCode::Char(';') {
            self.show_command_log = !self.show_command_log;
            self.persist_command_log_visibility();
            return Ok(());
        }

        // Help popup
        if key.code == KeyCode::Char('?') {
            self.show_diff_help();
            return Ok(());
        }

        // Number keys 1-5 to jump to sidebar panels (unfocus diff)
        // Use set_window instead of jump_to_window to avoid cycling tabs,
        // since the user is "arriving" from diff focus, not pressing the same key again.
        if let KeyCode::Char(c @ '1'..='5') = key.code {
            let n = c.to_digit(10).unwrap();
            if let Some(window) = SideWindow::from_number(n) {
                self.diff_focused = false;
                self.context_mgr.set_window(window);
                return Ok(());
            }
        }

        // Configured H/L scroll keybindings
        if matches_key(key, &keybindings.universal.scroll_left) {
            self.diff_view.scroll_left(4);
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.scroll_right) {
            self.diff_view.scroll_right(4);
            return Ok(());
        }

        if matches_key(key, &keybindings.universal.quit)
            || matches_key(key, &keybindings.universal.quit_alt1)
        {
            self.should_quit = true;
            return Ok(());
        }

        if matches_key(key, &keybindings.universal.return_key) {
            if self.diff_view.selected_note.is_some() {
                self.diff_view.selected_note = None;
            } else if self.diff_view.selected_revert_hunk.is_some() {
                self.diff_view.selected_revert_hunk = None;
            } else if !self.diff_view.search_query.is_empty() {
                self.diff_view.clear_search();
            } else {
                self.diff_focused = false;
                if self.started_in_diff_mode {
                    self.started_in_diff_mode = false;
                    self.screen_mode = ScreenMode::Normal;
                    self.layout.side_panel_ratio = 0.25;
                }
            }
            return Ok(());
        }

        if matches_key(key, &keybindings.universal.revert_block) {
            if self.context_mgr.active() == ContextId::Files {
                let hunk_idx = self
                    .diff_view
                    .selected_revert_hunk
                    .or(self.diff_view.hovered_revert_hunk);
                if let Some(hunk_idx) = hunk_idx {
                    self.diff_view.selected_revert_hunk = Some(hunk_idx);
                    self.show_hunk_context_menu(hunk_idx);
                }
            }
            return Ok(());
        }
        if matches_key(key, &keybindings.universal.undo_revert_block) {
            if self.context_mgr.active() == ContextId::Files
                && !self.diff_view.revert_undo_stack.is_empty()
            {
                if let Err(err) = self.undo_last_revert_block() {
                    self.popup = PopupState::Message {
                        title: "Undo revert failed".to_string(),
                        message: format!("{}", err),
                        kind: MessageKind::Error,
                    };
                }
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('c') => {
                if !self.diff_view.lines.is_empty() {
                    let line_idx = if let Some((idx, _)) = self.diff_view.hovered_line {
                        idx
                    } else {
                        // First visible line in diff
                        self.diff_view.scroll_offset
                    };
                    let panel = if let Some((_, p)) = self.diff_view.hovered_line {
                        p
                    } else {
                        crate::pager::side_by_side::DiffPanel::New
                    };
                    self.open_inline_note_editor(line_idx, panel, None);
                }
            }
            // n/N: cycle to next/previous note
            KeyCode::Char('n') => {
                self.cycle_note(true);
            }
            KeyCode::Char('N') => {
                self.cycle_note(false);
            }
            KeyCode::Char(',') => {
                let active = self.context_mgr.active();
                let new_idx = match active {
                    ContextId::Files if self.show_file_tree => {
                        let selected = self.context_mgr.selected(ContextId::Files);
                        crate::model::file_tree::find_parent_idx(&self.file_tree_nodes, selected)
                            .map(|idx| (ContextId::Files, idx))
                    }
                    ContextId::CommitFiles
                    | ContextId::StashFiles
                    | ContextId::BranchCommitFiles
                        if self.show_commit_file_tree =>
                    {
                        let selected = self.context_mgr.selected(active);
                        crate::model::file_tree::find_parent_idx(
                            &self.commit_file_tree_nodes,
                            selected,
                        )
                        .map(|idx| (active, idx))
                    }
                    _ => None,
                };
                if let Some((ctx, idx)) = new_idx {
                    self.context_mgr.set_selected(ctx, idx);
                    self.needs_diff_refresh = true;
                }
                return Ok(());
            }
            KeyCode::Char('.') => {
                let active = self.context_mgr.active();
                let new_idx = match active {
                    ContextId::Files if self.show_file_tree => {
                        let selected = self.context_mgr.selected(ContextId::Files);
                        crate::model::file_tree::find_first_child_idx(
                            &self.file_tree_nodes,
                            selected,
                        )
                        .map(|idx| (ContextId::Files, idx))
                    }
                    ContextId::CommitFiles
                    | ContextId::StashFiles
                    | ContextId::BranchCommitFiles
                        if self.show_commit_file_tree =>
                    {
                        let selected = self.context_mgr.selected(active);
                        crate::model::file_tree::find_first_child_idx(
                            &self.commit_file_tree_nodes,
                            selected,
                        )
                        .map(|idx| (active, idx))
                    }
                    _ => None,
                };
                if let Some((ctx, idx)) = new_idx {
                    self.context_mgr.set_selected(ctx, idx);
                    self.needs_diff_refresh = true;
                }
                return Ok(());
            }
            KeyCode::Char('>') => {
                let active = self.context_mgr.active();
                let new_idx = match active {
                    ContextId::Files if self.show_file_tree => {
                        let selected = self.context_mgr.selected(ContextId::Files);
                        crate::model::file_tree::find_next_sibling_idx(
                            &self.file_tree_nodes,
                            selected,
                        )
                        .map(|idx| (ContextId::Files, idx))
                    }
                    ContextId::CommitFiles
                    | ContextId::StashFiles
                    | ContextId::BranchCommitFiles
                        if self.show_commit_file_tree =>
                    {
                        let selected = self.context_mgr.selected(active);
                        crate::model::file_tree::find_next_sibling_idx(
                            &self.commit_file_tree_nodes,
                            selected,
                        )
                        .map(|idx| (active, idx))
                    }
                    _ => None,
                };
                if let Some((ctx, idx)) = new_idx {
                    self.context_mgr.set_selected(ctx, idx);
                    self.needs_diff_refresh = true;
                }
                return Ok(());
            }
            KeyCode::Char('<') => {
                let active = self.context_mgr.active();
                let new_idx = match active {
                    ContextId::Files if self.show_file_tree => {
                        let selected = self.context_mgr.selected(ContextId::Files);
                        crate::model::file_tree::find_prev_sibling_idx(
                            &self.file_tree_nodes,
                            selected,
                        )
                        .map(|idx| (ContextId::Files, idx))
                    }
                    ContextId::CommitFiles
                    | ContextId::StashFiles
                    | ContextId::BranchCommitFiles
                        if self.show_commit_file_tree =>
                    {
                        let selected = self.context_mgr.selected(active);
                        crate::model::file_tree::find_prev_sibling_idx(
                            &self.commit_file_tree_nodes,
                            selected,
                        )
                        .map(|idx| (active, idx))
                    }
                    _ => None,
                };
                if let Some((ctx, idx)) = new_idx {
                    self.context_mgr.set_selected(ctx, idx);
                    self.needs_diff_refresh = true;
                }
                return Ok(());
            }
            // j/k/up/down scroll line by line
            KeyCode::Char('j') | KeyCode::Down => {
                self.diff_view.scroll_down(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.diff_view.scroll_up(1);
            }
            // h/l/left/right scroll horizontally
            KeyCode::Char('h') | KeyCode::Left => {
                self.diff_view.scroll_left(4);
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.diff_view.scroll_right(4);
            }
            // { and } jump between hunks. In Files context they also select
            // the hunk as the revert target so the marker glyph turns
            // accent-coloured; the scroll motion stays the same as plain
            // hunk navigation (always jumps, even if already in viewport).
            KeyCode::Char('}') => {
                if self.context_mgr.active() == ContextId::Files {
                    self.diff_view.cycle_next_revert_hunk();
                } else {
                    self.diff_view.next_hunk();
                }
            }
            KeyCode::Char('{') => {
                if self.context_mgr.active() == ContextId::Files {
                    self.diff_view.cycle_prev_revert_hunk();
                } else {
                    self.diff_view.prev_hunk();
                }
            }
            // v toggles unified / side-by-side diff layout
            KeyCode::Char('v') => {
                self.diff_view.toggle_view_layout();
                self.persist_diff_view_layout();
            }
            // [ and ] toggle old-only / new-only view
            KeyCode::Char(']') => {
                use crate::pager::side_by_side::DiffSideView;
                self.diff_view.side_view = match self.diff_view.side_view {
                    DiffSideView::NewOnly => DiffSideView::Both,
                    _ => DiffSideView::NewOnly,
                };
            }
            KeyCode::Char('[') => {
                use crate::pager::side_by_side::DiffSideView;
                self.diff_view.side_view = match self.diff_view.side_view {
                    DiffSideView::OldOnly => DiffSideView::Both,
                    _ => DiffSideView::OldOnly,
                };
            }
            // z toggles line wrapping
            KeyCode::Char('z') => {
                self.diff_view.wrap = !self.diff_view.wrap;
                self.diff_view.horizontal_scroll = 0;
                self.persist_diff_line_wrap();
            }
            // t toggles note visibility
            KeyCode::Char('t') => {
                self.diff_view.notes_visible = !self.diff_view.notes_visible;
                if !self.diff_view.notes_visible {
                    self.diff_view.selected_note = None;
                }
            }
            // Page up/down for larger scrolling
            KeyCode::PageDown => {
                self.diff_view.scroll_down(20);
            }
            KeyCode::PageUp => {
                self.diff_view.scroll_up(20);
            }
            // g/G for top/bottom
            KeyCode::Char('g') => {
                self.diff_view.scroll_offset = 0;
            }
            KeyCode::Char('G') => {
                let max = self.diff_view.lines.len().saturating_sub(1);
                self.diff_view.scroll_offset = max;
            }
            _ => {}
        }
        Ok(())
    }

    fn open_diff_file_in_editor(&mut self) {
        let rel_path = self.diff_view.filename.clone();
        if rel_path.is_empty() {
            return;
        }
        let abs_path_buf = self.git.repo_path().join(&rel_path);
        if !abs_path_buf.exists() {
            return;
        }
        let abs_path = abs_path_buf.to_string_lossy().to_string();
        let os = &self.config.user_config.os;

        // Pick the hunk currently at the top of the viewport (after `{`/`}`
        // navigation, scroll_offset sits on a hunk start). Fall back to the
        // most recent hunk before the viewport, then the first hunk.
        let active_hunk_idx = self
            .diff_view
            .hunk_starts
            .iter()
            .rev()
            .find(|&&h| h <= self.diff_view.scroll_offset)
            .copied()
            .or_else(|| self.diff_view.hunk_starts.first().copied());

        let active_hunk_line = active_hunk_idx.and_then(|idx| {
            self.diff_view
                .file_line_number(idx, DiffPanel::New)
                .or_else(|| self.diff_view.file_line_number(idx, DiffPanel::Old))
        });

        if let Some(line) = active_hunk_line {
            let tpl = if !os.edit_at_line.is_empty() {
                &os.edit_at_line
            } else {
                &os.edit
            };
            if !tpl.is_empty() {
                let _ = crate::config::user_config::OsConfig::run_template_at_line(
                    tpl, &abs_path, line, 1,
                );
                return;
            }
        }

        if !os.edit.is_empty() {
            let _ = crate::config::user_config::OsConfig::run_template(&os.edit, &abs_path);
        } else {
            let _ = crate::os::platform::Platform::open_file(&abs_path);
        }
    }

    fn open_diff_file_in_default_program(&mut self) {
        let rel_path = self.diff_view.filename.clone();
        if rel_path.is_empty() {
            return;
        }
        let abs_path_buf = self.git.repo_path().join(&rel_path);
        if !abs_path_buf.exists() {
            return;
        }
        let abs_path = abs_path_buf.to_string_lossy().to_string();
        let open_template = &self.config.user_config.os.open;
        let _ = crate::config::user_config::OsConfig::run_template(open_template, &abs_path);
    }

    fn handle_paste(&mut self, data: String) {
        if data.is_empty() {
            return;
        }
        let popup_width = (self.layout.width * 60 / 100)
            .min(60)
            .max(30)
            .min(self.layout.width);
        let popup_inner = popup_width.saturating_sub(4) as usize;
        let config_width = self.config.user_config.git.commit.auto_wrap_width;
        let effective_width = if config_width > 0 {
            popup_inner.min(config_width)
        } else {
            popup_inner
        };
        match &mut self.popup {
            PopupState::Input {
                textarea,
                is_commit,
                confirm_focused,
                ..
            } => {
                if *confirm_focused {
                    return;
                }
                if *is_commit {
                    textarea.insert_str(&data);
                    if effective_width > 0 {
                        auto_wrap_textarea(textarea, effective_width);
                    }
                } else {
                    // Single-line input: strip newlines from pasted content.
                    let cleaned: String = data.replace('\r', "").replace('\n', " ");
                    textarea.insert_str(&cleaned);
                    if popup_inner > 0 {
                        soft_wrap_textarea(textarea, popup_inner);
                    }
                }
            }
            PopupState::CommitInput {
                focus,
                summary_textarea,
                body_textarea,
                body_state,
                ..
            } => {
                match *focus {
                    popup::CommitInputFocus::Summary => {
                        // Split on first newline: first line into summary, rest into body.
                        match data.find('\n') {
                            Some(idx) => {
                                let s = data[..idx].replace('\r', "");
                                let b = data[idx + 1..].trim_start_matches('\n').to_string();
                                summary_textarea.insert_str(&s);
                                if !b.is_empty() {
                                    body_state.insert_str(&b);
                                    if effective_width > 0 {
                                        body_state.render_into(body_textarea, effective_width);
                                    }
                                }
                            }
                            None => {
                                summary_textarea.insert_str(&data);
                            }
                        }
                    }
                    popup::CommitInputFocus::Body => {
                        body_state.insert_str(&data);
                        if effective_width > 0 {
                            body_state.render_into(body_textarea, effective_width);
                        }
                    }
                }
            }
            PopupState::Help {
                selected,
                scroll_offset,
                search_textarea,
                ..
            } => {
                let cleaned: String = data.replace('\r', "").replace('\n', " ");
                search_textarea.insert_str(&cleaned);
                *selected = 0;
                *scroll_offset = 0;
            }
            PopupState::RefPicker { core, .. } => {
                use crate::gui::popup::ListPickerItem;
                let cleaned: String = data.replace('\r', "").replace('\n', " ");
                core.search_textarea.insert_str(&cleaned);
                let new_search = core.search_textarea.lines().join("");
                if !core.items.is_empty() && core.items[0].category == "[ref]" {
                    core.items.remove(0);
                }
                let new_lower = new_search.to_lowercase();
                if !new_lower.is_empty() {
                    core.items.insert(
                        0,
                        ListPickerItem {
                            value: new_search.trim().to_string(),
                            label: new_search.trim().to_string(),
                            category: "[ref]".to_string(),
                        },
                    );
                    if let Some(idx) = core.items.iter().skip(1).position(|i| {
                        i.label.to_lowercase().contains(&new_lower)
                            || i.value.to_lowercase().contains(&new_lower)
                    }) {
                        core.selected = idx + 1;
                    } else {
                        core.selected = 0;
                    }
                } else {
                    core.selected = 0;
                }
                core.scroll_offset = 0;
            }
            PopupState::ThemePicker { core, .. } => {
                let cleaned: String = data.replace('\r', "").replace('\n', " ");
                core.search_textarea.insert_str(&cleaned);
                let new_search = core.search_textarea.lines().join("");
                let new_lower = new_search.to_lowercase();
                if !new_lower.is_empty() {
                    if let Some(idx) = core
                        .items
                        .iter()
                        .position(|i| i.label.to_lowercase().contains(&new_lower))
                    {
                        core.selected = idx;
                        self.current_theme_index = idx;
                        core.scroll_offset = idx;
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) fn handle_popup_key(&mut self, key: KeyEvent) -> Result<()> {
        let was_help = matches!(self.popup, PopupState::Help { .. });
        let was_ref_picker = matches!(self.popup, PopupState::RefPicker { .. });
        let was_theme_picker = matches!(self.popup, PopupState::ThemePicker { .. });

        match &self.popup {
            PopupState::Confirm { .. } => {
                if key.code == KeyCode::Char('y') || key.code == KeyCode::Enter {
                    let popup = std::mem::replace(&mut self.popup, PopupState::None);
                    if let PopupState::Confirm { on_confirm, .. } = popup {
                        if let Err(e) = on_confirm(self) {
                            self.popup = PopupState::Message {
                                title: "Error".to_string(),
                                message: format!("{}", e),
                                kind: MessageKind::Error,
                            };
                        }
                    }
                } else {
                    self.popup = PopupState::None;
                }
            }
            PopupState::Message { .. } => {
                // Any key dismisses the message
                self.popup = PopupState::None;
            }
            PopupState::Menu {
                items,
                selected: _selected,
                loading_index,
                ..
            } => {
                // Block all input while a menu item is loading (except Esc)
                if loading_index.is_some() && key.code != KeyCode::Esc {
                    return Ok(());
                }
                let _items_len = items.len();
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        if let PopupState::Menu {
                            items, selected, ..
                        } = &mut self.popup
                        {
                            // Skip disabled items
                            let mut next = *selected + 1;
                            while next < items.len() && items[next].action.is_none() {
                                next += 1;
                            }
                            if next < items.len() {
                                *selected = next;
                            }
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        if let PopupState::Menu {
                            items, selected, ..
                        } = &mut self.popup
                        {
                            // Skip disabled items
                            if *selected > 0 {
                                let mut prev = *selected - 1;
                                while prev > 0 && items[prev].action.is_none() {
                                    prev -= 1;
                                }
                                if items[prev].action.is_some() {
                                    *selected = prev;
                                }
                            }
                        }
                    }
                    KeyCode::Enter => {
                        self.execute_menu_action(None);
                    }
                    KeyCode::Esc => {
                        if let Some(stashed) = self.pending_commit_popup.take() {
                            self.popup = stashed;
                        } else {
                            self.popup = PopupState::None;
                        }
                    }
                    KeyCode::Char(c) => {
                        // Check if the typed char matches a menu item shortcut key
                        let key_str = c.to_string();
                        let matched_idx = items
                            .iter()
                            .position(|item| item.key.as_deref() == Some(key_str.as_str()));
                        if let Some(idx) = matched_idx {
                            // Check if the item has an action (not disabled)
                            let has_action = items[idx].action.is_some();
                            if has_action {
                                self.execute_menu_action(Some(idx));
                            }
                            // If disabled, do nothing (stay on menu)
                        }
                        // If no match, ignore the key (stay on menu)
                    }
                    _ => {}
                }
            }
            PopupState::Input {
                is_commit,
                confirm_focused,
                ..
            } => {
                use crossterm::event::KeyModifiers;
                let is_commit = *is_commit;
                let confirm_focused = *confirm_focused;

                // Tab toggles focus between textarea and confirm button (commit only)
                if is_commit && key.code == KeyCode::Tab {
                    if let PopupState::Input {
                        confirm_focused, ..
                    } = &mut self.popup
                    {
                        *confirm_focused = !*confirm_focused;
                    }
                }
                // Confirm: Ctrl+S for commit, Enter on confirm button, Enter for non-commit
                else if (is_commit
                    && key.code == KeyCode::Char('s')
                    && key.modifiers.contains(KeyModifiers::CONTROL))
                    || (is_commit && confirm_focused && key.code == KeyCode::Enter)
                    || (!is_commit && key.code == KeyCode::Enter)
                {
                    let popup = std::mem::replace(&mut self.popup, PopupState::None);
                    if let PopupState::Input {
                        textarea,
                        on_confirm,
                        is_commit: was_commit,
                        ..
                    } = popup
                    {
                        // Commit messages preserve hard-wrapped newlines; single-line inputs
                        // strip soft-wrap newlines to recover the user's literal text.
                        let text = if was_commit {
                            textarea.lines().join("\n")
                        } else {
                            textarea.lines().join("")
                        };
                        // Save to commit history before calling on_confirm
                        if was_commit && !text.trim().is_empty() {
                            // Remove duplicate if it exists
                            self.commit_message_history.retain(|m| m != &text);
                            self.commit_message_history.insert(0, text.clone());
                            // Keep history bounded
                            self.commit_message_history.truncate(50);
                            self.save_commit_history();
                        }
                        self.commit_history_idx = None;
                        if let Err(e) = on_confirm(self, &text) {
                            self.popup = PopupState::Message {
                                title: "Error".to_string(),
                                message: format!("{}", e),
                                kind: MessageKind::Error,
                            };
                        }
                    }
                } else if key.code == KeyCode::Esc {
                    self.popup = PopupState::None;
                    self.commit_history_idx = None;
                } else if is_commit
                    && !confirm_focused
                    && (key.code == KeyCode::Up || key.code == KeyCode::Down)
                    && !self.commit_message_history.is_empty()
                {
                    // Cycle through commit message history with Up/Down
                    if let PopupState::Input { textarea, .. } = &mut self.popup {
                        // Only cycle if on first line (Up) or last line (Down)
                        let cursor_row = textarea.cursor().0;
                        let line_count = textarea.lines().len();
                        let should_cycle = match key.code {
                            KeyCode::Up => cursor_row == 0,
                            KeyCode::Down => cursor_row >= line_count.saturating_sub(1),
                            _ => false,
                        };

                        if should_cycle {
                            let history_len = self.commit_message_history.len();
                            match key.code {
                                KeyCode::Up => {
                                    let new_idx = match self.commit_history_idx {
                                        None => {
                                            // Save current draft
                                            self.commit_history_draft = textarea.lines().join("\n");
                                            0
                                        }
                                        Some(idx) => (idx + 1).min(history_len - 1),
                                    };
                                    self.commit_history_idx = Some(new_idx);
                                    let msg = &self.commit_message_history[new_idx];
                                    let mut new_ta =
                                        popup::make_textarea("Enter commit message...");
                                    new_ta.insert_str(msg);
                                    *textarea = new_ta;
                                }
                                KeyCode::Down => {
                                    match self.commit_history_idx {
                                        Some(0) => {
                                            // Go back to draft
                                            self.commit_history_idx = None;
                                            let draft = self.commit_history_draft.clone();
                                            let mut new_ta =
                                                popup::make_textarea("Enter commit message...");
                                            new_ta.insert_str(&draft);
                                            *textarea = new_ta;
                                        }
                                        Some(idx) => {
                                            let new_idx = idx - 1;
                                            self.commit_history_idx = Some(new_idx);
                                            let msg = &self.commit_message_history[new_idx];
                                            let mut new_ta =
                                                popup::make_textarea("Enter commit message...");
                                            new_ta.insert_str(msg);
                                            *textarea = new_ta;
                                        }
                                        None => {
                                            // Already at draft, do nothing
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            // Not at boundary — forward to textarea for normal cursor movement
                            textarea_input(textarea, key);
                        }
                    }
                } else if is_commit
                    && !confirm_focused
                    && matches_key(
                        key,
                        &self
                            .config
                            .user_config
                            .keybinding
                            .commit_message
                            .commit_menu,
                    )
                {
                    // Commit message editor menu key (configurable)
                    self.show_commit_editor_menu()?;
                } else if !confirm_focused {
                    // Forward all other keys to the textarea (only when textarea is focused)
                    if let PopupState::Input {
                        textarea,
                        is_commit,
                        ..
                    } = &mut self.popup
                    {
                        textarea_input(textarea, key);
                        let popup_width = (self.layout.width * 60 / 100)
                            .min(60)
                            .max(30)
                            .min(self.layout.width);
                        let popup_inner = popup_width.saturating_sub(4) as usize;
                        if *is_commit {
                            // Hard-wrap: line breaks become part of the committed message
                            // (matches lazygit's 72-char convention).
                            let config_width = self.config.user_config.git.commit.auto_wrap_width;
                            let effective_width = if config_width > 0 {
                                popup_inner.min(config_width)
                            } else {
                                popup_inner
                            };
                            if effective_width > 0 {
                                auto_wrap_textarea(textarea, effective_width);
                            }
                        } else if popup_inner > 0 {
                            // Soft-wrap: visual only — newlines are stripped on submit so
                            // the original text (including spaces) round-trips exactly.
                            soft_wrap_textarea(textarea, popup_inner);
                        }
                    }
                }
            }
            PopupState::HunkCommentary { .. } => {
                use crossterm::event::KeyModifiers;
                if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    let popup = std::mem::replace(&mut self.popup, PopupState::None);
                    if let PopupState::HunkCommentary {
                        textarea,
                        file_path,
                        hunk_diff,
                        on_confirm,
                        ..
                    } = popup
                    {
                        let text = textarea.lines().join("\n");
                        if let Err(e) = on_confirm(self, &text, &file_path, &hunk_diff) {
                            self.popup = PopupState::Message {
                                title: "Error".to_string(),
                                message: format!("{}", e),
                                kind: MessageKind::Error,
                            };
                        }
                    }
                } else if key.code == KeyCode::Esc {
                    self.popup = PopupState::None;
                } else {
                    if let PopupState::HunkCommentary { textarea, .. } = &mut self.popup {
                        textarea_input(textarea, key);
                        let popup_width = (self.layout.width * 60 / 100)
                            .min(60)
                            .max(30)
                            .min(self.layout.width);
                        let popup_inner = popup_width.saturating_sub(4) as usize;
                        if popup_inner > 0 {
                            soft_wrap_textarea(textarea, popup_inner);
                        }
                    }
                }
            }
            PopupState::CommitInput { focus, .. } => {
                use crossterm::event::KeyModifiers;
                let focus = *focus;

                // Tab toggles focus between summary and body
                if key.code == KeyCode::Tab {
                    if let PopupState::CommitInput {
                        focus,
                        summary_textarea,
                        body_textarea,
                        ..
                    } = &mut self.popup
                    {
                        *focus = match *focus {
                            popup::CommitInputFocus::Summary => popup::CommitInputFocus::Body,
                            popup::CommitInputFocus::Body => popup::CommitInputFocus::Summary,
                        };
                        // Update cursor visibility based on focus
                        let visible = ratatui::style::Style::default()
                            .add_modifier(ratatui::style::Modifier::REVERSED);
                        let hidden = ratatui::style::Style::default();
                        match *focus {
                            popup::CommitInputFocus::Summary => {
                                summary_textarea.set_cursor_style(visible);
                                body_textarea.set_cursor_style(hidden);
                            }
                            popup::CommitInputFocus::Body => {
                                summary_textarea.set_cursor_style(hidden);
                                body_textarea.set_cursor_style(visible);
                            }
                        }
                    }
                }
                // Insert a newline in the body:
                //   - Enter while focused on Body (the natural keystroke for a multi-line field).
                //   - Shift+Enter from Summary jumps focus to Body and inserts a newline.
                //   - Ctrl+J (some terminals emit this for Shift+Enter) — without this branch it
                //     would hit tui_textarea's default `delete_line_by_head` binding.
                else if (key.code == KeyCode::Enter
                    && (focus == popup::CommitInputFocus::Body
                        || key.modifiers.contains(KeyModifiers::SHIFT)))
                    || (key.code == KeyCode::Char('j')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    let wrap_width = self.commit_body_wrap_width();
                    if let PopupState::CommitInput {
                        focus,
                        summary_textarea,
                        body_textarea,
                        body_state,
                        ..
                    } = &mut self.popup
                    {
                        if *focus == popup::CommitInputFocus::Summary {
                            *focus = popup::CommitInputFocus::Body;
                            let visible = ratatui::style::Style::default()
                                .add_modifier(ratatui::style::Modifier::REVERSED);
                            let hidden = ratatui::style::Style::default();
                            summary_textarea.set_cursor_style(hidden);
                            body_textarea.set_cursor_style(visible);
                        }
                        body_state.insert_char('\n');
                        body_state.render_into(body_textarea, wrap_width);
                    }
                }
                // Enter on summary: submit the commit
                else if focus == popup::CommitInputFocus::Summary && key.code == KeyCode::Enter {
                    let popup = std::mem::replace(&mut self.popup, PopupState::None);
                    if let PopupState::CommitInput {
                        summary_textarea,
                        body_state,
                        on_confirm,
                        ..
                    } = popup
                    {
                        let summary = summary_textarea.lines().join("");
                        let body = body_state.raw().trim().to_string();
                        let text = if body.is_empty() {
                            summary
                        } else {
                            format!("{}\n\n{}", summary, body)
                        };
                        // Save to commit history
                        if !text.trim().is_empty() {
                            self.commit_message_history.retain(|m| m != &text);
                            self.commit_message_history.insert(0, text.clone());
                            self.commit_message_history.truncate(50);
                            self.save_commit_history();
                        }
                        self.commit_history_idx = None;
                        // Successful submit: drop any stashed in-progress editor.
                        self.saved_commit_popup = None;
                        if let Err(e) = on_confirm(self, &text) {
                            self.popup = PopupState::Message {
                                title: "Error".to_string(),
                                message: format!("{}", e),
                                kind: MessageKind::Error,
                            };
                        }
                    }
                }
                // Esc: stash editor so re-opening commit prompt restores in-progress text.
                else if key.code == KeyCode::Esc {
                    let stashed = std::mem::replace(&mut self.popup, PopupState::None);
                    self.saved_commit_popup = Some(stashed);
                    self.commit_history_idx = None;
                }
                // Open commit menu key (configurable)
                else if matches_key(
                    key,
                    &self
                        .config
                        .user_config
                        .keybinding
                        .commit_message
                        .commit_menu,
                ) {
                    self.show_commit_editor_menu()?;
                }
                // AI generate key (configurable)
                else if matches_key(
                    key,
                    &self
                        .config
                        .user_config
                        .keybinding
                        .commit_message
                        .ai_generate,
                ) {
                    self.trigger_ai_commit_generation_from_editor();
                }
                // Up/Down on summary: cycle commit history
                else if focus == popup::CommitInputFocus::Summary
                    && (key.code == KeyCode::Up || key.code == KeyCode::Down)
                    && !self.commit_message_history.is_empty()
                {
                    let wrap_width = self.commit_body_wrap_width();
                    if let PopupState::CommitInput {
                        summary_textarea,
                        body_textarea,
                        body_state,
                        ..
                    } = &mut self.popup
                    {
                        let history_len = self.commit_message_history.len();
                        let load_msg = |summary_textarea: &mut tui_textarea::TextArea<'static>,
                                        body_textarea: &mut tui_textarea::TextArea<'static>,
                                        body_state: &mut popup::BodySoftWrap,
                                        msg: &str| {
                            let (summary, body) = split_commit_message(msg);
                            let mut new_summary = popup::make_commit_summary_textarea();
                            new_summary.insert_str(&summary);
                            *summary_textarea = new_summary;
                            *body_textarea = popup::make_commit_body_textarea();
                            // History entries were committed with hard wraps — undo them so
                            // they don't read as paragraph breaks in the soft-wrapped editor.
                            body_state.set_text(popup::unwrap_commit_body(&body));
                            body_state.render_into(body_textarea, wrap_width);
                        };
                        match key.code {
                            KeyCode::Up => {
                                let new_idx = match self.commit_history_idx {
                                    None => {
                                        // Save current draft
                                        let s = summary_textarea.lines().join("");
                                        let b = body_state.raw().to_string();
                                        self.commit_history_draft = if b.trim().is_empty() {
                                            s
                                        } else {
                                            format!("{}\n\n{}", s, b)
                                        };
                                        0
                                    }
                                    Some(idx) => (idx + 1).min(history_len - 1),
                                };
                                self.commit_history_idx = Some(new_idx);
                                let msg = self.commit_message_history[new_idx].clone();
                                load_msg(summary_textarea, body_textarea, body_state, &msg);
                            }
                            KeyCode::Down => match self.commit_history_idx {
                                Some(0) => {
                                    self.commit_history_idx = None;
                                    let draft = self.commit_history_draft.clone();
                                    load_msg(summary_textarea, body_textarea, body_state, &draft);
                                }
                                Some(idx) => {
                                    let new_idx = idx - 1;
                                    self.commit_history_idx = Some(new_idx);
                                    let msg = self.commit_message_history[new_idx].clone();
                                    load_msg(summary_textarea, body_textarea, body_state, &msg);
                                }
                                None => {}
                            },
                            _ => {}
                        }
                    }
                }
                // All other keys: forward to the focused textarea
                else {
                    let wrap_width = self.commit_body_wrap_width();
                    if let PopupState::CommitInput {
                        summary_textarea,
                        body_textarea,
                        body_state,
                        focus,
                        ..
                    } = &mut self.popup
                    {
                        match focus {
                            popup::CommitInputFocus::Summary => {
                                textarea_input(summary_textarea, key);
                            }
                            popup::CommitInputFocus::Body => {
                                // Body is driven by body_state (the unwrapped source of truth);
                                // body_textarea is just a soft-wrapped projection of it. Translate
                                // each key into a body_state edit, then re-render.
                                let mut handled = true;
                                let alt = key.modifiers.contains(KeyModifiers::ALT);
                                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                                let cmd = has_command_modifier(key.modifiers);
                                match key.code {
                                    KeyCode::Char(c) if !ctrl && !alt && !cmd => {
                                        body_state.insert_char(c);
                                    }
                                    // Cmd+Backspace / Ctrl+U: delete to start of visual line.
                                    // Most macOS terminals (Zed, WezTerm, …) intercept Cmd and
                                    // never forward it to the app, so the readline shortcut is
                                    // the only one that works everywhere.
                                    KeyCode::Backspace if cmd => {
                                        body_state.delete_to_visual_line_start(wrap_width);
                                    }
                                    KeyCode::Char('u') if ctrl => {
                                        body_state.delete_to_visual_line_start(wrap_width);
                                    }
                                    // Opt+Backspace / Ctrl+W: delete previous word.
                                    KeyCode::Backspace if alt => body_state.delete_word_left(),
                                    KeyCode::Char('w') if ctrl => body_state.delete_word_left(),
                                    KeyCode::Backspace => body_state.backspace(),
                                    KeyCode::Delete => body_state.delete(),
                                    // Cmd+Left/Right and Ctrl+A/E: jump to start/end of visual
                                    // row. Same reason as Cmd+Backspace — Ctrl is the portable
                                    // binding.
                                    KeyCode::Left if cmd => {
                                        body_state.move_visual_line_start(wrap_width)
                                    }
                                    KeyCode::Right if cmd => {
                                        body_state.move_visual_line_end(wrap_width)
                                    }
                                    KeyCode::Char('a') if ctrl => {
                                        body_state.move_visual_line_start(wrap_width)
                                    }
                                    KeyCode::Char('e') if ctrl => {
                                        body_state.move_visual_line_end(wrap_width)
                                    }
                                    // Opt+Left/Right: jump by word (matches the new-branch input
                                    // and the rest of the readline-style world).
                                    KeyCode::Left if alt => body_state.move_word_left(),
                                    KeyCode::Right if alt => body_state.move_word_right(),
                                    KeyCode::Char('b') if alt => body_state.move_word_left(),
                                    KeyCode::Char('f') if alt => body_state.move_word_right(),
                                    KeyCode::Left => body_state.move_left(),
                                    KeyCode::Right => body_state.move_right(),
                                    KeyCode::Up => body_state.move_visual_up(wrap_width),
                                    KeyCode::Down => body_state.move_visual_down(wrap_width),
                                    KeyCode::Home => body_state.move_home(),
                                    KeyCode::End => body_state.move_end(),
                                    _ => handled = false,
                                }
                                if handled {
                                    body_state.render_into(body_textarea, wrap_width);
                                }
                            }
                        }
                    }
                }
            }
            PopupState::Checklist {
                items,
                selected: _selected,
                search,
                ..
            } => {
                use crossterm::event::KeyModifiers;
                // Ctrl+A: clear all checks
                if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    if let PopupState::Checklist { items, .. } = &mut self.popup {
                        for item in items.iter_mut() {
                            item.checked = false;
                        }
                    }
                    return Ok(());
                }
                let visible_count = items
                    .iter()
                    .filter(|it| {
                        search.is_empty()
                            || it.label.to_lowercase().contains(&search.to_lowercase())
                    })
                    .count();
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let PopupState::Checklist { selected, .. } = &mut self.popup {
                            if visible_count > 0 {
                                *selected = (*selected + 1).min(visible_count - 1);
                            }
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let PopupState::Checklist { selected, .. } = &mut self.popup {
                            *selected = selected.saturating_sub(1);
                        }
                    }
                    KeyCode::Char(' ') => {
                        // Toggle checked state on the visible item at `selected`
                        if let PopupState::Checklist {
                            items,
                            selected,
                            search,
                            ..
                        } = &mut self.popup
                        {
                            let visible_indices: Vec<usize> = items
                                .iter()
                                .enumerate()
                                .filter(|(_, it)| {
                                    search.is_empty()
                                        || it.label.to_lowercase().contains(&search.to_lowercase())
                                })
                                .map(|(i, _)| i)
                                .collect();
                            if let Some(&real_idx) = visible_indices.get(*selected) {
                                items[real_idx].checked = !items[real_idx].checked;
                            }
                        }
                    }
                    KeyCode::Enter => {
                        let popup = std::mem::replace(&mut self.popup, PopupState::None);
                        if let PopupState::Checklist {
                            items, on_confirm, ..
                        } = popup
                        {
                            let checked: Vec<String> = items
                                .into_iter()
                                .filter(|it| it.checked)
                                .map(|it| it.label)
                                .collect();
                            if let Err(e) = on_confirm(self, checked) {
                                self.popup = PopupState::Message {
                                    title: "Error".to_string(),
                                    message: format!("{}", e),
                                    kind: MessageKind::Error,
                                };
                            }
                        }
                    }
                    KeyCode::Esc => {
                        self.popup = PopupState::None;
                    }
                    KeyCode::Backspace => {
                        if let PopupState::Checklist {
                            search, selected, ..
                        } = &mut self.popup
                        {
                            search.pop();
                            *selected = 0;
                        }
                    }
                    KeyCode::Char(c) => {
                        // Type into search filter (but not j/k which are nav)
                        // j/k already handled above, this won't fire for them
                        if let PopupState::Checklist {
                            search, selected, ..
                        } = &mut self.popup
                        {
                            search.push(c);
                            *selected = 0;
                        }
                    }
                    _ => {}
                }
            }
            PopupState::Loading { .. } => {
                // Block all input while loading — user must wait
            }
            PopupState::Help { .. } => {}
            PopupState::RefPicker { .. } => {}
            PopupState::ThemePicker { .. } => {}
            PopupState::None => {}
        }

        // These are handled separately to avoid borrow conflicts.
        // Use else-if so that a handler that transitions to another popup
        // (e.g. Help → ThemePicker on Enter) does not also fire the new
        // popup's handler with the same key event.
        if was_help && matches!(self.popup, PopupState::Help { .. }) {
            self.handle_help_popup_key(key);
        } else if was_ref_picker && matches!(self.popup, PopupState::RefPicker { .. }) {
            self.handle_ref_picker_key(key)?;
        } else if was_theme_picker && matches!(self.popup, PopupState::ThemePicker { .. }) {
            self.handle_theme_picker_key(key);
        }

        Ok(())
    }

    fn handle_help_popup_key(&mut self, key: KeyEvent) {
        // Helper: compute display index for a given entry selection
        fn find_display_idx(sections: &[HelpSection], sel: usize, search_lower: &str) -> usize {
            let has_search = !search_lower.is_empty();
            let mut ei = 0usize;
            let mut di = 0usize;
            for section in sections {
                let mut section_has_visible = false;
                for entry in &section.entries {
                    let matches = !has_search
                        || entry.key.to_lowercase().contains(search_lower)
                        || entry.description.to_lowercase().contains(search_lower);
                    if matches {
                        if !section_has_visible {
                            section_has_visible = true;
                            di += 1; // header row
                        }
                        if ei == sel {
                            return di;
                        }
                        ei += 1;
                        di += 1;
                    }
                }
            }
            di
        }

        fn count_visible(sections: &[HelpSection], search_lower: &str) -> usize {
            let has_search = !search_lower.is_empty();
            sections
                .iter()
                .map(|s| {
                    if has_search {
                        s.entries
                            .iter()
                            .filter(|e| {
                                e.key.to_lowercase().contains(search_lower)
                                    || e.description.to_lowercase().contains(search_lower)
                            })
                            .count()
                    } else {
                        s.entries.len()
                    }
                })
                .sum()
        }

        let mut open_theme_picker = false;

        if let PopupState::Help {
            sections,
            selected,
            search_textarea,
            scroll_offset,
        } = &mut self.popup
        {
            use crossterm::event::KeyModifiers;
            let search = search_textarea.lines().join("");
            let search_lower = search.to_lowercase();

            // Estimate list viewport height from terminal
            let popup_height = (self.layout.height as usize).saturating_sub(4).min(50);
            let list_height = popup_height.saturating_sub(5); // borders + search + sep + hint

            match key.code {
                KeyCode::Esc | KeyCode::Char('?')
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    self.popup = PopupState::None;
                    return;
                }
                KeyCode::Enter => {
                    // Check if the selected entry is "Color theme..."
                    let has_search = !search_lower.is_empty();
                    let mut ei = 0usize;
                    let mut found_desc = String::new();
                    'outer: for section in sections.iter() {
                        for entry in &section.entries {
                            let vis = !has_search
                                || entry.key.to_lowercase().contains(&search_lower)
                                || entry.description.to_lowercase().contains(&search_lower);
                            if vis {
                                if ei == *selected {
                                    found_desc = entry.description.clone();
                                    break 'outer;
                                }
                                ei += 1;
                            }
                        }
                    }
                    if found_desc == "Color theme..." {
                        open_theme_picker = true;
                    }
                }
                KeyCode::Down => {
                    let total = count_visible(sections, &search_lower);
                    if total > 0 {
                        *selected = (*selected + 1).min(total.saturating_sub(1));
                    }
                    let sdi = find_display_idx(sections, *selected, &search_lower);
                    if sdi >= *scroll_offset + list_height {
                        *scroll_offset = sdi.saturating_sub(list_height - 1);
                    }
                }
                KeyCode::Up => {
                    *selected = selected.saturating_sub(1);
                    if *selected == 0 {
                        // First item: always scroll to top so the section header is visible
                        *scroll_offset = 0;
                    } else {
                        let sdi = find_display_idx(sections, *selected, &search_lower);
                        if sdi <= *scroll_offset {
                            // Scroll up to show the section header too when possible
                            *scroll_offset = sdi.saturating_sub(1);
                        }
                    }
                }
                _ => {
                    textarea_input(search_textarea, key);
                    let new_search = search_textarea.lines().join("");
                    if new_search != search {
                        *selected = 0;
                        *scroll_offset = 0;
                    }
                }
            }
        }

        if open_theme_picker {
            self.popup = PopupState::None;
            self.show_theme_picker();
        }
    }

    fn handle_ref_picker_key(&mut self, key: KeyEvent) -> Result<()> {
        use crate::gui::popup::ListPickerItem;

        if let PopupState::RefPicker { core, .. } = &mut self.popup {
            let search = core.search_textarea.lines().join("");
            let total = core.items.len();

            let h = self.layout.height as usize;
            let list_height = list_picker_visible_height(h);

            match key.code {
                KeyCode::Esc => {
                    self.popup = PopupState::None;
                    return Ok(());
                }
                KeyCode::Enter => {
                    let value = if let Some(item) = core.items.get(core.selected) {
                        item.value.clone()
                    } else if !search.trim().is_empty() {
                        search.trim().to_string()
                    } else {
                        return Ok(());
                    };
                    let popup = std::mem::replace(&mut self.popup, PopupState::None);
                    if let PopupState::RefPicker { on_confirm, .. } = popup {
                        if let Err(e) = on_confirm(self, &value) {
                            self.popup = PopupState::Message {
                                title: "Error".to_string(),
                                message: format!("{}", e),
                                kind: MessageKind::Error,
                            };
                        }
                    }
                    return Ok(());
                }
                KeyCode::Down => {
                    if total > 0 {
                        core.selected = (core.selected + 1).min(total.saturating_sub(1));
                    }
                    let sdi = list_picker_display_idx(&core.items, core.selected);
                    if sdi >= core.scroll_offset + list_height {
                        core.scroll_offset = sdi.saturating_sub(list_height - 1);
                    }
                }
                KeyCode::Up => {
                    core.selected = core.selected.saturating_sub(1);
                    if core.selected == 0 {
                        core.scroll_offset = 0;
                    } else {
                        let sdi = list_picker_display_idx(&core.items, core.selected);
                        if sdi <= core.scroll_offset {
                            core.scroll_offset = sdi.saturating_sub(1);
                        }
                    }
                }
                _ => {
                    textarea_input(&mut core.search_textarea, key);
                    let new_search = core.search_textarea.lines().join("");
                    if new_search != search {
                        // Remove any previous raw-ref item at index 0
                        if !core.items.is_empty() && core.items[0].category == "[ref]" {
                            core.items.remove(0);
                        }

                        let new_lower = new_search.to_lowercase();
                        if !new_lower.is_empty() {
                            core.items.insert(
                                0,
                                ListPickerItem {
                                    value: new_search.trim().to_string(),
                                    label: new_search.trim().to_string(),
                                    category: "[ref]".to_string(),
                                },
                            );

                            if let Some(idx) = core.items.iter().skip(1).position(|i| {
                                i.label.to_lowercase().contains(&new_lower)
                                    || i.value.to_lowercase().contains(&new_lower)
                            }) {
                                core.selected = idx + 1;
                            } else {
                                core.selected = 0;
                            }
                            let sdi = list_picker_display_idx(&core.items, core.selected);
                            core.scroll_offset = sdi.saturating_sub(list_height / 2);
                        } else {
                            core.selected = 0;
                            core.scroll_offset = 0;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_theme_picker_key(&mut self, key: KeyEvent) {
        if let PopupState::ThemePicker {
            core,
            original_theme_index,
        } = &mut self.popup
        {
            let total = core.items.len();
            let search = core.search_textarea.lines().join("");

            let h = self.layout.height as usize;
            let list_height = list_picker_visible_height(h);

            match key.code {
                KeyCode::Esc => {
                    self.current_theme_index = *original_theme_index;
                    self.popup = PopupState::None;
                    return;
                }
                KeyCode::Enter => {
                    let idx = core.selected;
                    self.popup = PopupState::None;
                    self.current_theme_index = idx;
                    if let Some(ct) = crate::config::COLOR_THEMES.get(idx) {
                        let mut state = self.config.app_state.clone();
                        state.color_theme = Some(ct.id.to_string());
                        let _ = state.save(&self.config.state_path);
                    }
                    return;
                }
                KeyCode::Down => {
                    if total > 0 {
                        core.selected = (core.selected + 1) % total;
                    }
                    self.current_theme_index = core.selected;
                    if core.selected >= core.scroll_offset + list_height {
                        core.scroll_offset = core.selected.saturating_sub(list_height - 1);
                    }
                    if core.selected == 0 {
                        core.scroll_offset = 0;
                    }
                }
                KeyCode::Up => {
                    if total > 0 {
                        core.selected = if core.selected == 0 {
                            total - 1
                        } else {
                            core.selected - 1
                        };
                    }
                    self.current_theme_index = core.selected;
                    if core.selected < core.scroll_offset {
                        core.scroll_offset = core.selected;
                    }
                    if core.selected == total - 1 {
                        core.scroll_offset = total.saturating_sub(list_height);
                    }
                }
                _ => {
                    // Search/filter — jump to matching theme
                    textarea_input(&mut core.search_textarea, key);
                    let new_search = core.search_textarea.lines().join("");
                    if new_search != search {
                        let new_lower = new_search.to_lowercase();
                        if !new_lower.is_empty() {
                            if let Some(idx) = core
                                .items
                                .iter()
                                .position(|i| i.label.to_lowercase().contains(&new_lower))
                            {
                                core.selected = idx;
                                self.current_theme_index = idx;
                                // Center the match in the viewport
                                core.scroll_offset = idx.saturating_sub(list_height / 2);
                            }
                        } else {
                            core.selected = *original_theme_index;
                            self.current_theme_index = *original_theme_index;
                            core.scroll_offset =
                                original_theme_index.saturating_sub(list_height / 2);
                        }
                    }
                }
            }
        }
    }

    fn show_theme_picker(&mut self) {
        use crate::gui::popup::{ListPickerCore, ListPickerItem, make_help_search_textarea};

        let original = self.current_theme_index;
        let items: Vec<ListPickerItem> = crate::config::COLOR_THEMES
            .iter()
            .map(|ct| ListPickerItem {
                value: ct.id.to_string(),
                label: ct.name.to_string(),
                category: String::new(),
            })
            .collect();

        self.popup = PopupState::ThemePicker {
            core: ListPickerCore {
                items,
                selected: original,
                search_textarea: make_help_search_textarea(),
                scroll_offset: 0,
            },
            original_theme_index: original,
        };
    }

    pub fn show_interactive_rebase_picker(&mut self) {
        use crate::gui::popup::{ListPickerCore, ListPickerItem, make_help_search_textarea};

        let model = self.model.lock().unwrap();
        let mut items = Vec::new();

        for branch in &model.branches {
            if branch.head {
                continue;
            }
            items.push(ListPickerItem {
                value: branch.name.clone(),
                label: branch.name.clone(),
                category: "Branches".to_string(),
            });
        }

        for remote in &model.remotes {
            for branch in &remote.branches {
                let full_name = format!("{}/{}", remote.name, branch.name);
                items.push(ListPickerItem {
                    value: full_name.clone(),
                    label: full_name,
                    category: "Remote Branches".to_string(),
                });
            }
        }

        for tag in &model.tags {
            items.push(ListPickerItem {
                value: tag.name.clone(),
                label: tag.name.clone(),
                category: "Tags".to_string(),
            });
        }

        for commit in model.commits.iter().skip(1) {
            items.push(ListPickerItem {
                value: commit.hash.clone(),
                label: format!("{} {}", commit.short_hash(), commit.name),
                category: "Commits".to_string(),
            });
        }

        drop(model);

        self.popup = PopupState::RefPicker {
            title: "Interactive rebase current branch onto".to_string(),
            core: ListPickerCore {
                items,
                selected: 0,
                search_textarea: make_help_search_textarea(),
                scroll_offset: 0,
            },
            on_confirm: Box::new(|gui, ref_name| {
                controller::branches::enter_interactive_rebase_onto(gui, ref_name)
            }),
        };
    }

    fn show_help(&mut self) {
        let kb = &self.config.user_config.keybinding;
        let active = self.context_mgr.active();

        // Universal keybindings
        let universal = HelpSection {
            title: "Universal".into(),
            entries: vec![
                HelpEntry {
                    key: kb.universal.quit.clone(),
                    description: "Quit".into(),
                },
                HelpEntry {
                    key: kb.universal.quit_alt1.clone(),
                    description: "Quit (alt)".into(),
                },
                HelpEntry {
                    key: kb.universal.return_key.clone(),
                    description: "Return / Cancel".into(),
                },
                HelpEntry {
                    key: kb.universal.toggle_panel.clone(),
                    description: "Next panel".into(),
                },
                HelpEntry {
                    key: kb.universal.toggle_panel_reverse.clone(),
                    description: "Previous panel".into(),
                },
                HelpEntry {
                    key: kb.universal.prev_item.clone(),
                    description: "Previous item".into(),
                },
                HelpEntry {
                    key: kb.universal.next_item.clone(),
                    description: "Next item".into(),
                },
                HelpEntry {
                    key: kb.universal.prev_page.clone(),
                    description: "Page up".into(),
                },
                HelpEntry {
                    key: kb.universal.next_page.clone(),
                    description: "Page down".into(),
                },
                HelpEntry {
                    key: kb.universal.goto_top.clone(),
                    description: "Go to top".into(),
                },
                HelpEntry {
                    key: kb.universal.goto_bottom.clone(),
                    description: "Go to bottom".into(),
                },
                HelpEntry {
                    key: kb.universal.prev_block.clone(),
                    description: "Previous panel".into(),
                },
                HelpEntry {
                    key: kb.universal.next_block.clone(),
                    description: "Next panel".into(),
                },
                HelpEntry {
                    key: kb.universal.start_search.clone(),
                    description: "Search".into(),
                },
                HelpEntry {
                    key: kb.universal.next_match.clone(),
                    description: "Next search match".into(),
                },
                HelpEntry {
                    key: kb.universal.prev_match.clone(),
                    description: "Previous search match".into(),
                },
                HelpEntry {
                    key: kb.universal.scroll_up_main_alt1.clone(),
                    description: "Scroll diff up".into(),
                },
                HelpEntry {
                    key: kb.universal.scroll_down_main_alt1.clone(),
                    description: "Scroll diff down".into(),
                },
                HelpEntry {
                    key: kb.universal.scroll_left.clone(),
                    description: "Scroll left".into(),
                },
                HelpEntry {
                    key: kb.universal.scroll_right.clone(),
                    description: "Scroll right".into(),
                },
                HelpEntry {
                    key: kb.universal.undo.clone(),
                    description: "Undo".into(),
                },
                HelpEntry {
                    key: kb.universal.redo.clone(),
                    description: "Redo".into(),
                },
                HelpEntry {
                    key: kb.universal.refresh.clone(),
                    description: "Refresh".into(),
                },
                HelpEntry {
                    key: kb.universal.push_files.clone(),
                    description: "Push".into(),
                },
                HelpEntry {
                    key: kb.universal.pull_files.clone(),
                    description: "Pull".into(),
                },
                HelpEntry {
                    key: kb.universal.next_screen_mode.clone(),
                    description: "Enlarge panel".into(),
                },
                HelpEntry {
                    key: kb.universal.prev_screen_mode.clone(),
                    description: "Shrink panel".into(),
                },
                HelpEntry {
                    key: kb.universal.create_rebase_options_menu.clone(),
                    description: "Rebase options".into(),
                },
                HelpEntry {
                    key: kb.universal.create_patch_options_menu.clone(),
                    description: "Patch options".into(),
                },
                HelpEntry {
                    key: "{/}".into(),
                    description: "Previous/next hunk".into(),
                },
                HelpEntry {
                    key: ";".into(),
                    description: "Toggle command log".into(),
                },
                HelpEntry {
                    key: "W".into(),
                    description: "Compare / Diff mode".into(),
                },
                HelpEntry {
                    key: "I".into(),
                    description: "Interactive rebase onto...".into(),
                },
                HelpEntry {
                    key: "1-5".into(),
                    description: "Jump to panel".into(),
                },
                HelpEntry {
                    key: "?".into(),
                    description: "Show this help".into(),
                },
                HelpEntry {
                    key: "▸".into(),
                    description: "Color theme...".into(),
                },
            ],
        };

        // Context-specific keybindings
        let context_section = match active {
            ContextId::Files => HelpSection {
                title: "Files".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "Toggle dir / Focus diff".into(),
                    },
                    HelpEntry {
                        key: "<space>".into(),
                        description: "Stage / Unstage".into(),
                    },
                    HelpEntry {
                        key: kb.files.commit_changes.clone(),
                        description: "Commit".into(),
                    },
                    HelpEntry {
                        key: kb.files.generate_ai_commit.clone(),
                        description: "Generate AI commit".into(),
                    },
                    HelpEntry {
                        key: kb.files.amend_last_commit.clone(),
                        description: "Amend last commit".into(),
                    },
                    HelpEntry {
                        key: kb.files.commit_changes_with_editor.clone(),
                        description: "Commit with editor".into(),
                    },
                    HelpEntry {
                        key: kb.files.toggle_staged_all.clone(),
                        description: "Toggle stage all".into(),
                    },
                    HelpEntry {
                        key: kb.files.stash_all_changes.clone(),
                        description: "Stash changes".into(),
                    },
                    HelpEntry {
                        key: kb.files.view_stash_options.clone(),
                        description: "Stash options".into(),
                    },
                    HelpEntry {
                        key: kb.files.toggle_tree_view.clone(),
                        description: "Toggle tree view".into(),
                    },
                    HelpEntry {
                        key: kb.files.fetch.clone(),
                        description: "Fetch".into(),
                    },
                    HelpEntry {
                        key: kb.files.ignore_file.clone(),
                        description: "Ignore file".into(),
                    },
                    HelpEntry {
                        key: "d".into(),
                        description: "Discard changes".into(),
                    },
                    HelpEntry {
                        key: kb.universal.edit.clone(),
                        description: "Open in editor".into(),
                    },
                    HelpEntry {
                        key: kb.universal.open_file.clone(),
                        description: "Open in default program".into(),
                    },
                    HelpEntry {
                        key: "y".into(),
                        description: "Copy to clipboard menu".into(),
                    },
                    HelpEntry {
                        key: "{/}".into(),
                        description: "Cycle prev/next revert block in diff".into(),
                    },
                    HelpEntry {
                        key: kb.universal.revert_block.clone(),
                        description: "Open hunk menu (revert selected block)".into(),
                    },
                    HelpEntry {
                        key: kb.universal.undo_revert_block.clone(),
                        description: "Undo last revert (session)".into(),
                    },
                ],
            },
            ContextId::Worktrees => HelpSection {
                title: "Worktrees".into(),
                entries: vec![
                    HelpEntry {
                        key: "<space>".into(),
                        description: "Switch to worktree".into(),
                    },
                    HelpEntry {
                        key: "n".into(),
                        description: "Create worktree".into(),
                    },
                    HelpEntry {
                        key: "d".into(),
                        description: "Remove worktree".into(),
                    },
                ],
            },
            ContextId::Submodules => HelpSection {
                title: "Submodules".into(),
                entries: vec![
                    HelpEntry {
                        key: "<space>".into(),
                        description: "Update submodule".into(),
                    },
                    HelpEntry {
                        key: "a".into(),
                        description: "Add submodule".into(),
                    },
                    HelpEntry {
                        key: "d".into(),
                        description: "Remove submodule".into(),
                    },
                    HelpEntry {
                        key: "e".into(),
                        description: "Enter submodule".into(),
                    },
                    HelpEntry {
                        key: "u".into(),
                        description: "Update all submodules".into(),
                    },
                    HelpEntry {
                        key: "i".into(),
                        description: "Init submodules".into(),
                    },
                ],
            },
            ContextId::Branches => HelpSection {
                title: "Branches".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "View branch commits".into(),
                    },
                    HelpEntry {
                        key: "<space>".into(),
                        description: "Checkout branch".into(),
                    },
                    HelpEntry {
                        key: "c".into(),
                        description: "Checkout ref".into(),
                    },
                    HelpEntry {
                        key: "-".into(),
                        description: "Checkout previous branch".into(),
                    },
                    HelpEntry {
                        key: "n".into(),
                        description: "New branch".into(),
                    },
                    HelpEntry {
                        key: "d".into(),
                        description: "Delete branch".into(),
                    },
                    HelpEntry {
                        key: kb.branches.merge_into_current_branch.clone(),
                        description: "Merge into current".into(),
                    },
                    HelpEntry {
                        key: kb.branches.rebase_branch.clone(),
                        description: "Rebase".into(),
                    },
                    HelpEntry {
                        key: kb.branches.rename_branch.clone(),
                        description: "Rename branch".into(),
                    },
                    HelpEntry {
                        key: kb.branches.fast_forward.clone(),
                        description: "Fast-forward".into(),
                    },
                    HelpEntry {
                        key: kb.branches.set_upstream.clone(),
                        description: "Set upstream".into(),
                    },
                    HelpEntry {
                        key: "y".into(),
                        description: "Copy to clipboard menu".into(),
                    },
                    HelpEntry {
                        key: kb.branches.create_pull_request.clone(),
                        description: "Open in browser menu".into(),
                    },
                ],
            },
            ContextId::BranchCommits | ContextId::BranchCommitFiles => HelpSection {
                title: "Branch Commits".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "View commit files".into(),
                    },
                    HelpEntry {
                        key: "<esc>".into(),
                        description: "Back to branches".into(),
                    },
                    HelpEntry {
                        key: ".".into(),
                        description: "Toggle commit details panel".into(),
                    },
                ],
            },
            ContextId::Commits => HelpSection {
                title: "Commits".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "View commit files".into(),
                    },
                    HelpEntry {
                        key: kb.commits.squash_down.clone(),
                        description: "Squash down".into(),
                    },
                    HelpEntry {
                        key: kb.commits.rename_commit.clone(),
                        description: "Reword commit".into(),
                    },
                    HelpEntry {
                        key: kb.commits.view_reset_options.clone(),
                        description: "Reset options".into(),
                    },
                    HelpEntry {
                        key: kb.commits.mark_commit_as_fixup.clone(),
                        description: "Fixup commit".into(),
                    },
                    HelpEntry {
                        key: kb.commits.create_fixup_commit.clone(),
                        description: "Create fixup commit".into(),
                    },
                    HelpEntry {
                        key: kb.commits.squash_above_commits.clone(),
                        description: "Apply fixup commits".into(),
                    },
                    HelpEntry {
                        key: kb.commits.move_up_commit.clone(),
                        description: "Move commit up".into(),
                    },
                    HelpEntry {
                        key: kb.commits.move_down_commit.clone(),
                        description: "Move commit down".into(),
                    },
                    HelpEntry {
                        key: kb.commits.amend_to_commit.clone(),
                        description: "Amend to commit".into(),
                    },
                    HelpEntry {
                        key: kb.commits.pick_commit.clone(),
                        description: "Pick / Drop commit".into(),
                    },
                    HelpEntry {
                        key: kb.commits.revert_commit.clone(),
                        description: "Revert commit".into(),
                    },
                    HelpEntry {
                        key: kb.commits.cherry_pick_copy.clone(),
                        description: "Cherry-pick copy".into(),
                    },
                    HelpEntry {
                        key: kb.commits.paste_commits.clone(),
                        description: "Paste commits".into(),
                    },
                    HelpEntry {
                        key: "v".into(),
                        description: "Toggle range select".into(),
                    },
                    HelpEntry {
                        key: kb.commits.tag_commit.clone(),
                        description: "Tag commit".into(),
                    },
                    HelpEntry {
                        key: kb.commits.checkout_commit.clone(),
                        description: "Checkout commit".into(),
                    },
                    HelpEntry {
                        key: kb.commits.view_bisect_options.clone(),
                        description: "Bisect options".into(),
                    },
                    HelpEntry {
                        key: "o".into(),
                        description: "Open in browser".into(),
                    },
                    HelpEntry {
                        key: "y".into(),
                        description: "Copy to clipboard menu".into(),
                    },
                    HelpEntry {
                        key: kb.commits.interactive_rebase.clone(),
                        description: "Interactive rebase".into(),
                    },
                    HelpEntry {
                        key: kb.commits.open_log_menu.clone(),
                        description: "Filter by branch".into(),
                    },
                    HelpEntry {
                        key: ".".into(),
                        description: "Toggle commit details panel".into(),
                    },
                ],
            },
            ContextId::CommitFiles => HelpSection {
                title: "Commit Files".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "Toggle dir / Focus diff".into(),
                    },
                    HelpEntry {
                        key: "<esc>".into(),
                        description: "Back to commits".into(),
                    },
                    HelpEntry {
                        key: kb.files.toggle_tree_view.clone(),
                        description: "Toggle tree view".into(),
                    },
                    HelpEntry {
                        key: "y".into(),
                        description: "Copy to clipboard menu".into(),
                    },
                    HelpEntry {
                        key: ".".into(),
                        description: "Toggle commit details panel".into(),
                    },
                ],
            },
            ContextId::Reflog => HelpSection {
                title: "Reflog".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "View commit files".into(),
                    },
                    HelpEntry {
                        key: kb.commits.checkout_commit.clone(),
                        description: "Checkout commit".into(),
                    },
                    HelpEntry {
                        key: kb.commits.view_reset_options.clone(),
                        description: "Reset options".into(),
                    },
                    HelpEntry {
                        key: kb.commits.cherry_pick_copy.clone(),
                        description: "Cherry-pick".into(),
                    },
                    HelpEntry {
                        key: "y".into(),
                        description: "Copy to clipboard menu".into(),
                    },
                    HelpEntry {
                        key: ".".into(),
                        description: "Toggle commit details panel".into(),
                    },
                ],
            },
            ContextId::Stash => HelpSection {
                title: "Stash".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "View stash files".into(),
                    },
                    HelpEntry {
                        key: "<space>".into(),
                        description: "Apply stash".into(),
                    },
                    HelpEntry {
                        key: kb.stash.pop_stash.clone(),
                        description: "Pop stash".into(),
                    },
                    HelpEntry {
                        key: kb.stash.rename_stash.clone(),
                        description: "Rename stash".into(),
                    },
                    HelpEntry {
                        key: "d".into(),
                        description: "Drop stash".into(),
                    },
                ],
            },
            ContextId::StashFiles => HelpSection {
                title: "Stash Files".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "Toggle dir / Focus diff".into(),
                    },
                    HelpEntry {
                        key: "<esc>".into(),
                        description: "Back to stash".into(),
                    },
                    HelpEntry {
                        key: kb.files.toggle_tree_view.clone(),
                        description: "Toggle tree view".into(),
                    },
                    HelpEntry {
                        key: "y".into(),
                        description: "Copy to clipboard menu".into(),
                    },
                ],
            },
            ContextId::Remotes => HelpSection {
                title: "Remotes".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "View remote branches".into(),
                    },
                    HelpEntry {
                        key: "f".into(),
                        description: "Fetch from remote".into(),
                    },
                    HelpEntry {
                        key: "n".into(),
                        description: "Add new remote".into(),
                    },
                    HelpEntry {
                        key: "d".into(),
                        description: "Delete remote".into(),
                    },
                    HelpEntry {
                        key: kb.universal.push_files.clone(),
                        description: "Push".into(),
                    },
                    HelpEntry {
                        key: kb.universal.pull_files.clone(),
                        description: "Pull".into(),
                    },
                ],
            },
            ContextId::RemoteBranches => HelpSection {
                title: "Remote Branches".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "View branch commits".into(),
                    },
                    HelpEntry {
                        key: "<space>".into(),
                        description: "Checkout as local branch".into(),
                    },
                    HelpEntry {
                        key: kb.branches.merge_into_current_branch.clone(),
                        description: "Merge into current".into(),
                    },
                    HelpEntry {
                        key: kb.branches.rebase_branch.clone(),
                        description: "Rebase".into(),
                    },
                    HelpEntry {
                        key: "d".into(),
                        description: "Delete remote branch".into(),
                    },
                    HelpEntry {
                        key: "<esc>".into(),
                        description: "Back to remotes".into(),
                    },
                ],
            },
            ContextId::Tags => HelpSection {
                title: "Tags".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "View tag commits".into(),
                    },
                    HelpEntry {
                        key: "n".into(),
                        description: "Create tag".into(),
                    },
                    HelpEntry {
                        key: "d".into(),
                        description: "Delete tag".into(),
                    },
                    HelpEntry {
                        key: "P".into(),
                        description: "Push tag".into(),
                    },
                    HelpEntry {
                        key: "g".into(),
                        description: "Reset options".into(),
                    },
                ],
            },
            ContextId::Status => HelpSection {
                title: "Status".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "Recent repos".into(),
                    },
                    HelpEntry {
                        key: "y".into(),
                        description: "Copy to clipboard menu".into(),
                    },
                    HelpEntry {
                        key: "o".into(),
                        description: "Open in browser menu".into(),
                    },
                ],
            },
            _ => HelpSection {
                title: "Navigation".into(),
                entries: vec![
                    HelpEntry {
                        key: "<enter>".into(),
                        description: "Select / Open".into(),
                    },
                    HelpEntry {
                        key: "<space>".into(),
                        description: "Toggle / Confirm".into(),
                    },
                ],
            },
        };

        let sections = vec![context_section, universal];

        self.popup = PopupState::Help {
            sections,
            selected: 0,
            search_textarea: popup::make_help_search_textarea(),
            scroll_offset: 0,
        };
    }

    fn show_diff_help(&mut self) {
        let diff_section = HelpSection {
            title: "Diff Viewer".into(),
            entries: vec![
                HelpEntry {
                    key: "j/k".into(),
                    description: "Scroll down / up".into(),
                },
                HelpEntry {
                    key: "h/l".into(),
                    description: "Scroll left / right".into(),
                },
                HelpEntry {
                    key: "{/}".into(),
                    description: "Cycle prev / next hunk (selects revert block in Files)".into(),
                },
                HelpEntry {
                    key: "[".into(),
                    description: "Toggle old-only view".into(),
                },
                HelpEntry {
                    key: "]".into(),
                    description: "Toggle new-only view".into(),
                },
                HelpEntry {
                    key: "v".into(),
                    description: "Toggle unified / side-by-side view".into(),
                },
                HelpEntry {
                    key: "z".into(),
                    description: "Toggle line wrap".into(),
                },
                HelpEntry {
                    key: "g/G".into(),
                    description: "Go to top / bottom".into(),
                },
                HelpEntry {
                    key: "PgUp/PgDn".into(),
                    description: "Page up / down".into(),
                },
                HelpEntry {
                    key: "/".into(),
                    description: "Search in diff".into(),
                },
                HelpEntry {
                    key: "n/N".into(),
                    description: "Next / previous search match".into(),
                },
                HelpEntry {
                    key: "<enter>".into(),
                    description: "Open hunk menu on selected block (Files)".into(),
                },
                HelpEntry {
                    key: "click 󰧛".into(),
                    description: "Click revert icon to revert that block".into(),
                },
                HelpEntry {
                    key: "u".into(),
                    description: if self.diff_view.revert_undo_stack.is_empty() {
                        "Undo last revert (nothing to undo)".into()
                    } else {
                        format!(
                            "Undo last revert ({}/{})",
                            self.diff_view.revert_undo_stack.len(),
                            self.diff_view.revert_undo_high_water,
                        )
                    },
                },
                HelpEntry {
                    key: "e".into(),
                    description: "Edit file at line".into(),
                },
                HelpEntry {
                    key: "o".into(),
                    description: "Open file in default program".into(),
                },
                HelpEntry {
                    key: "y".into(),
                    description: "Copy selected text".into(),
                },
                HelpEntry {
                    key: "c".into(),
                    description: "Create note on line".into(),
                },
                HelpEntry {
                    key: "t".into(),
                    description: "Toggle notes visibility".into(),
                },
                HelpEntry {
                    key: "n/N".into(),
                    description: "Cycle next / previous note".into(),
                },
                HelpEntry {
                    key: "e".into(),
                    description: "Edit selected note (user notes only)".into(),
                },
                HelpEntry {
                    key: "d".into(),
                    description: "Delete selected note".into(),
                },
                HelpEntry {
                    key: "S".into(),
                    description: "Send selected note to AI".into(),
                },
                HelpEntry {
                    key: "".into(),
                    description: "Click to add note on a line".into(),
                },
                HelpEntry {
                    key: "q".into(),
                    description: "Quit".into(),
                },
                HelpEntry {
                    key: "+/_".into(),
                    description: "Enlarge / shrink panel".into(),
                },
                HelpEntry {
                    key: ";".into(),
                    description: "Toggle command log".into(),
                },
                HelpEntry {
                    key: "1-5".into(),
                    description: "Jump to sidebar panel".into(),
                },
                HelpEntry {
                    key: "esc".into(),
                    description: "Return to sidebar".into(),
                },
                HelpEntry {
                    key: "?".into(),
                    description: "Show this help".into(),
                },
                HelpEntry {
                    key: "▸".into(),
                    description: "Color theme...".into(),
                },
            ],
        };

        self.popup = PopupState::Help {
            sections: vec![diff_section],
            selected: 0,
            search_textarea: popup::make_help_search_textarea(),
            scroll_offset: 0,
        };
    }

    fn show_rebase_options_menu(
        &mut self,
        is_rebasing: bool,
        is_merging: bool,
        is_cherry_picking: bool,
    ) -> Result<()> {
        let mut items = Vec::new();

        if is_rebasing {
            items.push(popup::MenuItem {
                label: "Continue rebase".to_string(),
                description: "git rebase --continue".to_string(),
                key: Some("c".to_string()),
                action: Some(Box::new(|gui| {
                    gui.git.continue_rebase()?;
                    gui.needs_refresh = true;
                    Ok(())
                })),
            });
            items.push(popup::MenuItem {
                label: "Abort rebase".to_string(),
                description: "git rebase --abort".to_string(),
                key: Some("a".to_string()),
                action: Some(Box::new(|gui| {
                    gui.git.abort_rebase()?;
                    gui.needs_refresh = true;
                    Ok(())
                })),
            });
            items.push(popup::MenuItem {
                label: "Skip this commit".to_string(),
                description: "git rebase --skip".to_string(),
                key: Some("s".to_string()),
                action: Some(Box::new(|gui| {
                    gui.git.rebase_skip()?;
                    gui.needs_refresh = true;
                    Ok(())
                })),
            });
        }

        if is_merging {
            items.push(popup::MenuItem {
                label: "Abort merge".to_string(),
                description: "git merge --abort".to_string(),
                key: Some("a".to_string()),
                action: Some(Box::new(|gui| {
                    gui.git.abort_merge()?;
                    gui.needs_refresh = true;
                    Ok(())
                })),
            });
        }

        if is_cherry_picking {
            items.push(popup::MenuItem {
                label: "Continue cherry-pick".to_string(),
                description: "git cherry-pick --continue".to_string(),
                key: Some("c".to_string()),
                action: Some(Box::new(|gui| {
                    gui.git.continue_cherry_pick()?;
                    gui.needs_refresh = true;
                    Ok(())
                })),
            });
            items.push(popup::MenuItem {
                label: "Abort cherry-pick".to_string(),
                description: "git cherry-pick --abort".to_string(),
                key: Some("a".to_string()),
                action: Some(Box::new(|gui| {
                    gui.git.abort_cherry_pick()?;
                    gui.cherry_pick_clipboard.clear();
                    gui.needs_refresh = true;
                    Ok(())
                })),
            });
            items.push(popup::MenuItem {
                label: "Skip this commit".to_string(),
                description: "git cherry-pick --skip".to_string(),
                key: Some("s".to_string()),
                action: Some(Box::new(|gui| {
                    gui.git.skip_cherry_pick()?;
                    gui.needs_refresh = true;
                    Ok(())
                })),
            });
        }

        self.popup = PopupState::Menu {
            title: "Rebase/Merge/Cherry-pick options".to_string(),
            items,
            selected: 0,
            loading_index: None,
        };
        Ok(())
    }

    /// Show the commit menu from within the commit message editor (<c-o>).
    fn show_commit_editor_menu(&mut self) -> Result<()> {
        // Stash the current commit editor popup
        let stashed = std::mem::replace(&mut self.popup, PopupState::None);
        self.pending_commit_popup = Some(stashed);

        let generate_cmd = self.config.user_config.git.commit.generate_command.clone();
        let has_generate = !generate_cmd.is_empty();

        let ai_label = if has_generate {
            format!("Generate w/ AI ({})", generate_cmd)
        } else {
            "Generate w/ AI (not configured)".to_string()
        };

        let mut items = vec![
            popup::MenuItem {
                label: "Open in editor".to_string(),
                description: String::new(),
                key: Some("e".to_string()),
                action: Some(Box::new(|gui| {
                    // Restore the stashed editor — user can continue typing
                    // TODO: full $EDITOR integration would suspend the TUI
                    if let Some(stashed) = gui.pending_commit_popup.take() {
                        gui.popup = stashed;
                    }
                    Ok(())
                })),
            },
            popup::MenuItem {
                label: "Add co-author".to_string(),
                description: String::new(),
                key: Some("c".to_string()),
                action: Some(Box::new(|gui| {
                    // Restore editor, then open a prompt for co-author
                    let stashed = gui.pending_commit_popup.take();
                    gui.popup = PopupState::Input {
                        title: "Co-author (Name <email>)".to_string(),
                        textarea: popup::make_textarea("Name <email@example.com>"),
                        on_confirm: Box::new(move |gui, coauthor| {
                            if let Some(mut editor) = stashed {
                                if !coauthor.is_empty() {
                                    // Append co-author trailer to the body
                                    if let PopupState::CommitInput {
                                        ref mut body_textarea,
                                        ref mut body_state,
                                        ..
                                    } = editor
                                    {
                                        // Move logical cursor to end before appending so the
                                        // trailer goes at the bottom no matter where the user
                                        // last clicked.
                                        body_state.cursor = body_state.raw().chars().count();
                                        body_state.insert_str(&format!(
                                            "\n\nCo-authored-by: {}",
                                            coauthor
                                        ));
                                        let wrap = gui.commit_body_wrap_width();
                                        body_state.render_into(body_textarea, wrap);
                                    }
                                }
                                gui.popup = editor;
                            }
                            Ok(())
                        }),
                        is_commit: false,
                        confirm_focused: false,
                    };
                    Ok(())
                })),
            },
            popup::MenuItem {
                label: "Paste commit message from clipboard".to_string(),
                description: String::new(),
                key: Some("p".to_string()),
                action: Some(Box::new(|gui| {
                    let clipboard_text = read_clipboard();
                    if let Some(mut editor) = gui.pending_commit_popup.take() {
                        if let Some(text) = clipboard_text {
                            if !text.is_empty() {
                                if let PopupState::CommitInput {
                                    ref mut summary_textarea,
                                    ref mut body_textarea,
                                    ref mut body_state,
                                    ..
                                } = editor
                                {
                                    // Split pasted text: first line → summary, rest → body
                                    let (summary, body) = match text.find('\n') {
                                        Some(idx) => {
                                            let s = text[..idx].to_string();
                                            let b = text[idx + 1..]
                                                .trim_start_matches('\n')
                                                .to_string();
                                            (s, b)
                                        }
                                        None => (text.clone(), String::new()),
                                    };
                                    summary_textarea.select_all();
                                    summary_textarea.cut();
                                    summary_textarea.insert_str(&summary);
                                    // Clipboard usually holds an existing commit message that
                                    // was hard-wrapped — unwrap before loading.
                                    body_state.set_text(popup::unwrap_commit_body(&body));
                                    let wrap = gui.commit_body_wrap_width();
                                    body_state.render_into(body_textarea, wrap);
                                }
                            }
                        }
                        gui.popup = editor;
                    }
                    Ok(())
                })),
            },
        ];

        items.push(popup::MenuItem {
            label: "Clear summary and description".to_string(),
            description: String::new(),
            key: Some("x".to_string()),
            action: Some(Box::new(|gui| {
                if let Some(mut editor) = gui.pending_commit_popup.take() {
                    if let PopupState::CommitInput {
                        ref mut summary_textarea,
                        ref mut body_textarea,
                        ref mut body_state,
                        ref mut focus,
                        ..
                    } = editor
                    {
                        summary_textarea.select_all();
                        summary_textarea.cut();
                        body_state.set_text(String::new());
                        let wrap = gui.commit_body_wrap_width();
                        body_state.render_into(body_textarea, wrap);
                        *focus = popup::CommitInputFocus::Summary;
                    }
                    gui.popup = editor;
                }
                Ok(())
            })),
        });

        if has_generate {
            items.push(popup::MenuItem {
                label: ai_label,
                description: String::new(),
                key: Some("g".to_string()),
                action: Some(Box::new(|gui| {
                    gui.begin_ai_commit_generation_ui();
                    Ok(())
                })),
            });
        } else {
            items.push(popup::MenuItem {
                label: ai_label,
                description: String::new(),
                key: Some("g".to_string()),
                action: None, // Disabled — no generateCommand configured
            });
        }

        self.popup = PopupState::Menu {
            title: "Commit menu".to_string(),
            items,
            selected: 0,
            loading_index: None,
        };
        Ok(())
    }

    fn show_recent_repos(&mut self) -> Result<()> {
        let recent = self.config.app_state.recent_repos.clone();
        if recent.is_empty() {
            return Ok(());
        }

        let items: Vec<popup::MenuItem> = recent
            .into_iter()
            .map(|path| {
                let display = path.clone();
                let p = path.clone();
                popup::MenuItem {
                    label: display,
                    description: String::new(),
                    key: None,
                    action: Some(Box::new(move |gui| {
                        // Switch to the selected repo
                        let new_git = crate::git::GitCommands::new(std::path::Path::new(&p))?;
                        let new_model = new_git.load_model()?;
                        gui.git = std::sync::Arc::new(new_git);
                        *gui.model.lock().unwrap() = new_model;
                        gui.needs_refresh = false;
                        gui.needs_diff_refresh = true;
                        gui.context_mgr = context::ContextManager::new();
                        gui.diff_view.reset_keep_prefs();
                        if gui.show_file_tree {
                            gui.update_file_tree_state();
                        }
                        Ok(())
                    })),
                }
            })
            .collect();

        self.popup = PopupState::Menu {
            title: "Recent repos".to_string(),
            items,
            selected: 0,
            loading_index: None,
        };
        Ok(())
    }

    fn undo(&mut self) -> Result<()> {
        // Get reflog entries
        let result = self
            .git
            .git_cmd()
            .args(&["reflog", "--format=%H", "-n", "20"])
            .run()?;
        if !result.success {
            return Ok(());
        }
        let entries: Vec<&str> = result.stdout.lines().collect();
        let next_idx = self.undo_reflog_idx + 1;
        if next_idx >= entries.len() {
            return Ok(()); // Nothing more to undo
        }

        let target_hash = entries[next_idx].to_string();
        let short = &target_hash[..7.min(target_hash.len())];

        self.popup = PopupState::Confirm {
            title: "Undo".to_string(),
            message: format!("Undo to reflog entry {}? ({})", next_idx, short),
            on_confirm: Box::new(move |gui| {
                gui.git.reset_to_commit(&target_hash, "--mixed")?;
                gui.undo_reflog_idx = next_idx;
                gui.needs_refresh = true;
                Ok(())
            }),
        };
        Ok(())
    }

    fn redo(&mut self) -> Result<()> {
        if self.undo_reflog_idx == 0 {
            return Ok(()); // Nothing to redo
        }

        let result = self
            .git
            .git_cmd()
            .args(&["reflog", "--format=%H", "-n", "20"])
            .run()?;
        if !result.success {
            return Ok(());
        }
        let entries: Vec<&str> = result.stdout.lines().collect();
        let prev_idx = self.undo_reflog_idx - 1;
        if prev_idx >= entries.len() {
            return Ok(());
        }

        let target_hash = entries[prev_idx].to_string();
        let short = &target_hash[..7.min(target_hash.len())];

        self.popup = PopupState::Confirm {
            title: "Redo".to_string(),
            message: format!("Redo to reflog entry {}? ({})", prev_idx, short),
            on_confirm: Box::new(move |gui| {
                gui.git.reset_to_commit(&target_hash, "--mixed")?;
                gui.undo_reflog_idx = prev_idx;
                gui.needs_refresh = true;
                Ok(())
            }),
        };
        Ok(())
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Result<()> {
        if let PopupState::None = self.popup {
            // Search uses a textarea — forward keys to it
            if let Some(ref mut ta) = self.search_textarea {
                match key.code {
                    KeyCode::Esc => {
                        self.search_active = false;
                        self.search_query.clear();
                        self.search_matches.clear();
                        self.search_match_idx = 0;
                        self.search_textarea = None;
                    }
                    KeyCode::Enter => {
                        self.search_active = false;
                        // Jump to first match
                        if !self.search_matches.is_empty() {
                            self.search_match_idx = 0;
                            let idx = self.search_matches[0];
                            self.context_mgr.set_selection(idx);
                        }
                        self.search_textarea = None;
                    }
                    _ => {
                        textarea_input(ta, key);
                        // Sync textarea content back to search_query
                        self.search_query = ta.lines().join("");
                        self.update_search_matches();
                    }
                }
            }
        }
        Ok(())
    }

    fn update_search_matches(&mut self) {
        self.search_matches.clear();
        if self.search_query.is_empty() {
            return;
        }

        let query = self.search_query.to_lowercase();
        let model = self.model.lock().unwrap();
        let active = self.context_mgr.active();

        match active {
            ContextId::Files => {
                if self.show_file_tree {
                    // When file tree is active, indices are into file_tree_nodes
                    for (i, node) in self.file_tree_nodes.iter().enumerate() {
                        if node.path.to_lowercase().contains(&query)
                            || node.name.to_lowercase().contains(&query)
                        {
                            self.search_matches.push(i);
                        }
                    }
                } else {
                    for (i, file) in model.files.iter().enumerate() {
                        if file.name.to_lowercase().contains(&query) {
                            self.search_matches.push(i);
                        }
                    }
                }
            }
            ContextId::Branches => {
                for (i, branch) in model.branches.iter().enumerate() {
                    if branch.name.to_lowercase().contains(&query) {
                        self.search_matches.push(i);
                    }
                }
            }
            ContextId::Commits => {
                for (i, commit) in model.commits.iter().enumerate() {
                    if commit.name.to_lowercase().contains(&query)
                        || commit.hash.starts_with(&self.search_query)
                        || commit.author_name.to_lowercase().contains(&query)
                    {
                        self.search_matches.push(i);
                    }
                }
            }
            ContextId::Reflog => {
                for (i, commit) in model.reflog_commits.iter().enumerate() {
                    if commit.name.to_lowercase().contains(&query)
                        || commit.hash.starts_with(&self.search_query)
                    {
                        self.search_matches.push(i);
                    }
                }
            }
            ContextId::Stash => {
                for (i, entry) in model.stash_entries.iter().enumerate() {
                    if entry.name.to_lowercase().contains(&query) {
                        self.search_matches.push(i);
                    }
                }
            }
            ContextId::Tags => {
                for (i, tag) in model.tags.iter().enumerate() {
                    if tag.name.to_lowercase().contains(&query) {
                        self.search_matches.push(i);
                    }
                }
            }
            ContextId::Remotes => {
                for (i, remote) in model.remotes.iter().enumerate() {
                    if remote.name.to_lowercase().contains(&query) {
                        self.search_matches.push(i);
                    }
                }
            }
            ContextId::RemoteBranches => {
                for (i, rb) in model.sub_remote_branches.iter().enumerate() {
                    if rb.name.to_lowercase().contains(&query) {
                        self.search_matches.push(i);
                    }
                }
            }
            ContextId::Worktrees => {
                for (i, wt) in model.worktrees.iter().enumerate() {
                    if wt.branch.to_lowercase().contains(&query)
                        || wt.path.to_lowercase().contains(&query)
                    {
                        self.search_matches.push(i);
                    }
                }
            }
            ContextId::Submodules => {
                for (i, sub) in model.submodules.iter().enumerate() {
                    if sub.name.to_lowercase().contains(&query)
                        || sub.path.to_lowercase().contains(&query)
                    {
                        self.search_matches.push(i);
                    }
                }
            }
            ContextId::CommitFiles | ContextId::StashFiles | ContextId::BranchCommitFiles => {
                if self.show_commit_file_tree {
                    for (i, node) in self.commit_file_tree_nodes.iter().enumerate() {
                        if node.path.to_lowercase().contains(&query)
                            || node.name.to_lowercase().contains(&query)
                        {
                            self.search_matches.push(i);
                        }
                    }
                } else {
                    for (i, file) in model.commit_files.iter().enumerate() {
                        if file.name.to_lowercase().contains(&query) {
                            self.search_matches.push(i);
                        }
                    }
                }
            }
            ContextId::BranchCommits => {
                for (i, commit) in model.sub_commits.iter().enumerate() {
                    if commit.name.to_lowercase().contains(&query)
                        || commit.hash.to_lowercase().contains(&query)
                        || commit.author_name.to_lowercase().contains(&query)
                    {
                        self.search_matches.push(i);
                    }
                }
            }
            _ => {}
        }

        // Auto-jump to first match
        if !self.search_matches.is_empty() {
            self.search_match_idx = 0;
            let idx = self.search_matches[0];
            self.context_mgr.set_selection(idx);
        }
    }

    fn goto_next_search_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = (self.search_match_idx + 1) % self.search_matches.len();
        let idx = self.search_matches[self.search_match_idx];
        self.context_mgr.set_selection(idx);
    }

    fn goto_prev_search_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = if self.search_match_idx == 0 {
            self.search_matches.len() - 1
        } else {
            self.search_match_idx - 1
        };
        let idx = self.search_matches[self.search_match_idx];
        self.context_mgr.set_selection(idx);
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

        if !self.config.user_config.gui.mouse_events {
            return;
        }

        // ✦ AI-generate button on commit-message popups: track hover, handle clicks.
        if matches!(self.popup, PopupState::CommitInput { .. }) {
            let area = ratatui::layout::Rect::new(0, 0, self.layout.width, self.layout.height);
            if let Some(btn_rect) = views::commit_ai_button_geometry(&self.popup, area) {
                let over = rect_contains(btn_rect, mouse.column, mouse.row);
                match mouse.kind {
                    MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                        if self.commit_ai_button_hovered != over {
                            self.commit_ai_button_hovered = over;
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) if over => {
                        self.commit_ai_button_hovered = false;
                        let configured = !self
                            .config
                            .user_config
                            .git
                            .commit
                            .generate_command
                            .trim()
                            .is_empty();
                        if configured {
                            self.trigger_ai_commit_generation_from_editor();
                        } else {
                            let url = "https://github.com/blankeos/lazygitrs#whats-different";
                            if let Err(e) = crate::os::platform::Platform::open_file(url) {
                                self.popup = PopupState::Message {
                                    title: "Error".to_string(),
                                    message: format!("Could not open browser: {}", e),
                                    kind: MessageKind::Error,
                                };
                            }
                        }
                        return;
                    }
                    _ => {}
                }
            } else if self.commit_ai_button_hovered {
                self.commit_ai_button_hovered = false;
            }
        } else if self.commit_ai_button_hovered {
            self.commit_ai_button_hovered = false;
        }

        if matches!(self.popup, PopupState::CommitInput { .. }) {
            let area = ratatui::layout::Rect::new(0, 0, self.layout.width, self.layout.height);
            if let Some(body_rect) = views::commit_description_textarea_geometry(&self.popup, area)
                && rect_contains(body_rect, mouse.column, mouse.row)
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                        let rows: i16 = if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                            3
                        } else {
                            -3
                        };
                        let wrap_width = self.commit_body_wrap_width();
                        if let PopupState::CommitInput {
                            body_textarea,
                            body_state,
                            ..
                        } = &mut self.popup
                        {
                            body_textarea.scroll((rows, 0));
                            let (row, col) = body_textarea.cursor();
                            body_state.set_cursor_from_visual(row, col, wrap_width);
                        }
                        return;
                    }
                    _ => {}
                }
            }
        }

        // Rebase mode: scroll and click support
        if self.rebase_mode.active {
            match mouse.kind {
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                    // Use the viewport height stored by the renderer so this
                    // matches what's actually on screen (including resizes).
                    let list_h = self.rebase_mode.visible_height;
                    // List length includes entries + the base commit row appended at the bottom.
                    let list_len = self.rebase_mode.entries.len() + 1;
                    let delta: isize = if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                        3
                    } else {
                        -3
                    };
                    scroll::scroll_viewport(&mut self.rebase_mode.scroll, delta, list_len, list_h);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    // Compute the list area to determine which entry was clicked
                    let area =
                        ratatui::layout::Rect::new(0, 0, self.layout.width, self.layout.height);
                    let outer = ratatui::layout::Layout::default()
                        .direction(ratatui::layout::Direction::Vertical)
                        .constraints([
                            ratatui::layout::Constraint::Min(1),
                            ratatui::layout::Constraint::Length(1),
                        ])
                        .split(area);
                    let block = ratatui::widgets::Block::default()
                        .borders(ratatui::widgets::Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded);
                    let inner = block.inner(outer[0]);
                    let has_banner =
                        self.rebase_mode.phase == modes::rebase_mode::RebasePhase::InProgress;
                    let banner_h: u16 = if has_banner { 2 } else { 0 };
                    // List starts after: inner.y + info_line(1) + banner_h
                    let list_y = inner.y + 1 + banner_h;
                    let list_h = inner.height.saturating_sub(1 + banner_h) as usize;
                    if mouse.row >= list_y && mouse.row < list_y + list_h as u16 {
                        let row_in_list = (mouse.row - list_y) as usize;
                        let clicked_idx = self.rebase_mode.scroll + row_in_list;
                        if clicked_idx < self.rebase_mode.entries.len() {
                            self.rebase_mode.selected = clicked_idx;
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // Diff mode has its own mouse handling
        if self.diff_mode.active {
            self.handle_diff_mode_mouse(mouse);
            return;
        }

        // Help popup intercepts mouse scroll and click
        if let PopupState::Help {
            sections,
            selected,
            scroll_offset,
            search_textarea,
        } = &mut self.popup
        {
            // Compute total display rows so we can clamp scroll
            let search_lower = search_textarea.lines().join("").to_lowercase();
            let has_search = !search_lower.is_empty();
            let total_rows: usize = sections
                .iter()
                .map(|s| {
                    let visible = if has_search {
                        s.entries
                            .iter()
                            .filter(|e| {
                                e.key.to_lowercase().contains(&search_lower)
                                    || e.description.to_lowercase().contains(&search_lower)
                            })
                            .count()
                    } else {
                        s.entries.len()
                    };
                    if visible > 0 { visible + 1 } else { 0 } // +1 for header
                })
                .sum();

            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    *scroll_offset = scroll_offset.saturating_sub(3);
                }
                MouseEventKind::ScrollDown => {
                    *scroll_offset = (*scroll_offset + 3).min(total_rows.saturating_sub(1));
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    // Click to select an entry in the help list
                    let area =
                        ratatui::layout::Rect::new(0, 0, self.layout.width, self.layout.height);
                    let popup_width = (area.width * 70 / 100).min(72).max(36);
                    let content_height = total_rows.max(1);
                    let popup_height = (content_height as u16 + 5)
                        .min(area.height.saturating_sub(4))
                        .max(10);
                    let x = (area.width.saturating_sub(popup_width)) / 2;
                    let y = (area.height.saturating_sub(popup_height)) / 2;
                    let inner_y = y + 1; // border
                    let list_start = inner_y + 2; // search + separator
                    let inner_height = popup_height.saturating_sub(2); // borders
                    let list_height = inner_height.saturating_sub(3) as usize; // search + sep + hint

                    if mouse.row >= list_start
                        && mouse.row < list_start + list_height as u16
                        && mouse.column >= x
                        && mouse.column < x + popup_width
                    {
                        let row_in_list = (mouse.row - list_start) as usize;
                        let display_idx = *scroll_offset + row_in_list;

                        // Build flat display list to map display_idx to entry index
                        let mut di = 0usize;
                        let mut ei = 0usize;
                        let mut clicked_entry = None;
                        'sections: for section in sections.iter() {
                            let visible_entries: Vec<_> = section
                                .entries
                                .iter()
                                .filter(|e| {
                                    !has_search
                                        || e.key.to_lowercase().contains(&search_lower)
                                        || e.description.to_lowercase().contains(&search_lower)
                                })
                                .collect();
                            if !visible_entries.is_empty() {
                                if di == display_idx {
                                    // Clicked on a header — ignore
                                    break;
                                }
                                di += 1; // header
                                for _ in visible_entries {
                                    if di == display_idx {
                                        clicked_entry = Some(ei);
                                        break 'sections;
                                    }
                                    di += 1;
                                    ei += 1;
                                }
                            }
                        }
                        if let Some(entry_idx) = clicked_entry {
                            *selected = entry_idx;
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // RefPicker popup intercepts mouse scroll and click
        if let PopupState::RefPicker { core, .. } = &mut self.popup {
            let total = core.items.len();
            let h = self.layout.height as usize;
            let lh = list_picker_visible_height(h);
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    core.selected = core.selected.saturating_sub(1);
                    if core.selected < core.scroll_offset {
                        core.scroll_offset = core.selected;
                    }
                }
                MouseEventKind::ScrollDown => {
                    core.selected = (core.selected + 1).min(total.saturating_sub(1));
                    let di = list_picker_display_idx(&core.items, core.selected);
                    if di >= core.scroll_offset + lh {
                        core.scroll_offset = di.saturating_sub(lh - 1);
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    // Click to select an item in the list picker
                    let area =
                        ratatui::layout::Rect::new(0, 0, self.layout.width, self.layout.height);
                    let popup_width = (area.width * 60 / 100).min(60).max(30);
                    let max_popup = (area.height * 60 / 100).max(10);
                    let popup_height = max_popup.min(area.height.saturating_sub(4));
                    let x = (area.width.saturating_sub(popup_width)) / 2;
                    let y = (area.height.saturating_sub(popup_height)) / 2;
                    let inner_y = y + 1;
                    let list_start = inner_y + 2;
                    let inner_height = popup_height.saturating_sub(2);
                    let list_height = inner_height.saturating_sub(3) as usize;

                    if mouse.row >= list_start
                        && mouse.row < list_start + list_height as u16
                        && mouse.column >= x
                        && mouse.column < x + popup_width
                    {
                        let row_in_list = (mouse.row - list_start) as usize;
                        // Map display row to entry index, accounting for category headers
                        let has_categories = core.items.iter().any(|i| !i.category.is_empty());
                        let effective_scroll = core.scroll_offset.min(if has_categories {
                            // display length includes headers
                            let display_len =
                                list_picker_display_idx(&core.items, total.saturating_sub(1)) + 1;
                            display_len.saturating_sub(list_height)
                        } else {
                            total.saturating_sub(list_height)
                        });
                        let display_idx = effective_scroll + row_in_list;

                        if has_categories {
                            // Walk through display rows to find which entry was clicked
                            let mut di = 0usize;
                            let mut ei = 0usize;
                            let mut last_cat = String::new();
                            for item in core.items.iter() {
                                if !item.category.is_empty() && item.category != last_cat {
                                    if di == display_idx {
                                        break; // clicked on header
                                    }
                                    di += 1;
                                    last_cat = item.category.clone();
                                }
                                if di == display_idx {
                                    core.selected = ei;
                                    break;
                                }
                                di += 1;
                                ei += 1;
                            }
                        } else {
                            let clicked_idx = effective_scroll + row_in_list;
                            if clicked_idx < total {
                                core.selected = clicked_idx;
                            }
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        // ThemePicker popup intercepts mouse scroll and click
        if let PopupState::ThemePicker { core, .. } = &mut self.popup {
            let total = core.items.len();
            let h = self.layout.height as usize;
            let lh = list_picker_visible_height(h);
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    core.selected = core.selected.saturating_sub(1);
                    self.current_theme_index = core.selected;
                    if core.selected < core.scroll_offset {
                        core.scroll_offset = core.selected;
                    }
                }
                MouseEventKind::ScrollDown => {
                    core.selected = (core.selected + 1).min(total.saturating_sub(1));
                    self.current_theme_index = core.selected;
                    if core.selected >= core.scroll_offset + lh {
                        core.scroll_offset = core.selected.saturating_sub(lh - 1);
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    // Click to select a theme
                    let area =
                        ratatui::layout::Rect::new(0, 0, self.layout.width, self.layout.height);
                    let popup_width = (area.width * 60 / 100).min(60).max(30);
                    let max_popup = (area.height * 60 / 100).max(10);
                    let popup_height = max_popup.min(area.height.saturating_sub(4));
                    let x = (area.width.saturating_sub(popup_width)) / 2;
                    let y = (area.height.saturating_sub(popup_height)) / 2;
                    let inner_y = y + 1;
                    let list_start = inner_y + 2;
                    let inner_height = popup_height.saturating_sub(2);
                    let list_height = inner_height.saturating_sub(3) as usize;

                    if mouse.row >= list_start
                        && mouse.row < list_start + list_height as u16
                        && mouse.column >= x
                        && mouse.column < x + popup_width
                    {
                        let row_in_list = (mouse.row - list_start) as usize;
                        let effective_scroll =
                            core.scroll_offset.min(total.saturating_sub(list_height));
                        let clicked_idx = effective_scroll + row_in_list;
                        if clicked_idx < total {
                            core.selected = clicked_idx;
                            self.current_theme_index = clicked_idx;
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        let fl = self.compute_current_frame_layout();
        if let Some(grab_rect) = fl.grab_column {
            let over_grab = mouse.column == grab_rect.x
                && mouse.row >= grab_rect.y
                && mouse.row < grab_rect.y + grab_rect.height;

            if self.grab_column_dragging.is_some() {
                self.grab_column_hovered = true;
                match mouse.kind {
                    MouseEventKind::Drag(MouseButton::Left) => {
                        let total_width = self.layout.width as f64;
                        if total_width > 0.0 {
                            let new_ratio = (mouse.column as f64) / total_width;
                            self.layout.side_panel_ratio = new_ratio.clamp(0.05, 0.95);
                        }
                        return;
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        self.grab_column_dragging = None;
                        self.grab_column_hovered = over_grab;
                        return;
                    }
                    _ => {}
                }
            }

            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) if over_grab => {
                    self.grab_column_dragging = Some(mouse.column);
                    self.grab_column_hovered = true;
                    return;
                }
                MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                    if self.grab_column_hovered != over_grab {
                        self.grab_column_hovered = over_grab;
                    }
                    if over_grab {
                        return;
                    }
                }
                _ => {}
            }
        } else {
            self.grab_column_hovered = false;
            self.grab_column_dragging = None;
        }

        let main_panel = self.compute_main_panel_rect();
        let pl = DiffPanelLayout::compute(main_panel, &self.diff_view);

        // Track mouse hover over the revert-block marker (for tooltip).
        if !self.diff_mode.active {
            let new_hover = self.revert_hunk_at_position(main_panel, &pl, mouse.column, mouse.row);
            if self.diff_view.hovered_revert_hunk != new_hover {
                self.diff_view.hovered_revert_hunk = new_hover;
            }
        }

        // Track line hover for inline notes `` button
        let new_line_hover = if rect_contains(main_panel, mouse.column, mouse.row) {
            if let Some(panel) = pl.panel_at_x(mouse.column) {
                if let Some((line_idx, _)) = self.diff_view.line_chunk_at_row(mouse.row, &pl) {
                    // Check if this row is actually the  button row of the
                    // previous line (after its notes). If so, keep hovering the
                    // previous line so the  stays visible.
                    if line_idx > 0 {
                        let content_width =
                            pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
                        let panel_width =
                            pl.old_content_end_x.saturating_sub(pl.old_content_x) as usize;
                        let right_content_width =
                            pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
                        let prev_idx = line_idx - 1;
                        if let Some(prev_dl) = self.diff_view.lines.get(prev_idx) {
                            if prev_dl.file_header.is_none() && !prev_dl.comment_notes.is_empty() {
                                // Compute prev line's code_y
                                let mut acc = 0usize;
                                for (offset, dl) in self.diff_view.lines
                                    [self.diff_view.scroll_offset..=prev_idx]
                                    .iter()
                                    .enumerate()
                                {
                                    let idx = self.diff_view.scroll_offset + offset;
                                    if idx == prev_idx {
                                        break;
                                    }
                                    if self.diff_view.view_layout
                                        == crate::pager::side_by_side::DiffViewLayout::Unified
                                        || self.diff_view.side_view
                                            != crate::pager::side_by_side::DiffSideView::Both
                                    {
                                        acc +=
                                            crate::pager::side_by_side::unified_line_visual_height(
                                                dl,
                                                content_width,
                                                &self.diff_view,
                                                idx,
                                            );
                                    } else {
                                        acc += crate::pager::side_by_side::line_visual_height(
                                            dl,
                                            panel_width,
                                            right_content_width,
                                            &self.diff_view,
                                            idx,
                                        );
                                    }
                                }
                                let plus_y = pl.inner_y
                                    + acc as u16
                                    + (prev_dl.comment_notes.len() * 5) as u16;
                                if mouse.row == plus_y {
                                    Some((prev_idx, panel))
                                } else {
                                    Some((line_idx, panel))
                                }
                            } else {
                                Some((line_idx, panel))
                            }
                        } else {
                            Some((line_idx, panel))
                        }
                    } else {
                        Some((line_idx, panel))
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        if self.diff_view.hovered_line != new_line_hover {
            self.diff_view.hovered_line = new_line_hover;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let in_main = main_panel.x <= mouse.column
                    && mouse.column < main_panel.x + main_panel.width
                    && main_panel.y <= mouse.row
                    && mouse.row < main_panel.y + main_panel.height;

                // In Full screen mode, the main_panel covers everything.
                // If the sidebar is focused (not diff_focused), clicks should
                // go to the sidebar handler, not start a diff selection.
                let full_sidebar = self.screen_mode == ScreenMode::Full && !self.diff_focused;

                if in_main && !self.diff_view.is_empty() && !full_sidebar {
                    if self.try_handle_revert_block_click(main_panel, pl, mouse.column, mouse.row) {
                        self.diff_focused = true;
                        return;
                    }

                    if let Some((line_idx, panel)) = self.diff_view.hovered_line {
                        // Compute the  button Y position: code row + note_count * 5
                        let content_width =
                            pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
                        let panel_width =
                            pl.old_content_end_x.saturating_sub(pl.old_content_x) as usize;
                        let right_content_width =
                            pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
                        let mut plus_acc = 0usize;
                        for (offset, dl) in self.diff_view.lines
                            [self.diff_view.scroll_offset..=line_idx]
                            .iter()
                            .enumerate()
                        {
                            let idx = self.diff_view.scroll_offset + offset;
                            if idx == line_idx {
                                break;
                            }
                            if self.diff_view.view_layout
                                == crate::pager::side_by_side::DiffViewLayout::Unified
                                || self.diff_view.side_view
                                    != crate::pager::side_by_side::DiffSideView::Both
                            {
                                plus_acc += crate::pager::side_by_side::unified_line_visual_height(
                                    dl,
                                    content_width,
                                    &self.diff_view,
                                    idx,
                                );
                            } else {
                                plus_acc += crate::pager::side_by_side::line_visual_height(
                                    dl,
                                    panel_width,
                                    right_content_width,
                                    &self.diff_view,
                                    idx,
                                );
                            }
                        }
                        let note_count = if self.diff_view.notes_visible {
                            self.diff_view
                                .lines
                                .get(line_idx)
                                .map(|dl| dl.comment_notes.len())
                                .unwrap_or(0)
                        } else {
                            0
                        };
                        let plus_y = pl.inner_y + plus_acc as u16 + (note_count * 5) as u16;

                        // Check if click was on `` button (hidden when notes are off)
                        let is_on_plus_x = match panel {
                            crate::pager::side_by_side::DiffPanel::Old => {
                                mouse.column >= pl.old_content_end_x.saturating_sub(3)
                                    && mouse.column < pl.old_content_end_x
                            }
                            crate::pager::side_by_side::DiffPanel::New => {
                                mouse.column >= pl.new_content_end_x.saturating_sub(3)
                                    && mouse.column < pl.new_content_end_x
                            }
                        };
                        if self.diff_view.notes_visible && is_on_plus_x && mouse.row == plus_y {
                            self.open_inline_note_editor(line_idx, panel, None);
                            return;
                        }

                        // Check if click was on a saved note box
                        if let Some(note_id) =
                            self.note_id_at_click(line_idx, panel, mouse.row, &pl)
                        {
                            // Compute which row within the note block was clicked
                            let mut acc = 0usize;
                            for (offset, dl) in self.diff_view.lines
                                [self.diff_view.scroll_offset..=line_idx]
                                .iter()
                                .enumerate()
                            {
                                let idx = self.diff_view.scroll_offset + offset;
                                if idx == line_idx {
                                    break;
                                }
                                if self.diff_view.view_layout
                                    == crate::pager::side_by_side::DiffViewLayout::Unified
                                    || self.diff_view.side_view
                                        != crate::pager::side_by_side::DiffSideView::Both
                                {
                                    acc += crate::pager::side_by_side::unified_line_visual_height(
                                        dl,
                                        content_width,
                                        &self.diff_view,
                                        idx,
                                    );
                                } else {
                                    acc += crate::pager::side_by_side::line_visual_height(
                                        dl,
                                        panel_width,
                                        right_content_width,
                                        &self.diff_view,
                                        idx,
                                    );
                                }
                            }
                            let note_start_y = pl.inner_y + acc as u16;
                            let click_off = mouse.row.saturating_sub(note_start_y);
                            let block_row = click_off % 5;

                            if block_row == 4 {
                                // Bottom border — [S] send / [e] edit / [d] del by X position
                                let panel_end_x = match panel {
                                    crate::pager::side_by_side::DiffPanel::Old => {
                                        pl.old_content_end_x
                                    }
                                    crate::pager::side_by_side::DiffPanel::New => {
                                        pl.new_content_end_x
                                    }
                                };
                                let del_start = panel_end_x.saturating_sub(9);

                                // Check note source to determine which buttons are shown
                                let is_user_note = self.diff_view.lines[line_idx]
                                    .comment_notes
                                    .iter()
                                    .any(|n| {
                                        n.id == note_id
                                            && n.source == crate::pager::NoteSource::User
                                    });
                                let has_send = self.diff_view.lines[line_idx]
                                    .comment_notes
                                    .iter()
                                    .any(|n| {
                                        n.id == note_id
                                            && n.source == crate::pager::NoteSource::User
                                            && n.status == crate::pager::NoteStatus::New
                                    });

                                if is_user_note {
                                    let edit_start = panel_end_x.saturating_sub(19);
                                    let send_start = panel_end_x.saturating_sub(29);
                                    if mouse.column >= del_start && mouse.column < panel_end_x {
                                        self.delete_note(note_id);
                                    } else if mouse.column >= edit_start && mouse.column < del_start
                                    {
                                        self.open_inline_note_editor(
                                            line_idx,
                                            panel,
                                            Some(note_id),
                                        );
                                    } else if has_send
                                        && mouse.column >= send_start
                                        && mouse.column < edit_start
                                    {
                                        self.notify_ai_for_note(note_id);
                                    } else {
                                        self.diff_view.selected_note = Some(note_id);
                                    }
                                } else {
                                    // AI notes: only [d] del is shown
                                    if mouse.column >= del_start && mouse.column < panel_end_x {
                                        self.delete_note(note_id);
                                    } else {
                                        self.diff_view.selected_note = Some(note_id);
                                    }
                                }
                            } else {
                                // Note body — just select, don't enter edit mode
                                self.diff_view.selected_note = Some(note_id);
                            }
                            self.diff_focused = true;
                            return;
                        }
                    }

                    if let Some(panel) = pl.panel_at_x(mouse.column) {
                        self.diff_view.selection = Some(TextSelection {
                            panel,
                            start_col: mouse.column,
                            start_row: mouse.row,
                            end_col: mouse.column,
                            end_row: mouse.row,
                            dragging: true,
                            is_click: false,
                            text: String::new(),
                            edit_line_number: None,
                            edit_column_number: None,
                        });
                    } else {
                        self.diff_view.selection = None;
                    }
                    self.diff_focused = true;
                } else {
                    // Click outside diff — clear selection and handle normally
                    self.diff_view.selection = None;
                    self.handle_mouse_click(mouse.column, mouse.row);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(ref mut sel) = self.diff_view.selection {
                    if sel.dragging {
                        let (cmin, cmax) = pl.content_range(sel.panel);
                        // Allow dragging into gutter area of same panel (5 cols before content)
                        let col_min = cmin.saturating_sub(5);
                        sel.end_col = mouse.column.max(col_min).min(cmax.saturating_sub(1));
                        sel.end_row = mouse
                            .row
                            .max(pl.inner_y)
                            .min(pl.inner_end_y.saturating_sub(1));
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Finalize the selection
                if let Some(ref mut sel) = self.diff_view.selection {
                    sel.dragging = false;
                    // If start == end (just a click, no drag)
                    if sel.start_col == sel.end_col && sel.start_row == sel.end_row {
                        if self.diff_view.file_exists_on_disk {
                            // Keep as click-state to show the edit tooltip
                            sel.is_click = true;
                        } else {
                            self.diff_view.selection = None;
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if self.is_in_commit_details_panel(mouse.column, mouse.row) {
                    self.commit_details_scroll = self.commit_details_scroll.saturating_sub(2);
                    return;
                }
                self.diff_view.selection = None;
                let in_diff = self.diff_focused
                    || (self.screen_mode != ScreenMode::Full
                        && self.is_in_main_panel(mouse.column, mouse.row));
                if mouse.modifiers.contains(KeyModifiers::SHIFT) && in_diff {
                    self.diff_view.scroll_left(4);
                } else if in_diff {
                    self.diff_view.scroll_up(3);
                } else {
                    // Viewport-only scroll: move scroll offset without changing selection
                    let active_ctx = self.context_mgr.active();
                    let model = self.model.lock().unwrap();
                    let list_len = self.context_mgr.list_len(&model);
                    drop(model);
                    let visible_height = self.sidebar_visible_height();
                    let mut offset = self.context_mgr.scroll_offset(active_ctx);
                    scroll::scroll_viewport(&mut offset, -3, list_len, visible_height);
                    self.context_mgr.set_scroll_offset(active_ctx, offset);
                    self.context_mgr.viewport_manually_scrolled = true;
                }
            }
            MouseEventKind::ScrollDown => {
                if self.is_in_commit_details_panel(mouse.column, mouse.row) {
                    self.commit_details_scroll = self.commit_details_scroll.saturating_add(2);
                    return;
                }
                self.diff_view.selection = None;
                let in_diff = self.diff_focused
                    || (self.screen_mode != ScreenMode::Full
                        && self.is_in_main_panel(mouse.column, mouse.row));
                if mouse.modifiers.contains(KeyModifiers::SHIFT) && in_diff {
                    self.diff_view.scroll_right(4);
                } else if in_diff {
                    self.diff_view.scroll_down(3);
                } else {
                    // Viewport-only scroll: move scroll offset without changing selection
                    let active_ctx = self.context_mgr.active();
                    let model = self.model.lock().unwrap();
                    let list_len = self.context_mgr.list_len(&model);
                    drop(model);
                    let visible_height = self.sidebar_visible_height();
                    let mut offset = self.context_mgr.scroll_offset(active_ctx);
                    scroll::scroll_viewport(&mut offset, 3, list_len, visible_height);
                    self.context_mgr.set_scroll_offset(active_ctx, offset);
                    self.context_mgr.viewport_manually_scrolled = true;
                }
            }
            MouseEventKind::ScrollLeft => {
                if self.is_in_commit_details_panel(mouse.column, mouse.row) {
                    return;
                }
                if self.diff_focused
                    || (self.screen_mode != ScreenMode::Full
                        && self.is_in_main_panel(mouse.column, mouse.row))
                {
                    self.diff_view.scroll_left(4);
                }
            }
            MouseEventKind::ScrollRight => {
                if self.is_in_commit_details_panel(mouse.column, mouse.row) {
                    return;
                }
                if self.diff_focused
                    || (self.screen_mode != ScreenMode::Full
                        && self.is_in_main_panel(mouse.column, mouse.row))
                {
                    self.diff_view.scroll_right(4);
                }
            }
            _ => {}
        }
    }

    fn handle_diff_mode_mouse(&mut self, mouse: MouseEvent) {
        use self::modes::diff_mode::{DiffModeFocus, DiffModeSelector};
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
        use ratatui::layout::{Constraint, Direction, Layout, Rect};

        // Help popup intercepts mouse scroll
        if let PopupState::Help {
            sections,
            scroll_offset,
            search_textarea,
            ..
        } = &mut self.popup
        {
            let search_lower = search_textarea.lines().join("").to_lowercase();
            let has_search = !search_lower.is_empty();
            let total_rows: usize = sections
                .iter()
                .map(|s| {
                    let visible = if has_search {
                        s.entries
                            .iter()
                            .filter(|e| {
                                e.key.to_lowercase().contains(&search_lower)
                                    || e.description.to_lowercase().contains(&search_lower)
                            })
                            .count()
                    } else {
                        s.entries.len()
                    };
                    if visible > 0 { visible + 1 } else { 0 }
                })
                .sum();

            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    *scroll_offset = scroll_offset.saturating_sub(3);
                }
                MouseEventKind::ScrollDown => {
                    *scroll_offset = (*scroll_offset + 3).min(total_rows.saturating_sub(1));
                }
                _ => {}
            }
            return;
        }

        // RefPicker popup intercepts mouse scroll and click
        if let PopupState::RefPicker { core, .. } = &mut self.popup {
            let total = core.items.len();
            let h = self.layout.height as usize;
            let lh = list_picker_visible_height(h);
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    core.selected = core.selected.saturating_sub(1);
                    if core.selected < core.scroll_offset {
                        core.scroll_offset = core.selected;
                    }
                }
                MouseEventKind::ScrollDown => {
                    core.selected = (core.selected + 1).min(total.saturating_sub(1));
                    let di = list_picker_display_idx(&core.items, core.selected);
                    if di >= core.scroll_offset + lh {
                        core.scroll_offset = di.saturating_sub(lh - 1);
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let area =
                        ratatui::layout::Rect::new(0, 0, self.layout.width, self.layout.height);
                    let popup_width = (area.width * 60 / 100).min(60).max(30);
                    let max_popup = (area.height * 60 / 100).max(10);
                    let popup_height = max_popup.min(area.height.saturating_sub(4));
                    let x = (area.width.saturating_sub(popup_width)) / 2;
                    let y = (area.height.saturating_sub(popup_height)) / 2;
                    let inner_y = y + 1;
                    let list_start = inner_y + 2;
                    let inner_height = popup_height.saturating_sub(2);
                    let list_height = inner_height.saturating_sub(3) as usize;

                    if mouse.row >= list_start
                        && mouse.row < list_start + list_height as u16
                        && mouse.column >= x
                        && mouse.column < x + popup_width
                    {
                        let row_in_list = (mouse.row - list_start) as usize;
                        let has_categories = core.items.iter().any(|i| !i.category.is_empty());
                        let effective_scroll = core.scroll_offset.min(if has_categories {
                            let display_len =
                                list_picker_display_idx(&core.items, total.saturating_sub(1)) + 1;
                            display_len.saturating_sub(list_height)
                        } else {
                            total.saturating_sub(list_height)
                        });
                        let display_idx = effective_scroll + row_in_list;

                        if has_categories {
                            let mut di = 0usize;
                            let mut ei = 0usize;
                            let mut last_cat = String::new();
                            for item in core.items.iter() {
                                if !item.category.is_empty() && item.category != last_cat {
                                    if di == display_idx {
                                        break;
                                    }
                                    di += 1;
                                    last_cat = item.category.clone();
                                }
                                if di == display_idx {
                                    core.selected = ei;
                                    break;
                                }
                                di += 1;
                                ei += 1;
                            }
                        } else {
                            let clicked_idx = effective_scroll + row_in_list;
                            if clicked_idx < total {
                                core.selected = clicked_idx;
                            }
                        }
                    }
                }
                _ => {}
            }
            return;
        }

        let area = Rect::new(0, 0, self.layout.width, self.layout.height);

        // Replicate the diff mode layout to determine regions
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);

        let content = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(33), Constraint::Percentage(67)])
            .split(outer[0]);

        let sidebar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(1),
            ])
            .split(content[0]);

        let selector_a_rect = sidebar[0];
        let selector_b_rect = sidebar[1];
        let files_rect = sidebar[2];
        let diff_rect = content[1];

        let col = mouse.column;
        let row = mouse.row;

        // Combobox dropdown mouse handling — intercepts clicks/scrolls when editing
        if self.diff_mode.editing.is_some() && !self.diff_mode.search_results.is_empty() {
            let anchor = if matches!(
                self.diff_mode.editing,
                Some(crate::gui::modes::diff_mode::DiffModeSelector::A)
            ) {
                selector_a_rect
            } else {
                selector_b_rect
            };
            let total = self.diff_mode.search_results.len();
            let max_items = 10usize.min(total);
            let dropdown_height = (max_items as u16) + 2;
            let available_height = area.height.saturating_sub(anchor.y + anchor.height);
            let dropdown_area = Rect {
                x: anchor.x,
                y: anchor.y + anchor.height,
                width: anchor.width,
                height: dropdown_height.min(available_height),
            };

            if rect_contains(dropdown_area, col, row) {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        // Click on a dropdown item — select it and confirm
                        let inner_y = row.saturating_sub(dropdown_area.y + 1); // +1 for top border
                        let clicked_idx = self.diff_mode.dropdown_scroll + inner_y as usize;
                        if clicked_idx < total {
                            self.diff_mode.search_selected = clicked_idx;
                            self.diff_mode.confirm_selection();
                            if self.diff_mode.has_both_refs() {
                                let _ = crate::gui::controller::diff_mode::reload_diff_files(self);
                                self.diff_mode.focus = DiffModeFocus::CommitFiles;
                            } else if self.diff_mode.ref_a.is_empty() {
                                self.diff_mode.focus = DiffModeFocus::SelectorA;
                                self.diff_mode.start_editing(DiffModeSelector::A);
                                let model = self.model.lock().unwrap();
                                self.diff_mode.search_refs(
                                    &model.branches,
                                    &model.tags,
                                    &model.commits,
                                    &model.remotes,
                                    &model.head_branch_name,
                                );
                            } else {
                                self.diff_mode.focus = DiffModeFocus::SelectorB;
                                self.diff_mode.start_editing(DiffModeSelector::B);
                                let model = self.model.lock().unwrap();
                                self.diff_mode.search_refs(
                                    &model.branches,
                                    &model.tags,
                                    &model.commits,
                                    &model.remotes,
                                    &model.head_branch_name,
                                );
                            }
                            self.needs_diff_refresh = true;
                        }
                        return;
                    }
                    MouseEventKind::ScrollUp => {
                        if self.diff_mode.search_selected > 0 {
                            self.diff_mode.search_selected =
                                self.diff_mode.search_selected.saturating_sub(3);
                            self.diff_mode.ensure_dropdown_visible(10);
                        }
                        return;
                    }
                    MouseEventKind::ScrollDown => {
                        let len = self.diff_mode.search_results.len();
                        if len > 0 {
                            self.diff_mode.search_selected =
                                (self.diff_mode.search_selected + 3).min(len - 1);
                            self.diff_mode.ensure_dropdown_visible(10);
                        }
                        return;
                    }
                    _ => {}
                }
            }
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Check if click is in the diff panel — start text selection
                if rect_contains(diff_rect, col, row) && !self.diff_view.is_empty() {
                    let pl = DiffPanelLayout::compute(diff_rect, &self.diff_view);
                    if self.try_handle_revert_block_click(diff_rect, pl, col, row) {
                        self.diff_mode.focus = DiffModeFocus::DiffExploration;
                        return;
                    }
                    if let Some(panel) = pl.panel_at_x(col) {
                        self.diff_view.selection = Some(TextSelection {
                            panel,
                            start_col: col,
                            start_row: row,
                            end_col: col,
                            end_row: row,
                            dragging: true,
                            is_click: false,
                            text: String::new(),
                            edit_line_number: None,
                            edit_column_number: None,
                        });
                    } else {
                        self.diff_view.selection = None;
                    }
                    self.diff_mode.focus = DiffModeFocus::DiffExploration;
                } else {
                    self.diff_view.selection = None;

                    // Click on panels to switch focus
                    if rect_contains(selector_a_rect, col, row) {
                        self.diff_mode.focus = DiffModeFocus::SelectorA;
                        // Start editing on click
                        self.diff_mode.start_editing(DiffModeSelector::A);
                        let model = self.model.lock().unwrap();
                        self.diff_mode.search_refs(
                            &model.branches,
                            &model.tags,
                            &model.commits,
                            &model.remotes,
                            &model.head_branch_name,
                        );
                    } else if rect_contains(selector_b_rect, col, row) {
                        self.diff_mode.focus = DiffModeFocus::SelectorB;
                        // Start editing on click
                        self.diff_mode.start_editing(DiffModeSelector::B);
                        let model = self.model.lock().unwrap();
                        self.diff_mode.search_refs(
                            &model.branches,
                            &model.tags,
                            &model.commits,
                            &model.remotes,
                            &model.head_branch_name,
                        );
                    } else if rect_contains(files_rect, col, row) {
                        self.diff_mode.focus = DiffModeFocus::CommitFiles;
                        // Click to select a file — use stored scroll offset
                        let inner_y = row.saturating_sub(files_rect.y + 1);
                        let len = self.diff_mode.visible_files_len();
                        let clicked_idx = self.diff_mode.diff_files_scroll + inner_y as usize;
                        if clicked_idx < len {
                            self.diff_mode.diff_files_selected = clicked_idx;
                            self.diff_mode.viewport_manually_scrolled = false;
                            self.needs_diff_refresh = true;
                        }
                    } else if rect_contains(diff_rect, col, row) {
                        self.diff_mode.focus = DiffModeFocus::DiffExploration;
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let pl = DiffPanelLayout::compute(diff_rect, &self.diff_view);
                if let Some(ref mut sel) = self.diff_view.selection {
                    if sel.dragging {
                        let (cmin, cmax) = pl.content_range(sel.panel);
                        let col_min = cmin.saturating_sub(5);
                        sel.end_col = col.max(col_min).min(cmax.saturating_sub(1));
                        sel.end_row = row.max(pl.inner_y).min(pl.inner_end_y.saturating_sub(1));
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(ref mut sel) = self.diff_view.selection {
                    sel.dragging = false;
                    if sel.start_col == sel.end_col && sel.start_row == sel.end_row {
                        if self.diff_view.file_exists_on_disk {
                            sel.is_click = true;
                        } else {
                            self.diff_view.selection = None;
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                if rect_contains(diff_rect, col, row) {
                    self.diff_view.selection = None;
                    if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                        self.diff_view.scroll_left(4);
                    } else {
                        self.diff_view.scroll_up(3);
                    }
                } else if rect_contains(files_rect, col, row) {
                    // Viewport-only scroll: move scroll offset without changing selection
                    let len = self.diff_mode.visible_files_len();
                    let visible_height = files_rect.height.saturating_sub(2) as usize;
                    scroll::scroll_viewport(
                        &mut self.diff_mode.diff_files_scroll,
                        -3,
                        len,
                        visible_height,
                    );
                    self.diff_mode.viewport_manually_scrolled = true;
                }
            }
            MouseEventKind::ScrollDown => {
                if rect_contains(diff_rect, col, row) {
                    self.diff_view.selection = None;
                    if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                        self.diff_view.scroll_right(4);
                    } else {
                        self.diff_view.scroll_down(3);
                    }
                } else if rect_contains(files_rect, col, row) {
                    // Viewport-only scroll: move scroll offset without changing selection
                    let len = self.diff_mode.visible_files_len();
                    let visible_height = files_rect.height.saturating_sub(2) as usize;
                    scroll::scroll_viewport(
                        &mut self.diff_mode.diff_files_scroll,
                        3,
                        len,
                        visible_height,
                    );
                    self.diff_mode.viewport_manually_scrolled = true;
                }
            }
            MouseEventKind::ScrollLeft => {
                if rect_contains(diff_rect, col, row) {
                    self.diff_view.scroll_left(4);
                }
            }
            MouseEventKind::ScrollRight => {
                if rect_contains(diff_rect, col, row) {
                    self.diff_view.scroll_right(4);
                }
            }
            _ => {}
        }
    }

    fn handle_mouse_click(&mut self, col: u16, row: u16) {
        if let PopupState::HunkCommentary { .. } = &self.popup {
            let popup_width = (self.layout.width * 60 / 100)
                .min(60)
                .max(30)
                .min(self.layout.width);
            let ta_height = 16u16.min(self.layout.height);
            if ta_height >= 3 && popup_width >= 3 {
                let ta_y = (self.layout.height.saturating_sub(ta_height)) / 2;
                let x = (self.layout.width.saturating_sub(popup_width)) / 2;
                if row == ta_y
                    && col >= x + popup_width.saturating_sub(5)
                    && col <= x + popup_width.saturating_sub(1)
                {
                    self.popup = PopupState::None;
                    return;
                }
            }
        }

        let fl = self.compute_current_frame_layout();

        // Commit details panel is non-focusable; swallow clicks that land there
        // so they don't leak into the diff view / sidebars.
        if let Some(details_rect) = fl.commit_details_panel
            && rect_contains(details_rect, col, row)
        {
            return;
        }

        // In Full screen mode with sidebar focused, the sidebar is rendered
        // in main_panel — treat clicks there as sidebar item selection.
        if self.screen_mode == ScreenMode::Full && !self.diff_focused {
            let panel_rect = fl.main_panel;
            if panel_rect.x <= col
                && col < panel_rect.x + panel_rect.width
                && panel_rect.y <= row
                && row < panel_rect.y + panel_rect.height
            {
                let inner_y = row.saturating_sub(panel_rect.y + 1);
                let active_ctx = self.context_mgr.active();
                let model = self.model.lock().unwrap();
                let list_len = self.context_mgr.list_len(&model);
                drop(model);
                let scroll_offset = self.context_mgr.scroll_offset(active_ctx);
                let clicked_idx = scroll_offset + inner_y as usize;
                if clicked_idx < list_len {
                    self.context_mgr.set_selection(clicked_idx);
                }
            }
            return;
        }

        // Check if click is in the main (diff) panel
        if fl.main_panel.x <= col
            && col < fl.main_panel.x + fl.main_panel.width
            && fl.main_panel.y <= row
            && row < fl.main_panel.y + fl.main_panel.height
        {
            if !self.diff_view.is_empty() {
                self.diff_focused = true;
            }
            return;
        }

        // Check which side panel was clicked
        for (i, &panel_rect) in fl.side_panels.iter().enumerate() {
            if panel_rect.x <= col
                && col < panel_rect.x + panel_rect.width
                && panel_rect.y <= row
                && row < panel_rect.y + panel_rect.height
            {
                self.diff_focused = false;
                if let Some(&window) = SideWindow::ALL.get(i) {
                    let is_title_bar = row == panel_rect.y;

                    if is_title_bar {
                        // Title bar click: switch to the clicked tab if identifiable.
                        let local_x = col.saturating_sub(panel_rect.x);
                        if let Some(tab_ctx) = window.tab_at_x(local_x) {
                            self.context_mgr.set_active(tab_ctx);
                        } else {
                            // Clicked title area but not on a specific tab label —
                            // just activate this window (restore last context).
                            let ctx = self.context_mgr.last_context_for_window(window);
                            self.context_mgr.set_active(ctx);
                        }
                    } else {
                        // Content area click.
                        let current_window = self.context_mgr.active_window();
                        if current_window != window {
                            // Switching to a different window — restore its last context.
                            let ctx = self.context_mgr.last_context_for_window(window);
                            self.context_mgr.set_active(ctx);
                        }
                        // Same window: don't call set_active, preserving any sub-view.

                        // Select the clicked item.
                        let inner_y = row.saturating_sub(panel_rect.y + 1); // +1 for border
                        let active_ctx = self.context_mgr.active();
                        let model = self.model.lock().unwrap();
                        let list_len = self.context_mgr.list_len(&model);
                        drop(model);

                        let scroll_offset = self.context_mgr.scroll_offset(active_ctx);
                        let clicked_idx = scroll_offset + inner_y as usize;
                        if clicked_idx < list_len {
                            self.context_mgr.set_selection(clicked_idx);
                        }
                    }
                }
                return;
            }
        }
    }

    fn is_in_main_panel(&self, col: u16, row: u16) -> bool {
        let mp = self.compute_main_panel_rect();
        col >= mp.x && col < mp.x + mp.width && row >= mp.y && row < mp.y + mp.height
    }

    /// True if mouse is over the (non-focusable) commit details panel.
    fn is_in_commit_details_panel(&self, col: u16, row: u16) -> bool {
        let fl = self.compute_current_frame_layout();
        fl.commit_details_panel
            .map(|r| rect_contains(r, col, row))
            .unwrap_or(false)
    }

    /// Compute the current frame layout using the same flags as views::render.
    /// This must match views.rs so mouse coords map to the rects actually drawn.
    fn compute_current_frame_layout(&self) -> layout::FrameLayout {
        let area = ratatui::layout::Rect::new(0, 0, self.layout.width, self.layout.height);
        let panel_count = SideWindow::ALL.len();
        let active_window = self.context_mgr.active_window();
        let active_panel_index = SideWindow::ALL
            .iter()
            .position(|w| *w == active_window)
            .unwrap_or(1);

        // Mirror views.rs: show_details when the active context is a commit
        // list (or drill-in commit files) with a valid selection.
        let show_details = self.details_panel_applies();

        layout::compute_layout_with_details(
            area,
            self.layout.side_panel_ratio,
            panel_count,
            active_panel_index,
            self.screen_mode,
            show_details,
            !self.diff_focused,
        )
    }

    /// True when the active context is one where commit-details makes sense
    /// (drives both the `.` toggle and layout-time `show_details`).
    fn context_has_commit_details(&self) -> bool {
        matches!(
            self.context_mgr.active(),
            ContextId::Commits
                | ContextId::BranchCommits
                | ContextId::Reflog
                | ContextId::CommitFiles
                | ContextId::BranchCommitFiles
                | ContextId::StashFiles
        )
    }

    fn details_panel_applies(&self) -> bool {
        if !self.show_commit_details {
            return false;
        }
        let ctx = self.context_mgr.active();
        let sel = self.context_mgr.selected(ctx);
        let model = self.model.lock().unwrap();
        match ctx {
            ContextId::Commits => sel < model.commits.len(),
            ContextId::BranchCommits => sel < model.sub_commits.len(),
            ContextId::Reflog => sel < model.reflog_commits.len(),
            ContextId::CommitFiles | ContextId::BranchCommitFiles | ContextId::StashFiles => {
                let hash = &self.commit_files_hash;
                !hash.is_empty()
                    && (model.commits.iter().any(|c| c.hash == *hash)
                        || model.sub_commits.iter().any(|c| c.hash == *hash)
                        || model.reflog_commits.iter().any(|c| c.hash == *hash))
            }
            _ => false,
        }
    }

    /// Compute the exact main panel Rect using the real layout engine.
    fn compute_main_panel_rect(&self) -> ratatui::layout::Rect {
        self.compute_current_frame_layout().main_panel
    }

    fn revert_hunk_at_position(
        &self,
        panel_rect: ratatui::layout::Rect,
        layout: &DiffPanelLayout,
        col: u16,
        row: u16,
    ) -> Option<usize> {
        if self.context_mgr.active() != ContextId::Files {
            return None;
        }
        if self.diff_view.is_empty() {
            return None;
        }
        if !rect_contains(panel_rect, col, row) {
            return None;
        }
        let divider_x = layout.divider_x()?;
        if col != divider_x && col != divider_x + 1 {
            return None;
        }
        let (line_idx, chunk_idx) = self.diff_view.line_chunk_at_row(row, layout)?;
        let diff_line = self.diff_view.lines.get(line_idx)?;
        let edit_active = self
            .diff_view
            .inline_edit
            .as_ref()
            .map(|e| e.line_idx == line_idx)
            .unwrap_or(false);
        let saved_note_count = if !self.diff_view.notes_visible {
            0
        } else if edit_active {
            let editing_id = self
                .diff_view
                .inline_edit
                .as_ref()
                .map(|e| e.editing_id.as_str())
                .unwrap_or("");
            diff_line
                .comment_notes
                .iter()
                .filter(|n| n.id != editing_id)
                .count()
        } else {
            diff_line.comment_notes.len()
        };
        let extra_rows = saved_note_count * 5 + if edit_active { 5 } else { 0 };

        if chunk_idx != extra_rows {
            return None;
        }
        self.diff_view.hunk_index_for_start_line(line_idx)
    }

    /// If a click lands on a saved note box for the given line, return its id.
    fn note_id_at_click(
        &self,
        line_idx: usize,
        panel: crate::pager::side_by_side::DiffPanel,
        row: u16,
        pl: &crate::pager::side_by_side::DiffPanelLayout,
    ) -> Option<String> {
        use crate::pager::side_by_side::{DiffPanel, DiffSideView, DiffViewLayout};

        let diff_line = self.diff_view.lines.get(line_idx)?;
        if diff_line.comment_notes.is_empty() {
            return None;
        }
        // Notes are not clickable when hidden
        if !self.diff_view.notes_visible {
            return None;
        }

        let panel_is_old = match panel {
            DiffPanel::Old => true,
            DiffPanel::New => false,
        };

        let content_width = pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
        let panel_width = pl.old_content_end_x.saturating_sub(pl.old_content_x) as usize;
        let right_content_width = pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;

        let mut acc = 0usize;
        for (offset, dl) in self.diff_view.lines[self.diff_view.scroll_offset..=line_idx]
            .iter()
            .enumerate()
        {
            let idx = self.diff_view.scroll_offset + offset;
            if idx == line_idx {
                break;
            }
            if self.diff_view.view_layout == DiffViewLayout::Unified
                || self.diff_view.side_view != DiffSideView::Both
            {
                acc += crate::pager::side_by_side::unified_line_visual_height(
                    dl,
                    content_width,
                    &self.diff_view,
                    idx,
                );
            } else {
                acc += crate::pager::side_by_side::line_visual_height(
                    dl,
                    panel_width,
                    right_content_width,
                    &self.diff_view,
                    idx,
                );
            }
        }

        let note_start_y = pl.inner_y + acc as u16;
        let click_off = row.saturating_sub(note_start_y);

        // Iterate ALL notes in render order; each takes 5 rows.
        // Return the one whose block the click falls within AND matches the panel.
        for (i, note) in diff_line.comment_notes.iter().enumerate() {
            let block_start = (i * 5) as u16;
            let block_end = block_start + 5;
            if click_off >= block_start && click_off < block_end && note.is_old == panel_is_old {
                return Some(note.id.clone());
            }
        }

        None
    }

    fn delete_note(&mut self, note_id: String) {
        let mut lines_file = crate::pager::notes_store::load(self.git.repo_path());
        let original_notes = lines_file.notes.clone();
        let deleted_note_opt = lines_file.notes.iter().find(|c| c.id == note_id).cloned();
        lines_file.notes.retain(|c| c.id != note_id);
        crate::pager::notes_store::save(self.git.repo_path(), lines_file.clone());
        self.diff_view.load_notes(self.git.repo_path());

        if let Some(deleted) = deleted_note_opt {
            let notes_on_same_line: Vec<&crate::pager::notes_store::LinesEntry> = original_notes
                .iter()
                .filter(|n| {
                    n.file == deleted.file && n.line == deleted.line && n.panel == deleted.panel
                })
                .collect();

            let mut found_next_on_same_line = None;
            if let Some(pos) = notes_on_same_line.iter().position(|n| n.id == deleted.id) {
                if pos + 1 < notes_on_same_line.len() {
                    found_next_on_same_line = Some(notes_on_same_line[pos + 1].id.clone());
                } else if pos > 0 {
                    found_next_on_same_line = Some(notes_on_same_line[pos - 1].id.clone());
                }
            }

            if found_next_on_same_line.is_some() {
                self.diff_view.selected_note = found_next_on_same_line;
                return;
            }

            let main_panel = self.compute_main_panel_rect();
            let pl = DiffPanelLayout::compute(main_panel, &self.diff_view);
            let content_width = pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
            let panel_width = pl.old_content_end_x.saturating_sub(pl.old_content_x) as usize;
            let right_content_width =
                pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
            let visible_height = pl.inner_end_y.saturating_sub(pl.inner_y) as usize;

            let start = self.diff_view.scroll_offset;
            let mut acc = 0usize;
            let mut visible_note_ids = HashSet::new();
            for idx in start..self.diff_view.lines.len() {
                if acc >= visible_height {
                    break;
                }
                let dl = &self.diff_view.lines[idx];
                for note in &dl.comment_notes {
                    visible_note_ids.insert(note.id.clone());
                }
                if self.diff_view.view_layout == crate::pager::side_by_side::DiffViewLayout::Unified
                    || self.diff_view.side_view != crate::pager::side_by_side::DiffSideView::Both
                {
                    acc += crate::pager::side_by_side::unified_line_visual_height(
                        dl,
                        content_width,
                        &self.diff_view,
                        idx,
                    );
                } else {
                    acc += crate::pager::side_by_side::line_visual_height(
                        dl,
                        panel_width,
                        right_content_width,
                        &self.diff_view,
                        idx,
                    );
                }
            }

            let candidate_notes: Vec<&crate::pager::notes_store::LinesEntry> = lines_file
                .notes
                .iter()
                .filter(|n| n.file == deleted.file)
                .collect();

            let visible_candidates: Vec<&crate::pager::notes_store::LinesEntry> = candidate_notes
                .iter()
                .filter(|n| visible_note_ids.contains(&n.id))
                .cloned()
                .collect();

            let subset = if !visible_candidates.is_empty() {
                visible_candidates
            } else {
                candidate_notes
            };

            let mut closest: Option<(&crate::pager::notes_store::LinesEntry, usize)> = None;
            for &note in &subset {
                let dist = (note.line as isize - deleted.line as isize).abs() as usize;
                match closest {
                    None => {
                        closest = Some((note, dist));
                    }
                    Some((prev_note, prev_dist)) => {
                        if dist < prev_dist {
                            closest = Some((note, dist));
                        } else if dist == prev_dist {
                            if note.panel == deleted.panel && prev_note.panel != deleted.panel {
                                closest = Some((note, dist));
                            }
                        }
                    }
                }
            }

            if let Some((note, _)) = closest {
                self.diff_view.selected_note = Some(note.id.clone());
            } else {
                self.diff_view.selected_note = None;
            }
        } else {
            self.diff_view.selected_note = None;
        }
    }

    fn ensure_note_visible(&mut self, note_id: &str) -> bool {
        let main_panel = self.compute_main_panel_rect();
        let pl = DiffPanelLayout::compute(main_panel, &self.diff_view);
        let content_width = pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
        let panel_width = pl.old_content_end_x.saturating_sub(pl.old_content_x) as usize;
        let right_content_width = pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
        let visible_height = pl.inner_end_y.saturating_sub(pl.inner_y) as usize;

        let start = self.diff_view.scroll_offset;
        let mut acc = 0usize;
        let mut visible_note_ids = HashSet::new();
        for idx in start..self.diff_view.lines.len() {
            if acc >= visible_height {
                break;
            }
            let dl = &self.diff_view.lines[idx];
            for note in &dl.comment_notes {
                visible_note_ids.insert(note.id.clone());
            }
            if self.diff_view.view_layout == crate::pager::side_by_side::DiffViewLayout::Unified
                || self.diff_view.side_view != crate::pager::side_by_side::DiffSideView::Both
            {
                acc += crate::pager::side_by_side::unified_line_visual_height(
                    dl,
                    content_width,
                    &self.diff_view,
                    idx,
                );
            } else {
                acc += crate::pager::side_by_side::line_visual_height(
                    dl,
                    panel_width,
                    right_content_width,
                    &self.diff_view,
                    idx,
                );
            }
        }

        if visible_note_ids.contains(note_id) {
            return true;
        }

        // Scroll to make it visible
        let mut target_line_idx = None;
        for (i, dl) in self.diff_view.lines.iter().enumerate() {
            if dl.comment_notes.iter().any(|n| n.id == note_id) {
                target_line_idx = Some(i);
                break;
            }
        }

        if let Some(line_idx) = target_line_idx {
            if line_idx < self.diff_view.scroll_offset {
                self.diff_view.scroll_offset = line_idx;
            }

            let start = self.diff_view.scroll_offset.min(line_idx);
            let mut acc = 0usize;
            for (offset, dl) in self.diff_view.lines[start..=line_idx].iter().enumerate() {
                let idx = start + offset;
                if idx == line_idx {
                    break;
                }
                if self.diff_view.view_layout == crate::pager::side_by_side::DiffViewLayout::Unified
                    || self.diff_view.side_view != crate::pager::side_by_side::DiffSideView::Both
                {
                    acc += crate::pager::side_by_side::unified_line_visual_height(
                        dl,
                        content_width,
                        &self.diff_view,
                        idx,
                    );
                } else {
                    acc += crate::pager::side_by_side::line_visual_height(
                        dl,
                        panel_width,
                        right_content_width,
                        &self.diff_view,
                        idx,
                    );
                }
            }

            if acc >= visible_height {
                self.diff_view.scroll_offset = line_idx.saturating_sub(visible_height / 3);
            }
        }

        false
    }

    /// Send a user note to the AI session.
    ///
    /// Delivery priority:
    /// 1. **TUI push** — if the AI CLI registered a `serverUrl` (e.g.
    ///    opencode's `http://127.0.0.1:4096`), call `POST /tui/append-prompt`
    ///    + `POST /tui/submit-prompt` to inject the prompt inline.
    /// 2. **SSE** — push an event to any connected SSE clients.
    /// 3. **Spawn** — fall back to `notifyCommand` subprocess.
    ///
    /// After sending, the note's `status` is updated from `New` → `Sent`
    /// in `.lines.json` so the TUI can show a visual indicator.
    fn notify_ai_for_note(&mut self, note_id: String) {
        let ai_config = self.config.user_config.ai_notes.clone();
        if !ai_config.enabled {
            self.show_error(
                "AI Notes Not Configured",
                anyhow::anyhow!(
                    "Set `aiNotes.enabled` to true in config.yml to enable sending notes to an AI session."
                ),
            );
            return;
        }

        // Find the note and its file/line context.
        let mut note_info: Option<(String, usize, String)> = None;
        for (i, dl) in self.diff_view.lines.iter().enumerate() {
            if let Some(note) = dl.comment_notes.iter().find(|n| n.id == note_id) {
                let file = self.diff_view.file_at_line(i).to_string();
                let panel = if note.is_old {
                    crate::pager::side_by_side::DiffPanel::Old
                } else {
                    crate::pager::side_by_side::DiffPanel::New
                };
                let line = self.diff_view.file_line_number(i, panel).unwrap_or(0);
                note_info = Some((file, line, note.text.clone()));
                break;
            }
        }

        let (file, line, note_text) = match note_info {
            Some(info) => info,
            None => return,
        };

        // Build the prompt that tells the AI to check the notes endpoint.
        let prompt = format!(
            "A review note was created in lazygitrs on file '{}' at line {}:\n\n\"{}\"\n\n\
             Load the lazygitrs-review skill if available. \
             Fetch all notes with: PORT=$(cat .lazygitrs.port) && curl -s http://127.0.0.1:$PORT/session-api/notes\n\
             Review the note above and respond by POSTing your annotations to \
             http://127.0.0.1:$PORT/session-api using the AgentContext JSON format:\n\
             {{\"version\":1,\"files\":[{{\"path\":\"...\",\"annotations\":[{{\"summary\":\"...\",\"rationale\":\"...\",\"newRange\":[L,L]}}]}}]}}",
            file, line, note_text
        );

        // 1. Try TUI push (opencode /tui/append-prompt + /tui/submit-prompt).
        let server_url = self
            .acp_server_url
            .lock()
            .map(|g| g.clone())
            .unwrap_or(None)
            .or_else(|| {
                crate::pager::notes_store::load(self.git.repo_path())
                    .session
                    .filter(|s| !s.server_url.is_empty())
                    .map(|s| s.server_url)
            });

        if let Some(ref url) = server_url {
            if self.push_prompt_to_tui(url, &prompt) {
                self.mark_note_sent(&note_id);
                return;
            }
        }

        // 2. Try SSE — if there are connected listeners, push the event.
        let sse_payload = serde_json::json!({
            "type": "note-sent",
            "noteId": note_id,
            "file": file,
            "line": line,
            "note": note_text,
            "prompt": prompt,
        })
        .to_string();

        let sse_delivered =
            self.sse_tx.receiver_count() > 0 && self.sse_tx.send(sse_payload).is_ok();

        // Update note status → Sent.
        self.mark_note_sent(&note_id);

        if sse_delivered {
            return;
        }

        // 3. Fallback: spawn the notifyCommand if configured by the active session.
        let notify_command = self
            .acp_notify_command
            .lock()
            .map(|g| g.clone())
            .unwrap_or(None)
            .or_else(|| {
                crate::pager::notes_store::load(self.git.repo_path())
                    .session
                    .filter(|s| !s.notify_command.is_empty())
                    .map(|s| s.notify_command)
            })
            .or_else(|| {
                if let Ok(home) = std::env::var("HOME") {
                    let global_path =
                        std::path::PathBuf::from(home).join(".lazygitrs_active_session.json");
                    if let Ok(data) = std::fs::read_to_string(&global_path) {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                            if let Some(cmd) = json.get("notifyCommand").and_then(|v| v.as_str()) {
                                if !cmd.is_empty() {
                                    return Some(cmd.to_string());
                                }
                            }
                        }
                    }
                }
                None
            });

        let cmd_template = match notify_command {
            Some(cmd) if !cmd.is_empty() => cmd,
            _ => return,
        };

        // Expand the command template.
        let session_id = self
            .acp_session_id
            .lock()
            .map(|g| g.clone())
            .unwrap_or(None)
            .or_else(|| {
                crate::pager::notes_store::load(self.git.repo_path())
                    .session
                    .map(|s| s.session_id)
            })
            .or_else(|| {
                if let Ok(home) = std::env::var("HOME") {
                    let global_path =
                        std::path::PathBuf::from(home).join(".lazygitrs_active_session.json");
                    if let Ok(data) = std::fs::read_to_string(&global_path) {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&data) {
                            if let Some(sid) = json.get("sessionId").and_then(|v| v.as_str()) {
                                if !sid.is_empty() {
                                    return Some(sid.to_string());
                                }
                            }
                        }
                    }
                }
                None
            })
            .unwrap_or_default();
        let workspace_path = self.git.repo_path().to_string_lossy().to_string();
        let cmd_str = cmd_template
            .replace("{{session_id}}", &session_id)
            .replace("{{workspace_path}}", &shell_escape_arg(&workspace_path))
            .replace("{{prompt}}", &shell_escape_arg(&prompt));

        crate::os::cmd::log_command(&cmd_str);
        let log_path = self.git.repo_path().join(".lazygitrs-ai-notify.log");
        match std::fs::File::create(&log_path) {
            Ok(log_file) => {
                let stderr_file = log_file.try_clone().unwrap_or_else(|_| {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open("/dev/null")
                        .unwrap()
                });
                match std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd_str)
                    .stdout(std::process::Stdio::from(log_file))
                    .stderr(std::process::Stdio::from(stderr_file))
                    .spawn()
                {
                    Ok(_) => {}
                    Err(e) => {
                        self.show_error(
                            "AI Notes Error",
                            anyhow::anyhow!("Failed to spawn AI CLI: {}", e),
                        );
                    }
                }
            }
            Err(_) => {
                match std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd_str)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(_) => {}
                    Err(e) => {
                        self.show_error(
                            "AI Notes Error",
                            anyhow::anyhow!("Failed to spawn AI CLI: {}", e),
                        );
                    }
                }
            }
        }
    }

    /// Push a prompt to the AI CLI's TUI via its HTTP server.
    /// Returns `true` on success.
    fn push_prompt_to_tui(&self, server_url: &str, prompt: &str) -> bool {
        let append_url = format!("{}/tui/append-prompt", server_url);
        let submit_url = format!("{}/tui/submit-prompt", server_url);

        let append_body = serde_json::json!({"text": prompt}).to_string();

        // Append the prompt text.
        let append_resp = std::process::Command::new("curl")
            .args([
                "-s",
                "-X",
                "POST",
                &append_url,
                "-H",
                "content-type: application/json",
                "--data",
                &append_body,
                "--max-time",
                "3",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        if !matches!(append_resp, Ok(s) if s.success()) {
            return false;
        }

        // Submit the prompt.
        let submit_resp = std::process::Command::new("curl")
            .args(["-s", "-X", "POST", &submit_url, "--max-time", "3"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        matches!(submit_resp, Ok(s) if s.success())
    }

    /// Mark a note as `Sent` in `.lines.json` and reload the diff view.
    fn mark_note_sent(&mut self, note_id: &str) {
        let mut lines_file = crate::pager::notes_store::load(self.git.repo_path());
        if let Some(entry) = lines_file.notes.iter_mut().find(|c| c.id == note_id) {
            entry.status = crate::pager::NoteStatus::Sent;
        }
        crate::pager::notes_store::save(self.git.repo_path(), lines_file);
        self.diff_view.load_notes(self.git.repo_path());
    }

    fn cycle_note(&mut self, forward: bool) {
        // Collect all notes in render order: (line_idx, note_id)
        let mut all_notes: Vec<(usize, String)> = Vec::new();
        for (i, dl) in self.diff_view.lines.iter().enumerate() {
            for n in &dl.comment_notes {
                all_notes.push((i, n.id.clone()));
            }
        }
        if all_notes.is_empty() {
            return;
        }

        let current = self.diff_view.selected_note.as_deref();
        let next = if let Some(cur) = current {
            let pos = all_notes.iter().position(|(_, id)| id == cur);
            if forward {
                pos.and_then(|p| {
                    let np = (p + 1) % all_notes.len();
                    all_notes.get(np).cloned()
                })
            } else {
                pos.and_then(|p| {
                    let np = if p == 0 { all_notes.len() - 1 } else { p - 1 };
                    all_notes.get(np).cloned()
                })
            }
        } else if forward {
            Some(all_notes[0].clone())
        } else {
            Some(all_notes.last().cloned().unwrap())
        };

        if let Some((line_idx, note_id)) = next {
            // If the target note is above the current scroll offset,
            // scroll up to it first to avoid an invalid slice range.
            if line_idx < self.diff_view.scroll_offset {
                self.diff_view.scroll_offset = line_idx;
            }

            // Scroll to make the note visible.
            let main_panel = self.compute_main_panel_rect();
            let pl = DiffPanelLayout::compute(main_panel, &self.diff_view);
            let content_width = pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
            let panel_width = pl.old_content_end_x.saturating_sub(pl.old_content_x) as usize;
            let right_content_width =
                pl.new_content_end_x.saturating_sub(pl.new_content_x) as usize;
            let visible_height = pl.inner_end_y.saturating_sub(pl.inner_y) as usize;

            let start = self.diff_view.scroll_offset.min(line_idx);
            let mut acc = 0usize;
            for (offset, dl) in self.diff_view.lines[start..=line_idx].iter().enumerate() {
                let idx = start + offset;
                if idx == line_idx {
                    break;
                }
                if self.diff_view.view_layout == crate::pager::side_by_side::DiffViewLayout::Unified
                    || self.diff_view.side_view != crate::pager::side_by_side::DiffSideView::Both
                {
                    acc += crate::pager::side_by_side::unified_line_visual_height(
                        dl,
                        content_width,
                        &self.diff_view,
                        idx,
                    );
                } else {
                    acc += crate::pager::side_by_side::line_visual_height(
                        dl,
                        panel_width,
                        right_content_width,
                        &self.diff_view,
                        idx,
                    );
                }
            }

            if acc >= visible_height {
                // Note is below viewport — scroll up so it becomes visible.
                self.diff_view.scroll_offset = line_idx.saturating_sub(visible_height / 3);
            }

            self.diff_view.selected_note = Some(note_id);
            self.diff_focused = true;
        }
    }

    fn open_inline_note_editor(
        &mut self,
        line_idx: usize,
        panel: crate::pager::side_by_side::DiffPanel,
        editing_id: Option<String>,
    ) {
        let file_path = self.diff_view.file_at_line(line_idx).to_string();
        let line_num = self
            .diff_view
            .file_line_number(line_idx, panel)
            .unwrap_or(0);

        let title = format!("Draft Note - {} - {}", file_path, line_num);
        let mut textarea = crate::gui::popup::make_textarea(&title);
        textarea.set_cursor_line_style(ratatui::style::Style::default());

        let editing_id = editing_id.unwrap_or_default();

        // Load existing note text if editing.
        if !editing_id.is_empty() {
            let target_path = self.git.repo_path().join(".lines.json");
            if target_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&target_path) {
                    let existing: Vec<serde_json::Value> =
                        serde_json::from_str(&content).unwrap_or_default();
                    if let Some(note) = existing
                        .iter()
                        .find(|c| c["id"].as_str() == Some(&editing_id))
                        .and_then(|c| c["comment"].as_str())
                    {
                        textarea.insert_str(note);
                    }
                }
            }
        }

        self.diff_view.inline_edit = Some(crate::pager::side_by_side::InlineEdit {
            line_idx,
            panel,
            textarea,
            editing_id,
        });
        self.diff_focused = true;
    }

    fn try_handle_revert_block_click(
        &mut self,
        panel_rect: ratatui::layout::Rect,
        layout: DiffPanelLayout,
        col: u16,
        row: u16,
    ) -> bool {
        if self.diff_mode.active {
            return false;
        }
        let Some(hunk_idx) = self.revert_hunk_at_position(panel_rect, &layout, col, row) else {
            return false;
        };
        self.diff_view.selected_revert_hunk = Some(hunk_idx);
        if let Err(err) = self.revert_selected_file_hunk(hunk_idx) {
            self.popup = PopupState::Message {
                title: "Revert block failed".to_string(),
                message: format!("{}", err),
                kind: MessageKind::Error,
            };
        }
        true
    }

    /// Open the hunk action menu (shown when Enter is pressed on a selected
    /// or hovered revert hunk). Cancel is focused first so an accidental
    /// Enter doesn't revert anything.
    fn show_hunk_context_menu(&mut self, hunk_idx: usize) {
        let items = vec![
            popup::MenuItem {
                label: "Cancel".to_string(),
                description: String::new(),
                key: None,
                // No-op: execute_menu_action already drops the menu popup
                // before invoking the action, so returning Ok leaves the
                // menu closed. Esc also closes the menu via the universal
                // menu Esc handler.
                action: Some(Box::new(|_gui| Ok(()))),
            },
            popup::MenuItem {
                label: "Revert hunk".to_string(),
                description: String::new(),
                key: None,
                action: Some(Box::new(move |gui| {
                    if let Err(err) = gui.revert_selected_file_hunk(hunk_idx) {
                        gui.popup = PopupState::Message {
                            title: "Revert block failed".to_string(),
                            message: format!("{}", err),
                            kind: MessageKind::Error,
                        };
                    }
                    Ok(())
                })),
            },
        ];

        self.popup = PopupState::Menu {
            title: "Hunk".to_string(),
            items,
            selected: 0,
            loading_index: None,
        };
    }

    fn revert_selected_file_hunk(&mut self, hunk_idx: usize) -> Result<()> {
        let Some(file_idx) = self.selected_file_index() else {
            return Ok(());
        };

        let model = self.model.lock().unwrap();
        let Some(file) = model.files.get(file_idx) else {
            return Ok(());
        };

        if !file.has_unstaged_changes {
            self.popup = PopupState::Message {
                title: "Revert block".to_string(),
                message: "Block revert is available only for unstaged changes.".to_string(),
                kind: MessageKind::Info,
            };
            return Ok(());
        }

        let file_name = file.name.clone();
        drop(model);

        let Some((want_old, want_new)) = self.diff_view.visual_block_line_ranges(hunk_idx) else {
            return Ok(());
        };
        if want_old.is_none() && want_new.is_none() {
            return Ok(());
        }

        let diff = self.git.diff_file(&file_name)?;
        if diff.is_empty() {
            return Ok(());
        }

        // Snapshot the working-tree file before reverting so the user can undo
        // (`u`) within this session. Only keep the snapshot if the revert
        // actually succeeds; otherwise we'd leak unrelated state into the stack.
        let abs_path = self.git.repo_path().join(&file_name);
        let pre_bytes = std::fs::read(&abs_path).ok();

        self.git
            .revert_visual_block_in_worktree(&file_name, &diff, want_old, want_new)?;

        if let Some(bytes) = pre_bytes {
            let stack = &mut self.diff_view.revert_undo_stack;
            if stack.len() >= crate::pager::side_by_side::REVERT_UNDO_STACK_CAP {
                stack.remove(0);
            }
            stack.push(crate::pager::side_by_side::RevertUndoEntry {
                file_path: file_name.clone(),
                pre_revert_bytes: bytes,
            });
            self.diff_view.revert_undo_high_water =
                self.diff_view.revert_undo_high_water.max(stack.len());
        }

        self.diff_view.selection = None;
        self.needs_files_refresh = true;
        self.needs_diff_refresh = true;
        Ok(())
    }

    fn undo_last_revert_block(&mut self) -> Result<()> {
        let Some(entry) = self.diff_view.revert_undo_stack.pop() else {
            return Ok(());
        };
        let abs_path = self.git.repo_path().join(&entry.file_path);
        std::fs::write(&abs_path, &entry.pre_revert_bytes)
            .with_context(|| format!("failed to restore {}", entry.file_path))?;
        if self.diff_view.revert_undo_stack.is_empty() {
            self.diff_view.revert_undo_high_water = 0;
        }
        self.needs_files_refresh = true;
        self.needs_diff_refresh = true;
        Ok(())
    }

    /// Approximate visible height of the active sidebar panel (inner area minus borders).
    fn sidebar_visible_height(&self) -> usize {
        let fl = self.compute_current_frame_layout();
        let active_window = self.context_mgr.active_window();
        let active_panel_index = SideWindow::ALL
            .iter()
            .position(|w| *w == active_window)
            .unwrap_or(1);
        // In Full screen mode with sidebar focused, the list is rendered in main_panel
        let panel_rect = if self.screen_mode == ScreenMode::Full && !self.diff_focused {
            fl.main_panel
        } else {
            fl.side_panels
                .get(active_panel_index)
                .copied()
                .unwrap_or(fl.main_panel)
        };
        // Subtract 2 for top/bottom borders
        panel_rect.height.saturating_sub(2) as usize
    }

    pub(crate) fn sync_rebase_progress_view(&mut self) -> bool {
        let was_active_in_progress =
            self.rebase_mode.active && self.rebase_mode.phase == RebasePhase::InProgress;
        let previous_current_hash = if was_active_in_progress {
            self.rebase_mode
                .entries
                .iter()
                .find(|entry| entry.status == EntryStatus::Current)
                .map(|entry| entry.hash.clone())
        } else {
            None
        };
        let previous_selected_hash = if was_active_in_progress {
            self.rebase_mode
                .entries
                .get(self.rebase_mode.selected)
                .map(|entry| entry.hash.clone())
        } else {
            None
        };
        let previous_scroll = self.rebase_mode.scroll;

        let Some(mut progress) = self.git.parse_rebase_progress() else {
            return false;
        };
        self.git.hydrate_progress(&mut progress);
        self.rebase_mode.enter_in_progress(&progress);

        let current_hash = self
            .rebase_mode
            .entries
            .iter()
            .find(|entry| entry.status == EntryStatus::Current)
            .map(|entry| entry.hash.clone());

        if was_active_in_progress
            && previous_current_hash.is_some()
            && previous_current_hash == current_hash
        {
            if let Some(selected_hash) = previous_selected_hash {
                if let Some(selected) = self
                    .rebase_mode
                    .entries
                    .iter()
                    .position(|entry| entry.hash == selected_hash)
                {
                    self.rebase_mode.selected = selected;
                    let list_len = self.rebase_mode.entries.len() + 1;
                    let max_scroll = list_len.saturating_sub(self.rebase_mode.visible_height);
                    self.rebase_mode.scroll = previous_scroll.min(max_scroll);
                    self.rebase_mode
                        .ensure_visible(self.rebase_mode.visible_height);
                }
            }
        }

        true
    }

    fn refresh(&mut self) -> Result<()> {
        self.reset_commit_pagination();
        let new_model = self.git.load_model()?;
        let mut model = self.model.lock().unwrap();
        model.replace_keeping_file_order(new_model);

        // If branch filters are active, reload commits for those branches only.
        if !self.commit_branch_filter.is_empty() {
            if let Ok(filtered) = self
                .git
                .load_commits_for_branches(&self.commit_branch_filter, DEFAULT_COMMIT_LIMIT)
            {
                model.commits = filtered;
            }
        }
        self.commit_history_complete = model.commits.len() < DEFAULT_COMMIT_LIMIT;

        // Rebuild file tree inline to avoid borrow issues
        if self.show_file_tree {
            self.file_tree_nodes = build_file_tree(&model.files, &self.collapsed_dirs);
            self.context_mgr.files_list_len_override = Some(self.file_tree_nodes.len());
        } else {
            self.file_tree_nodes.clear();
            self.context_mgr.files_list_len_override = None;
        }

        // If we're viewing branch commits, re-load them (refresh wipes the model)
        if (self.context_mgr.active() == ContextId::BranchCommits
            || self.context_mgr.active() == ContextId::BranchCommitFiles)
            && !self.branch_commits_name.is_empty()
        {
            if let Ok(commits) = self
                .git
                .load_commits_for_branch(&self.branch_commits_name, 300)
            {
                model.sub_commits = commits;
            }
        }

        // If we're viewing remote branches (or drilled into commits/files from them), re-load them
        if !self.remote_branches_name.is_empty()
            && (self.context_mgr.active() == ContextId::RemoteBranches
                || ((self.context_mgr.active() == ContextId::BranchCommits
                    || self.context_mgr.active() == ContextId::BranchCommitFiles)
                    && self.sub_commits_parent_context == ContextId::RemoteBranches))
        {
            if let Some(remote) = model
                .remotes
                .iter()
                .find(|r| r.name == self.remote_branches_name)
            {
                model.sub_remote_branches = remote.branches.clone();
            }
        }

        // If we're viewing commit/stash files, re-load them (refresh wipes the model)
        if (self.context_mgr.active() == ContextId::CommitFiles
            || self.context_mgr.active() == ContextId::StashFiles
            || self.context_mgr.active() == ContextId::BranchCommitFiles)
            && !self.commit_files_hash.is_empty()
        {
            if let Ok(cf) = self.git.commit_files(&self.commit_files_hash) {
                model.commit_files = cf;
            }
            if self.show_commit_file_tree {
                self.commit_file_tree_nodes = crate::model::file_tree::build_commit_file_tree(
                    &model.commit_files,
                    &self.commit_files_collapsed_dirs,
                );
                self.context_mgr.commit_files_list_len_override =
                    Some(self.commit_file_tree_nodes.len());
            }
        }

        let is_rebasing = model.is_rebasing;
        drop(model);

        // Auto-enter or resync rebase InProgress mode when a rebase is
        // detected on disk. If the view is already open, keep its todo status
        // in step with Git so `rebase --continue` can advance to the next
        // paused commit without leaving the old entry marked current.
        if is_rebasing {
            let should_open = !self.rebase_mode.active && !self.rebase_mode.in_progress_dismissed;
            let should_resync =
                self.rebase_mode.active && self.rebase_mode.phase == RebasePhase::InProgress;
            if should_open || should_resync {
                self.sync_rebase_progress_view();
            }
        }
        // If rebase mode was active but the rebase completed, exit and show success.
        if !is_rebasing && self.rebase_mode.active {
            if self.rebase_mode.phase == RebasePhase::InProgress {
                let branch = self.rebase_mode.branch_name.clone();
                let count = self.rebase_mode.total_count;
                self.rebase_mode.exit();
                self.popup = crate::gui::popup::PopupState::Message {
                    title: "Rebase complete".to_string(),
                    message: format!(
                        "Successfully rebased '{}' ({} commit{}).",
                        branch,
                        count,
                        if count == 1 { "" } else { "s" },
                    ),
                    kind: crate::gui::popup::MessageKind::Info,
                };
            }
        }
        // Clear the dismissal flag once no rebase is in progress, so the next
        // rebase (or new conflict) can auto-open the InProgress view again.
        if !is_rebasing && self.rebase_mode.in_progress_dismissed {
            self.rebase_mode.in_progress_dismissed = false;
        }

        Ok(())
    }

    /// Lightweight refresh that only reloads files and diff stats.
    /// Use this after staging/unstaging operations where branches, commits,
    /// tags, etc. haven't changed.
    fn refresh_files_only(&mut self) -> Result<()> {
        let (files, shortstat) = std::thread::scope(|s| {
            let h_files = s.spawn(|| self.git.load_files());
            let h_stat = s.spawn(|| self.git.diff_shortstat());
            (h_files.join().unwrap(), h_stat.join().unwrap())
        });

        let mut model = self.model.lock().unwrap();
        if let Ok(f) = files {
            model.set_files(f);
        }
        if let Ok((added, deleted)) = shortstat {
            model.total_additions = added;
            model.total_deletions = deleted;
        }

        if self.show_file_tree {
            self.file_tree_nodes = build_file_tree(&model.files, &self.collapsed_dirs);
            self.context_mgr.files_list_len_override = Some(self.file_tree_nodes.len());
        } else {
            self.file_tree_nodes.clear();
            self.context_mgr.files_list_len_override = None;
        }

        Ok(())
    }

    /// Resolve the currently selected file index in the files panel.
    /// In tree view, maps the tree node selection to the actual file index.
    /// Returns None if a directory node is selected (no file to operate on).
    pub fn selected_file_index(&self) -> Option<usize> {
        let selected = self.context_mgr.selected_active();
        if self.show_file_tree {
            self.file_tree_nodes
                .get(selected)
                .and_then(|node| node.file_index)
        } else {
            Some(selected)
        }
    }

    fn commit_history_path(config: &AppConfig) -> std::path::PathBuf {
        config.state_dir.join("commit_message_history")
    }

    fn persist_command_log_visibility(&self) {
        if let Ok(mut state) = AppState::load(&self.config.state_path) {
            state.show_command_log = Some(self.show_command_log);
            let _ = state.save(&self.config.state_path);
        }
    }

    pub fn persist_file_tree_visibility(&self) {
        if let Ok(mut state) = AppState::load(&self.config.state_path) {
            state.show_file_tree = Some(self.show_file_tree);
            let _ = state.save(&self.config.state_path);
        }
    }

    pub fn persist_commit_details_visibility(&self) {
        if let Ok(mut state) = AppState::load(&self.config.state_path) {
            state.show_commit_details = Some(self.show_commit_details);
            let _ = state.save(&self.config.state_path);
        }
    }

    pub fn persist_diff_line_wrap(&self) {
        if let Ok(mut state) = AppState::load(&self.config.state_path) {
            state.diff_line_wrap = Some(self.diff_view.wrap);
            let _ = state.save(&self.config.state_path);
        }
    }

    pub fn persist_diff_view_layout(&self) {
        if let Ok(mut state) = AppState::load(&self.config.state_path) {
            state.diff_view = Some(self.diff_view.view_layout.as_state_value().to_string());
            let _ = state.save(&self.config.state_path);
        }
    }

    fn load_commit_history(config: &AppConfig) -> Vec<String> {
        let path = Self::commit_history_path(config);
        match std::fs::read_to_string(&path) {
            Ok(contents) => contents
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Effective wrap width for the commit-body textarea, derived from popup
    /// geometry and the user's `git.commit.auto_wrap_width` config.
    fn commit_body_wrap_width(&self) -> usize {
        let popup_width = (self.layout.width * 60 / 100)
            .min(60)
            .max(30)
            .min(self.layout.width.max(1));
        let popup_inner = popup_width.saturating_sub(4) as usize;
        let config_width = self.config.user_config.git.commit.auto_wrap_width;
        if config_width > 0 {
            popup_inner.min(config_width)
        } else {
            popup_inner
        }
        .max(1)
    }

    fn save_commit_history(&self) {
        let path = Self::commit_history_path(&self.config);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let contents = self.commit_message_history.join("\0");
        let _ = std::fs::write(&path, contents);
    }

    pub fn update_file_tree_state(&mut self) {
        if self.show_file_tree {
            let model = self.model.lock().unwrap();
            self.file_tree_nodes = build_file_tree(&model.files, &self.collapsed_dirs);
            self.context_mgr.files_list_len_override = Some(self.file_tree_nodes.len());
        } else {
            self.file_tree_nodes.clear();
            self.context_mgr.files_list_len_override = None;
        }
    }

    /// Exit sub-contexts (like CommitFiles) back to their parent context
    /// before navigating away to another window.
    fn exit_sub_contexts(&mut self) {
        self.range_select_anchor = None;
        if self.context_mgr.active() == ContextId::CommitFiles {
            self.context_mgr.set_active(ContextId::Commits);
        }
        if self.context_mgr.active() == ContextId::StashFiles {
            self.context_mgr.set_active(ContextId::Stash);
        }
        if self.context_mgr.active() == ContextId::BranchCommitFiles {
            self.context_mgr.set_active(ContextId::BranchCommits);
        }
        if self.context_mgr.active() == ContextId::BranchCommits {
            self.context_mgr.set_active(ContextId::Branches);
        }
        if self.context_mgr.active() == ContextId::RemoteBranches {
            self.context_mgr.set_active(ContextId::Remotes);
        }
    }

    fn next_screen_mode(&mut self) {
        self.screen_mode = match self.screen_mode {
            ScreenMode::Normal => ScreenMode::Half,
            ScreenMode::Half => ScreenMode::Full,
            ScreenMode::Full => ScreenMode::Normal,
        };
    }

    fn prev_screen_mode(&mut self) {
        self.screen_mode = match self.screen_mode {
            ScreenMode::Normal => ScreenMode::Full,
            ScreenMode::Half => ScreenMode::Normal,
            ScreenMode::Full => ScreenMode::Half,
        };
    }
}

/// Split a commit message into (summary, body).
/// The summary is the first line; the body is everything after the first blank line separator.
fn split_commit_message(msg: &str) -> (String, String) {
    match msg.find('\n') {
        Some(idx) => {
            let summary = msg[..idx].to_string();
            let rest = msg[idx + 1..].trim_start_matches('\n').to_string();
            (summary, rest)
        }
        None => (msg.to_string(), String::new()),
    }
}

/// Auto-wrap all lines in a textarea so no line exceeds `wrap_width`.
/// Rebuilds the entire textarea content with hard line breaks at word boundaries.
/// Soft-wrap: like `auto_wrap_textarea` but preserves every character (including
/// spaces at line breaks). Inserts visual newlines only — callers join with `""`
/// at submit time to recover the original string. Used for single-line popup
/// inputs (branch name, tag name, etc.) that need browser-textarea-style visual
/// wrapping without polluting the value sent downstream.
fn soft_wrap_textarea(textarea: &mut tui_textarea::TextArea<'static>, wrap_width: usize) {
    if wrap_width == 0 {
        return;
    }

    let raw: String = textarea.lines().join("");
    if raw.is_empty() {
        return;
    }
    let chars: Vec<char> = raw.chars().collect();

    // Skip if already laid out correctly: every line ≤ wrap_width, and every
    // non-final line is exactly wrap_width chars.
    let lines = textarea.lines();
    let last = lines.len().saturating_sub(1);
    let already_ok = lines.iter().enumerate().all(|(i, l)| {
        let n = l.chars().count();
        if i < last {
            n == wrap_width
        } else {
            n <= wrap_width
        }
    });
    if already_ok {
        return;
    }

    // Track absolute char offset of cursor so we can restore it after rewrap.
    let (cursor_row, cursor_col) = textarea.cursor();
    let mut cursor_abs = 0usize;
    for (i, line) in textarea.lines().iter().enumerate() {
        let line_chars = line.chars().count();
        if i < cursor_row {
            cursor_abs += line_chars;
        } else {
            cursor_abs += cursor_col.min(line_chars);
            break;
        }
    }

    let mut wrapped: Vec<String> = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let end = (start + wrap_width).min(chars.len());
        wrapped.push(chars[start..end].iter().collect());
        start = end;
    }
    let new_text = wrapped.join("\n");

    // Map cursor back into the wrapped layout (each row is exactly wrap_width
    // chars except possibly the last).
    let new_row = cursor_abs / wrap_width;
    let new_col = cursor_abs % wrap_width;

    textarea.select_all();
    textarea.cut();
    textarea.insert_str(&new_text);
    textarea.move_cursor(tui_textarea::CursorMove::Top);
    textarea.move_cursor(tui_textarea::CursorMove::Head);
    for _ in 0..new_row {
        textarea.move_cursor(tui_textarea::CursorMove::Down);
    }
    for _ in 0..new_col {
        textarea.move_cursor(tui_textarea::CursorMove::Forward);
    }
}

fn auto_wrap_textarea(textarea: &mut tui_textarea::TextArea<'static>, wrap_width: usize) {
    if wrap_width == 0 {
        return;
    }

    let needs_wrap = textarea.lines().iter().any(|l| l.len() > wrap_width);
    if !needs_wrap {
        return;
    }

    // Compute cursor's absolute char offset in the original text
    let (cursor_row, cursor_col) = textarea.cursor();
    let original_lines: Vec<String> = textarea.lines().iter().map(|s| s.to_string()).collect();

    let mut cursor_abs = 0usize;
    for (i, line) in original_lines.iter().enumerate() {
        if i < cursor_row {
            cursor_abs += line.len() + 1;
        } else {
            cursor_abs += cursor_col.min(line.len());
            break;
        }
    }

    // Word-wrap all lines
    let mut wrapped: Vec<String> = Vec::new();
    for line in &original_lines {
        if line.len() <= wrap_width {
            wrapped.push(line.clone());
        } else {
            let mut remaining = line.as_str();
            while remaining.len() > wrap_width {
                let break_at = remaining[..wrap_width].rfind(' ').unwrap_or(wrap_width);
                let break_at = if break_at == 0 { wrap_width } else { break_at };
                wrapped.push(remaining[..break_at].to_string());
                remaining = remaining[break_at..].trim_start();
            }
            if !remaining.is_empty() {
                wrapped.push(remaining.to_string());
            }
        }
    }

    let new_text = wrapped.join("\n");

    // Map the absolute cursor offset into the new wrapped text
    // The wrapping only adds newlines (replacing spaces), so character content
    // is preserved. Walk the new text to find the right row/col.
    let mut abs = 0usize;
    let mut new_row = 0;
    let mut new_col = 0;
    for (i, wline) in wrapped.iter().enumerate() {
        if abs + wline.len() >= cursor_abs {
            new_row = i;
            new_col = (cursor_abs - abs).min(wline.len());
            break;
        }
        abs += wline.len() + 1; // +1 for newline
        new_row = i + 1;
        new_col = 0;
    }

    // Replace content and restore cursor
    textarea.select_all();
    textarea.cut();
    textarea.insert_str(&new_text);

    textarea.move_cursor(tui_textarea::CursorMove::Top);
    textarea.move_cursor(tui_textarea::CursorMove::Head);
    for _ in 0..new_row {
        textarea.move_cursor(tui_textarea::CursorMove::Down);
    }
    for _ in 0..new_col {
        textarea.move_cursor(tui_textarea::CursorMove::Forward);
    }
}

/// Read text from the system clipboard.
fn read_clipboard() -> Option<String> {
    let cmd = if cfg!(target_os = "macos") {
        "pbpaste"
    } else if cfg!(target_os = "windows") {
        "powershell.exe -command Get-Clipboard"
    } else {
        "xclip -selection clipboard -o"
    };

    std::process::Command::new("sh")
        .args(["-c", cmd])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn matches_key(key: KeyEvent, binding: &str) -> bool {
    if let Some(expected) = parse_key(binding) {
        // Compare code and modifiers, ignore kind/state
        key.code == expected.code && key.modifiers == expected.modifiers
    } else {
        false
    }
}

fn rect_contains(r: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
}

fn setup_terminal() -> Result<(Term, bool)> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::event::EnableFocusChange,
        crossterm::event::EnableBracketedPaste,
        crossterm::style::Print("\x1b[?1003h"), // Enable Any-event mouse tracking for Hover
        cursor::Hide
    )?;
    let keyboard_enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if keyboard_enhanced {
        execute!(
            stdout,
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | crossterm::event::KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                    | crossterm::event::KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
            )
        )?;
    }
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok((terminal, keyboard_enhanced))
}

/// Shell-escape a string so it can be safely passed as a single argument.
/// Wraps in single quotes and escapes any embedded single quotes.
fn shell_escape_arg(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Split a command string into program + args, respecting single-quoted
/// segments (the prompt is always single-quoted via `shell_escape_arg`).
fn split_command(cmd: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;

    for c in cmd.chars() {
        match c {
            '\'' if !in_single_quote => {
                in_single_quote = true;
            }
            '\'' if in_single_quote => {
                in_single_quote = false;
            }
            c if in_single_quote => {
                current.push(c);
            }
            ' ' | '\t' if !in_single_quote => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            c => {
                current.push(c);
            }
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn restore_terminal(terminal: &mut Term, keyboard_enhanced: bool) -> Result<()> {
    drain_pending_terminal_events(Duration::from_millis(0));

    if keyboard_enhanced {
        execute!(
            terminal.backend_mut(),
            crossterm::event::PopKeyboardEnhancementFlags
        )?;
    }
    execute!(
        terminal.backend_mut(),
        crossterm::style::Print("\x1b[?1003l"),
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableFocusChange,
        crossterm::event::DisableBracketedPaste,
        cursor::Show,
        LeaveAlternateScreen
    )?;

    terminal::disable_raw_mode()?;
    Ok(())
}
