use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::config::Theme;

use super::highlight::FileHighlighter;
use super::{ChangeType, DiffLine, InlineSegment};

/// A section of a multi-file diff with its own highlighters.
struct FileSection {
    old_highlighter: FileHighlighter,
    new_highlighter: FileHighlighter,
}

/// Which panel of the side-by-side diff the selection is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffPanel {
    Old, // Left panel (deleted / original code)
    New, // Right panel (added / new code)
}

/// Mouse text selection state in the diff view.
#[derive(Clone, Debug)]
pub struct TextSelection {
    /// Which panel (left/right) the selection lives in.
    pub panel: DiffPanel,
    /// Terminal column where the selection started.
    pub start_col: u16,
    /// Terminal row where the selection started.
    pub start_row: u16,
    /// Terminal column where the selection currently ends.
    pub end_col: u16,
    /// Terminal row where the selection currently ends.
    pub end_row: u16,
    /// Whether the user is still dragging (selection in progress).
    pub dragging: bool,
    /// The extracted selected text (populated after rendering).
    pub text: String,
}

impl TextSelection {
    /// Returns (top_row, top_col, bottom_row, bottom_col) in normalized order.
    pub fn normalized(&self) -> (u16, u16, u16, u16) {
        if self.start_row < self.end_row
            || (self.start_row == self.end_row && self.start_col <= self.end_col)
        {
            (self.start_row, self.start_col, self.end_row, self.end_col)
        } else {
            (self.end_row, self.end_col, self.start_row, self.start_col)
        }
    }
}

/// Computed column layout for the diff panel's left/right content areas.
/// All coordinates are absolute terminal X positions.
#[derive(Clone, Copy, Debug)]
pub struct DiffPanelLayout {
    /// Whether this is a new-file diff (only right panel visible).
    pub is_new_file: bool,
    /// Left panel content start X (after gutter).
    pub old_content_x: u16,
    /// Left panel content end X (exclusive, up to divider).
    pub old_content_end_x: u16,
    /// Right panel content start X (after right gutter).
    pub new_content_x: u16,
    /// Right panel content end X (exclusive).
    pub new_content_end_x: u16,
    /// Inner area Y start (after top border).
    pub inner_y: u16,
    /// Inner area Y end (exclusive, before bottom border).
    pub inner_end_y: u16,
}

impl DiffPanelLayout {
    /// Compute the panel layout from a main panel Rect and diff state.
    pub fn compute(panel_rect: Rect, state: &DiffViewState) -> Self {
        let inner_x = panel_rect.x + 1; // border
        let inner_y = panel_rect.y + 1;
        let inner_w = panel_rect.width.saturating_sub(2);
        let inner_end_y = inner_y + panel_rect.height.saturating_sub(2);

        let gutter: u16 = 5;
        let divider: u16 = 1;

        let is_new_file = state.old_content.is_empty() && state.sections.len() <= 1;

        // Single-side view uses full-width single panel
        let single_side = match state.side_view {
            DiffSideView::OldOnly => Some(DiffPanel::Old),
            DiffSideView::NewOnly => Some(DiffPanel::New),
            DiffSideView::Both => None,
        };

        if single_side.is_some() || is_new_file {
            // Single panel — gutter(5) + content(rest)
            let content_x = inner_x + gutter;
            let content_end_x = inner_x + inner_w;
            let panel = single_side.unwrap_or(DiffPanel::New);
            let (old_x, old_end, new_x, new_end) = match panel {
                DiffPanel::Old => (content_x, content_end_x, 0, 0),
                DiffPanel::New => (0, 0, content_x, content_end_x),
            };
            Self {
                is_new_file: is_new_file && single_side.is_none(),
                old_content_x: old_x,
                old_content_end_x: old_end,
                new_content_x: new_x,
                new_content_end_x: new_end,
                inner_y,
                inner_end_y,
            }
        } else {
            let total_chrome = gutter * 2 + divider;
            let content_w = if inner_w > total_chrome { inner_w - total_chrome } else { 0 };
            let panel_w = content_w / 2;

            let old_content_x = inner_x + gutter;
            let old_content_end_x = old_content_x + panel_w;
            // divider is at old_content_end_x, right gutter starts at old_content_end_x + 1
            let new_content_x = old_content_end_x + divider + gutter;
            let new_content_end_x = inner_x + inner_w;

            Self {
                is_new_file: false,
                old_content_x,
                old_content_end_x,
                new_content_x,
                new_content_end_x,
                inner_y,
                inner_end_y,
            }
        }
    }

