use ratatui::layout::{Constraint, Direction, Layout, Rect};

use super::ScreenMode;

/// Tracks the terminal size and computes panel rects.
#[derive(Debug)]
pub struct LayoutState {
    pub width: u16,
    pub height: u16,
    pub side_panel_ratio: f64,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            width: 80,
            height: 24,
            side_panel_ratio: 0.3333,
        }
    }
}

impl LayoutState {
    pub fn update_size(&mut self, w: u16, h: u16) {
        self.width = w;
        self.height = h;
    }
}

/// The computed layout rects for a single frame.
pub struct FrameLayout {
    pub side_panels: Vec<Rect>,
    pub main_panel: Rect,
    pub status_bar: Rect,
    /// Whether portrait (vertical stack) layout is in effect.
    pub portrait: bool,
}

/// Height for the status panel (always compact: 1 content line + 2 border lines).
const STATUS_PANEL_HEIGHT: u16 = 3;

/// Portrait mode threshold: narrow terminal with enough vertical space.
/// Matches the original lazygit: width <= 84 && height > 45.
fn should_use_portrait(width: u16, height: u16, screen_mode: ScreenMode) -> bool {
    screen_mode == ScreenMode::Normal && width <= 84 && height > 45
}

pub fn compute_layout(
    area: Rect,
    side_ratio: f64,
    panel_count: usize,
    active_panel_index: usize,
    screen_mode: ScreenMode,
) -> FrameLayout {
    // Top-level: main area + status bar at bottom
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1), // status bar
        ])
        .split(area);

    let main_area = outer[0];
    let status_bar = outer[1];

    // Full screen mode: no side panel, main takes everything
    if screen_mode == ScreenMode::Full {
        return FrameLayout {
            side_panels: Vec::new(),
            main_panel: main_area,
            status_bar,
            portrait: false,
        };
    }

    let portrait = should_use_portrait(area.width, area.height, screen_mode);

    if portrait {
        // Portrait mode: side panels stacked vertically on top, main panel below.
        // The side section gets roughly half the height so both sections are usable.
        let side_height = (main_area.height as f64 * side_ratio).round() as u16;
        let side_height = side_height
            .max(panel_count as u16 * 2)
            .min(main_area.height / 2);

        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(side_height),
                Constraint::Min(1),
            ])
            .split(main_area);

        let side_area = vertical[0];
        let main_panel = vertical[1];

        // Side panels laid out vertically (same as landscape), active expands.
        let collapsed: u16 = if side_area.height < 21 { 1 } else { 3 };
        let panel_constraints: Vec<Constraint> = (0..panel_count)
            .map(|i| {
                if i == 0 {
                    Constraint::Length(STATUS_PANEL_HEIGHT)
                } else if i == active_panel_index {
                    Constraint::Min(collapsed)
                } else {
                    Constraint::Length(collapsed)
                }
            })
            .collect();

        let side_panels = Layout::default()
            .direction(Direction::Vertical)
            .constraints(panel_constraints)
            .split(side_area)
            .to_vec();

        return FrameLayout {
            side_panels,
            main_panel,
            status_bar,
            portrait: true,
        };
    }

    // Landscape mode (default): side panel on the left, main on the right.

    // Half mode: side panel enlarges to 50/50 split
    // Normal mode: use the configured ratio
    let effective_ratio = match screen_mode {
        ScreenMode::Half => 0.5,
        _ => side_ratio,
    };

    // Split main area into side panel and main content
    let side_width = ((main_area.width as f64) * effective_ratio) as u16;
    let max_side = if screen_mode == ScreenMode::Half {
        main_area.width.saturating_sub(20) // leave at least 20 cols for main
    } else {
        main_area.width / 2
    };
    let side_width = side_width.max(20).min(max_side);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(side_width),
            Constraint::Min(1),
        ])
        .split(main_area);

    let side_area = horizontal[0];
    let main_panel = horizontal[1];

    // Side panel sizing: active panel expands, others collapse.
    // On very short terminals (< 21 rows) unfocused panels shrink to 1 line.
    let side_height = side_area.height;
    let collapsed: u16 = if side_height < 21 { 1 } else { 3 };

    let panel_constraints: Vec<Constraint> = (0..panel_count)
        .map(|i| {
            if i == 0 {
                Constraint::Length(STATUS_PANEL_HEIGHT)
            } else if i == active_panel_index {
                Constraint::Min(collapsed)
            } else {
                Constraint::Length(collapsed)
            }
        })
        .collect();

    let side_panels = Layout::default()
        .direction(Direction::Vertical)
        .constraints(panel_constraints)
        .split(side_area)
        .to_vec();

    FrameLayout {
        side_panels,
        main_panel,
        status_bar,
        portrait: false,
    }
}
