/// Commit graph layout engine.
///
/// Given an ordered list of commits (newest first) with their parent hashes,
/// computes an ASCII graph column layout similar to `git log --graph`.

use ratatui::style::{Color, Style};
use ratatui::text::Span;

/// The computed graph cell for one commit row.
#[derive(Debug, Clone)]
pub struct GraphRow {
    /// The column (0-based) where this commit's node sits.
    pub commit_col: usize,
    /// The graph cells to render before the commit info.
    pub cells: Vec<GraphCell>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphCell {
    /// The commit node itself: ●
    Node,
    /// A vertical pipe passing through: │
    Pipe,
    /// A merge line coming from the right: ╮
    MergeRight,
    /// Horizontal connector: ─
    Horizontal,
    /// Empty space
    Empty,
}

/// Graph colors — each column gets a rotating color.
const GRAPH_COLORS: &[Color] = &[
    Color::Green,
    Color::Yellow,
    Color::Magenta,
    Color::Cyan,
    Color::Blue,
    Color::Red,
    Color::LightGreen,
    Color::LightYellow,
];

fn col_color(col: usize) -> Color {
    GRAPH_COLORS[col % GRAPH_COLORS.len()]
}

/// Compute graph layout for a list of commits.
///
/// `commits` is a slice of (hash, parent_hashes) in display order (newest first).
///
/// The algorithm tracks "lanes" — each lane holds the hash of a commit it is
/// waiting for. When that commit appears, the lane either continues (first parent
/// takes over) or closes. Merge parents open new lanes to the right.
pub fn compute_graph(commits: &[(String, Vec<String>)]) -> Vec<GraphRow> {
    // `lanes[i]` = Some(hash) means lane i is an open line waiting for `hash`.
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for (hash, parents) in commits {
        // 1. Find which lane this commit lives in.
        let commit_col = if let Some(col) = lanes.iter().position(|l| l.as_deref() == Some(hash)) {
            col
        } else {
            // No lane waiting for us — find an empty slot or append.
            if let Some(empty) = lanes.iter().position(|l| l.is_none()) {
                empty
            } else {
                lanes.push(None);
                lanes.len() - 1
            }
        };

        // 2. Collect all lanes pointing to this commit (there can be multiple
        //    if several branches converge here). Close them all.
        let mut closing: Vec<usize> = Vec::new();
        for (i, lane) in lanes.iter().enumerate() {
            if lane.as_deref() == Some(hash) {
                closing.push(i);
            }
        }
        for &c in &closing {
            lanes[c] = None;
        }

        // 3. Assign parents.
        let first_parent = parents.first();
        let merge_parents = if parents.len() > 1 { &parents[1..] } else { &[] };

        // First parent continues in the commit's lane.
        if let Some(fp) = first_parent {
            lanes[commit_col] = Some(fp.clone());
        }

        // For closing lanes that aren't the commit_col, re-assign them to the
        // first parent so the line visually merges into the commit's lane.
        // (They were already cleared above — the first parent inherits the
        // commit_col lane, so closing lanes simply disappear.)

        // Merge parents get new lanes.
        let mut merge_cols: Vec<usize> = Vec::new();
        for mp in merge_parents {
            // Check if any existing lane already tracks this parent.
            if let Some(existing) = lanes.iter().position(|l| l.as_deref() == Some(mp.as_str())) {
                merge_cols.push(existing);
            } else {
                // Allocate new lane.
                let col = if let Some(empty) = lanes.iter().position(|l| l.is_none()) {
                    lanes[empty] = Some(mp.clone());
                    empty
                } else {
                    lanes.push(Some(mp.clone()));
                    lanes.len() - 1
                };
                merge_cols.push(col);
            }
        }

        // 4. Build cells for this row.
        let width = lanes.len().max(commit_col + 1);
        let mut cells = vec![GraphCell::Empty; width];

        // Draw vertical pipes for all active lanes (except commit_col which gets a node).
        for (i, lane) in lanes.iter().enumerate() {
            if lane.is_some() && i != commit_col {
                cells[i] = GraphCell::Pipe;
            }
        }

        // The commit node.
        cells[commit_col] = GraphCell::Node;

        // Draw merge lines from merge-parent lanes to the commit.
        for &mc in &merge_cols {
            if mc > commit_col {
                for c in (commit_col + 1)..mc {
                    if cells[c] == GraphCell::Empty {
                        cells[c] = GraphCell::Horizontal;
                    }
                }
                cells[mc] = GraphCell::MergeRight;
            }
        }

        // Trim trailing empties.
        while cells.last() == Some(&GraphCell::Empty) {
            cells.pop();
        }

        rows.push(GraphRow { commit_col, cells });

        // Trim trailing empty lanes.
        while lanes.last() == Some(&None) {
            lanes.pop();
        }
    }

    rows
}

/// Render a GraphRow into a Vec<Span> for display.
pub fn render_graph_spans(row: &GraphRow, max_width: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();

    for (i, cell) in row.cells.iter().enumerate() {
        let (ch, style) = match cell {
            GraphCell::Node => ("●", Style::default().fg(col_color(row.commit_col))),
            GraphCell::Pipe => ("│", Style::default().fg(col_color(i))),
            GraphCell::MergeRight => ("╮", Style::default().fg(col_color(i))),
            GraphCell::Horizontal => ("─", Style::default().fg(col_color(row.commit_col))),
            GraphCell::Empty => (" ", Style::default()),
        };
        spans.push(Span::styled(ch.to_string(), style));
    }

    // Pad to max_width so commit info aligns across rows.
    let current = row.cells.len();
    if current < max_width {
        spans.push(Span::raw(" ".repeat(max_width - current)));
    }

    // Separator.
    spans.push(Span::raw(" "));

    spans
}