    /// Determine which panel an X coordinate falls in, if any.
    pub fn panel_at_x(&self, x: u16) -> Option<DiffPanel> {
        if self.is_new_file {
            if x >= self.new_content_x && x < self.new_content_end_x {
                return Some(DiffPanel::New);
            }
            // Also count gutter clicks as panel clicks
            if x >= self.new_content_x.saturating_sub(5) && x < self.new_content_end_x {
                return Some(DiffPanel::New);
            }
            return None;
        }
        // Include gutter in the clickable zone for each panel
        if x >= self.old_content_x.saturating_sub(5) && x < self.old_content_end_x {
            Some(DiffPanel::Old)
        } else if x >= self.new_content_x.saturating_sub(5) && x < self.new_content_end_x {
            Some(DiffPanel::New)
        } else {
            None
        }
    }

    /// Get the content column range for a given panel.
    pub fn content_range(&self, panel: DiffPanel) -> (u16, u16) {
        match panel {
            DiffPanel::Old => (self.old_content_x, self.old_content_end_x),
            DiffPanel::New => (self.new_content_x, self.new_content_end_x),
        }
    }
}

/// Which side(s) of the diff to display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSideView {
    Both,
    OldOnly,
    NewOnly,
}

/// A search match within the diff content, used for n/N navigation.
#[derive(Clone, Debug)]
pub struct DiffSearchMatch {
    /// Index into `DiffViewState::lines`.
    pub line_idx: usize,
    /// Which panel the match is on.
    pub panel: DiffPanel,
    /// Character (byte) offset within the line text.
    pub col: usize,
}

/// State for the diff view panel.
pub struct DiffViewState {
    pub scroll_offset: usize,
    pub horizontal_scroll: usize,
    pub lines: Vec<DiffLine>,
    pub hunk_starts: Vec<usize>,
    pub filename: String,
    pub old_content: String,
    pub new_content: String,
    pub tab_width: usize,
    /// Per-file-section highlighters for multi-file diffs.
    sections: Vec<FileSection>,
    /// Active mouse text selection, if any.
    pub selection: Option<TextSelection>,
    /// Which side(s) of the diff to show (Both, OldOnly, NewOnly).
    pub side_view: DiffSideView,
    /// Whether the search input is currently active (typing).
    pub search_active: bool,
    /// Current search query string.
    pub search_query: String,
    /// All matches found in the diff content.
    pub search_matches: Vec<DiffSearchMatch>,
    /// Index of the current match (for n/N navigation).
    pub search_match_idx: usize,
    /// Textarea widget for search input.
    pub search_textarea: Option<tui_textarea::TextArea<'static>>,
}

impl Default for DiffViewState {
    fn default() -> Self {
        Self {
            scroll_offset: 0,
            horizontal_scroll: 0,
            lines: Vec::new(),
            hunk_starts: Vec::new(),
            filename: String::new(),
            old_content: String::new(),
            new_content: String::new(),
            tab_width: 4,
            sections: Vec::new(),
            selection: None,
            side_view: DiffSideView::Both,
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_match_idx: 0,
            search_textarea: None,
        }
    }
}

impl DiffViewState {
    pub fn new() -> Self {
        Self {
            tab_width: 4,
            ..Default::default()
        }
    }

    /// Activate search mode with an empty query.
    pub fn start_search(&mut self) {
        self.search_active = true;
        self.search_query.clear();
        self.search_matches.clear();
        self.search_match_idx = 0;
        let mut ta = tui_textarea::TextArea::default();
        ta.set_cursor_line_style(Style::default());
        self.search_textarea = Some(ta);
    }

    /// Update search matches after the query changes.
    pub fn update_search(&mut self) {
        self.search_matches.clear();
        if self.search_query.is_empty() {
            self.search_match_idx = 0;
            return;
        }
        let query_lower = self.search_query.to_lowercase();
        for (line_idx, line) in self.lines.iter().enumerate() {
            if line.file_header.is_some() {
                continue;
            }
            // Search old side
            if let Some((_, ref text)) = line.old_line {
                let text_lower = text.to_lowercase();
                let mut start = 0;
                while let Some(pos) = text_lower[start..].find(&query_lower) {
                    self.search_matches.push(DiffSearchMatch {
                        line_idx,
                        panel: DiffPanel::Old,
                        col: start + pos,
                    });
                    start += pos + 1;
                }
            }
            // Search new side
            if let Some((_, ref text)) = line.new_line {
                let text_lower = text.to_lowercase();
                let mut start = 0;
                while let Some(pos) = text_lower[start..].find(&query_lower) {
                    self.search_matches.push(DiffSearchMatch {
                        line_idx,
                        panel: DiffPanel::New,
                        col: start + pos,
                    });
                    start += pos + 1;
                }
            }
        }
        // Clamp match index
        if self.search_matches.is_empty() {
            self.search_match_idx = 0;
        } else {
            self.search_match_idx = self.search_match_idx.min(self.search_matches.len() - 1);
        }
    }

    /// Dismiss search input but keep the query and highlights.
    pub fn dismiss_search(&mut self) {
        self.search_active = false;
        self.search_textarea = None;
    }

    /// Clear search entirely.
    pub fn clear_search(&mut self) {
        self.search_active = false;
        self.search_query.clear();
        self.search_matches.clear();
        self.search_match_idx = 0;
        self.search_textarea = None;
    }

    /// Navigate to the next search match and scroll to it.
    pub fn next_search_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        self.search_match_idx = (self.search_match_idx + 1) % self.search_matches.len();
        self.scroll_to_current_match();
    }

    /// Navigate to the previous search match and scroll to it.
    pub fn prev_search_match(&mut self) {
        if self.search_matches.is_empty() {
            return;
        }
        if self.search_match_idx == 0 {
            self.search_match_idx = self.search_matches.len() - 1;
        } else {
            self.search_match_idx -= 1;
        }
        self.scroll_to_current_match();
    }

    /// Scroll so the current search match is visible.
    pub fn scroll_to_current_match(&mut self) {
        if let Some(m) = self.search_matches.get(self.search_match_idx) {
            let line = m.line_idx;
            // Scroll so the match line is visible (roughly centered)
            if line < self.scroll_offset || line >= self.scroll_offset + 20 {
                self.scroll_offset = line.saturating_sub(5);
            }
        }
    }

    /// Load a diff from old/new content (single file).
    pub fn load(&mut self, filename: &str, old: &str, new: &str) {
        // Preserve scroll position when reloading the same file (e.g. periodic refresh)
        let same_file = self.filename == filename;
        self.filename = filename.to_string();
        self.old_content = old.to_string();
        self.new_content = new.to_string();
        self.lines = super::diff_algo::compute_side_by_side(old, new, self.tab_width);
        self.hunk_starts = super::diff_algo::find_hunk_starts(&self.lines);
        if same_file {
            // Clamp scroll in case the diff got shorter
            let max = self.lines.len().saturating_sub(1);
            self.scroll_offset = self.scroll_offset.min(max);
        } else {
            self.scroll_offset = 0;
            self.horizontal_scroll = 0;
            self.selection = None;
            self.clear_search();
        }
        // Preserve side_view across reloads so periodic refresh doesn't reset it
        // Single section with index 0
        self.sections = vec![FileSection {
            old_highlighter: FileHighlighter::new(old, filename),
            new_highlighter: FileHighlighter::new(new, filename),
        }];
    }

    /// Load from raw diff output (git diff).
    /// Automatically detects multi-file diffs and splits into per-file sections.
    pub fn load_from_diff_output(&mut self, filename: &str, diff_output: &str) {
        let file_diffs = parse_multi_file_diff(diff_output);

        if file_diffs.len() <= 1 {
            // Single file diff — use the simple path
            let (old, new) = parse_unified_diff(diff_output);
            // Use the actual filename from the diff header if available
            let actual_name = file_diffs
                .first()
                .map(|(name, _)| name.as_str())
                .unwrap_or(filename);
            self.load(actual_name, &old, &new);
        } else {
            // Multi-file diff — build per-section lines with highlighters
            let file_count = file_diffs.len();
            let new_filename = format!("{} ({} files)", filename, file_count);
            let same_file = self.filename == new_filename;
            self.filename = new_filename;
            self.old_content = String::new();
            self.new_content = String::new();
            self.lines = Vec::new();
            self.sections = Vec::new();
            if !same_file {
                self.scroll_offset = 0;
                self.horizontal_scroll = 0;
                self.selection = None;
                self.clear_search();
            }

            for (section_idx, (file_name, file_diff)) in file_diffs.iter().enumerate() {
                let (old, new) = parse_unified_diff(file_diff);

                // Add file header separator line
                self.lines.push(DiffLine {
                    old_line: None,
                    new_line: None,
                    change_type: ChangeType::Equal,
                    old_segments: None,
                    new_segments: None,
                    file_header: Some(file_name.clone()),
                    section_index: section_idx,
                });

                // Compute diff lines for this file section
                let mut section_lines =
                    super::diff_algo::compute_side_by_side(&old, &new, self.tab_width);
                for line in &mut section_lines {
                    line.section_index = section_idx;
                }
                self.lines.append(&mut section_lines);

                // Create highlighters for this section
                self.sections.push(FileSection {
                    old_highlighter: FileHighlighter::new(&old, file_name),
                    new_highlighter: FileHighlighter::new(&new, file_name),
                });
            }

            self.hunk_starts = super::diff_algo::find_hunk_starts(&self.lines);

            if same_file {
                // Clamp scroll in case the diff got shorter
                let max = self.lines.len().saturating_sub(1);
                self.scroll_offset = self.scroll_offset.min(max);
            }
        }
    }

    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    pub fn scroll_down(&mut self, amount: usize) {
        let max = self.lines.len().saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + amount).min(max);
    }

    pub fn scroll_left(&mut self, amount: usize) {
        self.horizontal_scroll = self.horizontal_scroll.saturating_sub(amount);
    }

    pub fn scroll_right(&mut self, amount: usize) {
        self.horizontal_scroll += amount;
    }

    pub fn next_hunk(&mut self) {
        if let Some(next) = self
            .hunk_starts
            .iter()
            .find(|&&h| h > self.scroll_offset)
        {
            self.scroll_offset = *next;
        }
    }

    pub fn prev_hunk(&mut self) {
        if let Some(prev) = self
            .hunk_starts
            .iter()
            .rev()
            .find(|&&h| h < self.scroll_offset)
        {
            self.scroll_offset = *prev;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Get the highlighters for a given section index.
    fn highlighters_for_section(&self, section_index: usize) -> Option<(&FileHighlighter, &FileHighlighter)> {
        self.sections
            .get(section_index)
            .map(|s| (&s.old_highlighter, &s.new_highlighter))
    }
}

/// Render a side-by-side diff view into the given area.
/// Uses direct buffer writes instead of per-cell Paragraph widgets for performance.
pub fn render_diff(
    frame: &mut Frame,
    area: Rect,
    state: &DiffViewState,
    theme: &Theme,
    focused: bool,
) {
    let border_style = if focused {
        theme.active_border
    } else {
        theme.inactive_border
    };

    if state.is_empty() {
        let block = Block::default()
            .title(" Diff ")
            .borders(Borders::ALL)
            .border_style(border_style);
        let widget = Paragraph::new(" No changes to display");
        frame.render_widget(widget.block(block), area);
        return;
    }

    let side_label = match state.side_view {
        DiffSideView::OldOnly => " [old] ",
        DiffSideView::NewOnly => " [new] ",
        DiffSideView::Both => "",
    };
    let title = if side_label.is_empty() {
        format!(" {} ", state.filename)
    } else {
        format!(" {}{}", state.filename, side_label)
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 10 || inner.height < 2 {
        return;
    }

    let gutter_width = 5u16;
    let divider_width = 1u16;

    // Detect new file: old content is empty, so no left panel needed
    let is_new_file = state.old_content.is_empty() && state.sections.len() <= 1;

    // Single-side view mode ([ for old, ] for new)
    let single_side = match state.side_view {
        DiffSideView::OldOnly => Some(DiffPanel::Old),
        DiffSideView::NewOnly => Some(DiffPanel::New),
        DiffSideView::Both => None,
    };

    let visible_height = inner.height as usize;
    let buf = frame.buffer_mut();

    if single_side.is_some() || is_new_file {
        // Single-panel mode: new file, old-only, or new-only
        let show_panel = single_side.unwrap_or(DiffPanel::New); // new-file defaults to New
        let content_width = inner.width.saturating_sub(gutter_width);

        for (row, diff_line) in state.lines[state.scroll_offset..]
            .iter()
            .take(visible_height)
            .enumerate()
        {
            let y = inner.y + row as u16;
            if y >= inner.y + inner.height {
                break;
            }

            // Handle file header separator lines
            if let Some(ref header) = diff_line.file_header {
                render_file_header(buf, inner.x, y, inner.width, header, theme);
                continue;
            }

            let default_hl = FileHighlighter::default();
            let (old_highlighter, new_highlighter) = state
                .highlighters_for_section(diff_line.section_index)
                .unwrap_or((&default_hl, &default_hl));

            // Pick the appropriate side's data
            let (line_data, segments, is_old_side, highlighter) = match show_panel {
                DiffPanel::Old => (
                    &diff_line.old_line,
                    &diff_line.old_segments,
                    true,
                    old_highlighter,
                ),
                DiffPanel::New => (
                    &diff_line.new_line,
                    &diff_line.new_segments,
                    false,
                    new_highlighter,
                ),
            };

            // Skip lines that only exist on the other side
            // (e.g., Insert lines have no old_line, Delete lines have no new_line)
            // Show them as blank with appropriate background
            let bg = if is_new_file {
                theme.diff_add_bg
            } else {
                match (diff_line.change_type, show_panel) {
                    (ChangeType::Delete, DiffPanel::Old) => theme.diff_remove_bg,
                    (ChangeType::Insert, DiffPanel::New) => theme.diff_add_bg,
                    (ChangeType::Modified, DiffPanel::Old) => theme.diff_remove_bg,
                    (ChangeType::Modified, DiffPanel::New) => theme.diff_add_bg,
                    _ => Color::Reset,
                }
            };

            // Gutter
            let line_num = line_data
                .as_ref()
                .map(|(n, _)| format!("{:>4} ", n))
                .unwrap_or_else(|| "     ".to_string());
            let gutter_style = Style::default().fg(Color::DarkGray).bg(bg);
            buf_write_str(buf, inner.x, y, &line_num, gutter_style, gutter_width);

            // Content
            if line_data.is_some() {
                let spans = build_content_spans(
                    line_data.as_ref().map(|(n, t)| (*n, t.as_str())),
                    segments,
                    diff_line.change_type,
                    is_old_side,
                    highlighter,
                    bg,
                    theme,
                    content_width as usize,
                );
                buf_write_spans(buf, inner.x + gutter_width, y, &spans, content_width, state.horizontal_scroll);
            } else {
                // Empty line (this side has no content for this row)
                let fill: String = std::iter::repeat(' ').take(content_width as usize).collect();
                buf_write_str(buf, inner.x + gutter_width, y, &fill, Style::default().bg(bg), content_width);
            }
        }
    } else {
        // Normal side-by-side diff
        let total_chrome = gutter_width * 2 + divider_width;
        let content_width = if inner.width > total_chrome {
            inner.width - total_chrome
        } else {
            inner.width
        };
        let panel_width = content_width / 2;

        for (row, diff_line) in state.lines[state.scroll_offset..]
            .iter()
            .take(visible_height)
            .enumerate()
        {
            let y = inner.y + row as u16;
            if y >= inner.y + inner.height {
                break;
            }

            // Handle file header separator lines
            if let Some(ref header) = diff_line.file_header {
                render_file_header(buf, inner.x, y, inner.width, header, theme);
                continue;
            }

            let default_hl = FileHighlighter::default();
            let (old_highlighter, new_highlighter) = state
                .highlighters_for_section(diff_line.section_index)
                .unwrap_or((&default_hl, &default_hl));

            let (left_bg, right_bg) = line_bg_colors(diff_line.change_type, theme);

            // Left gutter
            let left_num = diff_line
                .old_line
                .as_ref()
                .map(|(n, _)| format!("{:>4} ", n))
                .unwrap_or_else(|| "     ".to_string());
            let gutter_style = Style::default().fg(Color::DarkGray).bg(left_bg);
            buf_write_str(buf, inner.x, y, &left_num, gutter_style, gutter_width);

            // Left content
            let left_spans = if diff_line.change_type == ChangeType::Insert {
                // Addition-only line: fill left side with slash pattern
                let slash_fill: String = std::iter::repeat('/').take(panel_width as usize).collect();
                vec![Span::styled(
                    slash_fill,
                    Style::default().fg(Color::Rgb(60, 60, 60)).bg(left_bg),
                )]
            } else {
                build_content_spans(
                    diff_line.old_line.as_ref().map(|(n, t)| (*n, t.as_str())),
                    &diff_line.old_segments,
                    diff_line.change_type,
                    true,
                    old_highlighter,
                    left_bg,
                    theme,
                    panel_width as usize,
                )
            };
            buf_write_spans(buf, inner.x + gutter_width, y, &left_spans, panel_width, state.horizontal_scroll);

            // Divider
            let div_x = inner.x + gutter_width + panel_width;
            let divider_style = Style::default().fg(Color::DarkGray);
            buf_write_str(buf, div_x, y, "│", divider_style, divider_width);

            // Right gutter
            let right_num = diff_line
                .new_line
                .as_ref()
                .map(|(n, _)| format!("{:>4} ", n))
                .unwrap_or_else(|| "     ".to_string());
            let right_gutter_style = Style::default().fg(Color::DarkGray).bg(right_bg);
            let right_gutter_x = div_x + divider_width;
            buf_write_str(buf, right_gutter_x, y, &right_num, right_gutter_style, gutter_width);

            // Right content
            let right_spans = if diff_line.change_type == ChangeType::Delete {
                // Deletion-only line: fill right side with slash pattern
                let slash_fill: String = std::iter::repeat('/').take(panel_width as usize).collect();
                vec![Span::styled(
                    slash_fill,
                    Style::default().fg(Color::Rgb(60, 60, 60)).bg(right_bg),
                )]
            } else {
                build_content_spans(
                    diff_line.new_line.as_ref().map(|(n, t)| (*n, t.as_str())),
                    &diff_line.new_segments,
                    diff_line.change_type,
                    false,
                    new_highlighter,
                    right_bg,
                    theme,
                    panel_width as usize,
                )
            };
            let right_content_x = right_gutter_x + gutter_width;
            let right_content_width = inner
                .width
                .saturating_sub(gutter_width * 2 + panel_width + divider_width);
            buf_write_spans(buf, right_content_x, y, &right_spans, right_content_width, state.horizontal_scroll);
        }
    }
}

/// Render a file header separator line spanning the full width.
fn render_file_header(buf: &mut Buffer, x: u16, y: u16, width: u16, filename: &str, _theme: &Theme) {
    let buf_area = buf.area();
    if y < buf_area.y || y >= buf_area.y + buf_area.height {
        return;
    }

    let header_style = Style::default()
        .fg(Color::Rgb(158, 203, 255))
        .bg(Color::Rgb(30, 40, 55))
        .add_modifier(Modifier::BOLD);

    // Build header text: "── filename ──────..."
    let prefix = "── ";
    let suffix_char = '─';
    let label = format!("{}{} ", prefix, filename);
    let remaining = (width as usize).saturating_sub(label.len());
    let full_line = format!("{}{}", label, suffix_char.to_string().repeat(remaining));

    buf_write_str(buf, x, y, &full_line, header_style, width);
}

/// Write a string directly to the buffer at (x, y) with the given style, clamped to max_width.
#[inline]
fn buf_write_str(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style, max_width: u16) {
    let buf_area = buf.area();
    if y < buf_area.y || y >= buf_area.y + buf_area.height {
        return;
    }
    let mut col = x;
    let end_col = x.saturating_add(max_width).min(buf_area.x + buf_area.width);
    for ch in text.chars() {
        if col >= end_col {
            break;
        }
        let width = unicode_display_width(ch);
        if width == 0 {
            continue;
        }
        if let Some(cell) = buf.cell_mut((col, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
        col += width as u16;
    }
}

/// Write styled spans directly to the buffer at (x, y), clamped to max_width.
/// `h_scroll` skips the first N display columns of content.
#[inline]
fn buf_write_spans(buf: &mut Buffer, x: u16, y: u16, spans: &[Span<'_>], max_width: u16, h_scroll: usize) {
    let buf_area = buf.area();
    if y < buf_area.y || y >= buf_area.y + buf_area.height {
        return;
    }
    let mut col = x;
    let end_col = x.saturating_add(max_width).min(buf_area.x + buf_area.width);
    let mut skipped: usize = 0;
    for span in spans {
        for ch in span.content.chars() {
            if col >= end_col {
                return;
            }
            let width = unicode_display_width(ch);
            if width == 0 {
                continue;
            }
            if skipped < h_scroll {
                skipped += width;
                continue;
            }
            if let Some(cell) = buf.cell_mut((col, y)) {
                cell.set_char(ch);
                cell.set_style(span.style);
            }
            col += width as u16;
        }
    }
}

/// Get the display width of a character (1 for most, 2 for CJK wide chars).
#[inline]
fn unicode_display_width(ch: char) -> usize {
    if ch == '\t' || ch == '\n' || ch == '\r' {
        return 0;
    }
    unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1)
}

/// Get background colors for a diff line based on change type.
fn line_bg_colors(change_type: ChangeType, theme: &Theme) -> (Color, Color) {
    match change_type {
        ChangeType::Equal => (Color::Reset, Color::Reset),
        ChangeType::Delete => (theme.diff_remove_bg, Color::Reset),
        ChangeType::Insert => (Color::Reset, theme.diff_add_bg),
        ChangeType::Modified => (theme.diff_remove_bg, theme.diff_add_bg),
    }
}

/// Build styled spans for one side of a diff line.
#[allow(clippy::too_many_arguments)]
fn build_content_spans<'a>(
    line_data: Option<(usize, &str)>,
    segments: &Option<Vec<InlineSegment>>,
    change_type: ChangeType,
    is_old_side: bool,
    highlighter: &FileHighlighter,
    bg: Color,
    theme: &Theme,
    max_width: usize,
) -> Vec<Span<'a>> {
    let Some((line_num, text)) = line_data else {
        // Empty side — fill with background
        let fill = " ".repeat(max_width);
        return vec![Span::styled(fill, Style::default().bg(bg))];
    };

    // If we have word-level segments, use those
    if let Some(segs) = segments {
        return build_word_diff_spans(segs, is_old_side, bg, theme, max_width);
    }

    // Otherwise, try syntax highlighting
    let highlighted = highlighter.get_line_spans(line_num, Some(bg));
    if !highlighted.is_empty() {
        return highlighted;
    }

    // Fallback: plain text with background
    let fg = match change_type {
        ChangeType::Delete => theme.diff_remove.fg.unwrap_or(Color::Red),
        ChangeType::Insert => theme.diff_add.fg.unwrap_or(Color::Green),
        _ => Color::White,
    };

    let display = if text.len() > max_width {
        // Find a safe byte boundary to avoid slicing mid-character
        let mut end = max_width;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    } else {
        text
    };

    vec![Span::styled(
        display.to_string(),
        Style::default().fg(fg).bg(bg),
    )]
}

/// Build spans with word-level diff emphasis.
fn build_word_diff_spans<'a>(
    segments: &[InlineSegment],
    is_old_side: bool,
    bg: Color,
    theme: &Theme,
    _max_width: usize,
) -> Vec<Span<'a>> {
    segments
        .iter()
        .map(|seg| {
            if seg.emphasized {
                let emphasis_bg = if is_old_side {
                    theme.diff_remove_word
                } else {
                    theme.diff_add_word
                };
                Span::styled(
                    seg.text.clone(),
                    Style::default()
                        .bg(emphasis_bg)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(seg.text.clone(), Style::default().bg(bg).fg(Color::White))
            }
        })
        .collect()
}

/// Render search highlights over the diff buffer by scanning visible content areas
/// for occurrences of the search query and applying a highlight style.
/// `current_match_line` is the line index of the currently selected match (for emphasis).
pub fn render_diff_search_highlights(
    frame: &mut Frame,
    area: Rect,
    state: &DiffViewState,
) {
    if state.search_query.is_empty() || state.search_matches.is_empty() {
        return;
    }

    let pl = DiffPanelLayout::compute(area, state);
    let query_lower = state.search_query.to_lowercase();
    let query_len = query_lower.len();
    let current_match_line = state
        .search_matches
        .get(state.search_match_idx)
        .map(|m| m.line_idx);

    let buf = frame.buffer_mut();
    let buf_area = *buf.area();

    // Scan each content area (old panel, new panel) for matches
    let panel_ranges: Vec<(u16, u16)> = {
        let mut ranges = Vec::new();
        if pl.old_content_x > 0 && pl.old_content_end_x > pl.old_content_x {
            ranges.push((pl.old_content_x, pl.old_content_end_x));
        }
        if pl.new_content_x > 0 && pl.new_content_end_x > pl.new_content_x {
            ranges.push((pl.new_content_x, pl.new_content_end_x));
        }
        ranges
    };

    let visible_height = area.height.saturating_sub(2) as usize; // -2 for borders
    let highlight_style = Style::default().bg(Color::Rgb(120, 100, 30)).fg(Color::White);
    let current_highlight_style = Style::default()
        .bg(Color::Rgb(200, 170, 40))
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);

    for row_offset in 0..visible_height {
        let line_idx = state.scroll_offset + row_offset;
        let y = pl.inner_y + row_offset as u16;
        if y >= pl.inner_end_y || y >= buf_area.y + buf_area.height {
            break;
        }

        let is_current_line = current_match_line == Some(line_idx);

        for &(range_start, range_end) in &panel_ranges {
            // Read the row text from buffer cells in this range
            let mut row_chars: Vec<(u16, char)> = Vec::new();
            for x in range_start..range_end.min(buf_area.x + buf_area.width) {
                if let Some(cell) = buf.cell((x, y)) {
                    let ch = cell.symbol().chars().next().unwrap_or(' ');
                    row_chars.push((x, ch));
                }
            }

            // Build string for searching
            let row_text: String = row_chars.iter().map(|(_, ch)| *ch).collect();
            let row_lower = row_text.to_lowercase();

            let mut start = 0;
            while let Some(pos) = row_lower[start..].find(&query_lower) {
                let match_start = start + pos;
                let match_end = match_start + query_len;
                let style = if is_current_line {
                    current_highlight_style
                } else {
                    highlight_style
                };
                for i in match_start..match_end {
                    if i < row_chars.len() {
                        let x = row_chars[i].0;
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_style(style);
                        }
                    }
                }
                start = match_start + 1;
            }
        }
    }
}

/// Render a search bar at the bottom of the diff panel area.
pub fn render_diff_search_bar(
    frame: &mut Frame,
    area: Rect,
    state: &DiffViewState,
) {
    // Only render if search is active (typing) or has a query (dismissed but results shown)
    if !state.search_active && state.search_query.is_empty() {
        return;
    }

    // Position at the bottom row of the panel (inside the border)
    let bar_y = area.y + area.height.saturating_sub(2);
    let bar_x = area.x + 1;
    let bar_width = area.width.saturating_sub(2);

    if bar_width < 10 {
        return;
    }

    let bar_rect = Rect::new(bar_x, bar_y, bar_width, 1);

    // Clear the bar area
    let buf = frame.buffer_mut();
    for x in bar_rect.x..bar_rect.x + bar_rect.width {
        if let Some(cell) = buf.cell_mut((x, bar_y)) {
            cell.set_char(' ');
            cell.set_style(Style::default().bg(Color::Rgb(40, 40, 50)));
        }
    }

    let match_info = if !state.search_matches.is_empty() {
        format!(
            " {}/{}",
            state.search_match_idx + 1,
            state.search_matches.len()
        )
    } else if !state.search_query.is_empty() {
        " (no matches)".to_string()
    } else {
        String::new()
    };

    if state.search_active {
        // Render with textarea
        let prefix_width = 2u16; // " /"
        let suffix_width = match_info.len() as u16;
        let ta_width = bar_width.saturating_sub(prefix_width + suffix_width);

        let prefix_rect = Rect::new(bar_rect.x, bar_y, prefix_width, 1);
        let prefix = Paragraph::new(Span::styled(
            " /",
            Style::default().fg(Color::Yellow).bg(Color::Rgb(40, 40, 50)),
        ));
        frame.render_widget(prefix, prefix_rect);

        if let Some(ref ta) = state.search_textarea {
            let ta_rect = Rect::new(bar_rect.x + prefix_width, bar_y, ta_width, 1);
            frame.render_widget(&*ta, ta_rect);
        }

        if !match_info.is_empty() {
            let suffix_rect = Rect::new(bar_rect.x + prefix_width + ta_width, bar_y, suffix_width, 1);
            let suffix = Paragraph::new(Span::styled(
                match_info,
                Style::default().fg(Color::Yellow).bg(Color::Rgb(40, 40, 50)),
            ));
            frame.render_widget(suffix, suffix_rect);
        }
    } else {
        // Dismissed search — show query + match info
        let text = format!(" /{}{}", state.search_query, match_info);
        let style = Style::default().fg(Color::Yellow).bg(Color::Rgb(40, 40, 50));
        buf_write_str(
            frame.buffer_mut(),
            bar_rect.x,
            bar_y,
            &text,
            style,
            bar_width,
        );
    }
}

/// Parse a multi-file unified diff into per-file sections.
/// Returns Vec of (filename, raw_diff_for_that_file).
fn parse_multi_file_diff(diff: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
    let mut current_filename = String::new();
    let mut current_lines: Vec<&str> = Vec::new();

    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            // Save previous section
            if !current_filename.is_empty() {
                sections.push((current_filename, current_lines));
                current_lines = Vec::new();
            }
            // Extract filename from "diff --git a/path b/path"
            current_filename = extract_filename_from_diff_header(line);
        } else {
            current_lines.push(line);
        }
    }

    // Save last section
    if !current_filename.is_empty() {
        sections.push((current_filename, current_lines));
    }

    sections
        .into_iter()
        .map(|(name, lines)| (name, lines.join("\n")))
        .collect()
}

/// Extract the filename from a "diff --git a/path b/path" header line.
fn extract_filename_from_diff_header(line: &str) -> String {
    // Format: "diff --git a/some/path b/some/path"
    // We want "some/path" (the b/ side, which is the new name)
    if let Some(b_part) = line.split(" b/").last() {
        b_part.to_string()
    } else {
        // Fallback: strip "diff --git " prefix
        line.trim_start_matches("diff --git ").to_string()
    }
}

/// Parse a unified diff into old/new content for side-by-side display.
/// This handles `git diff` output format.
fn parse_unified_diff(diff: &str) -> (String, String) {
    let mut old_lines = Vec::new();
    let mut new_lines = Vec::new();
    let mut in_hunk = false;

    for line in diff.lines() {
        if line.starts_with("@@") {
            in_hunk = true;
            continue;
        }

        if !in_hunk {
            continue;
        }

        if line.starts_with('-') {
            old_lines.push(&line[1..]);
        } else if line.starts_with('+') {
            new_lines.push(&line[1..]);
        } else if let Some(ctx) = line.strip_prefix(' ') {
            old_lines.push(ctx);
            new_lines.push(ctx);
        } else {
            // Could be "\ No newline at end of file" or other metadata
            old_lines.push(line);
            new_lines.push(line);
        }
    }

    (old_lines.join("\n"), new_lines.join("\n"))
}
