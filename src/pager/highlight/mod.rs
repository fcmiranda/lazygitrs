mod config;
mod queries;

use std::collections::HashMap;
use std::path::Path;

use ratatui::prelude::*;
use tree_sitter_highlight::{HighlightEvent, Highlighter};

use config::{LanguageConfig, CONFIGS, HIGHLIGHT_NAMES};

/// Map a highlight index to a ratatui Color.
pub fn highlight_color(index: usize) -> Color {
    match HIGHLIGHT_NAMES.get(index) {
        Some(&"comment") => Color::Rgb(106, 115, 125),    // gray
        Some(&"keyword") => Color::Rgb(255, 123, 114),    // red/pink
        Some(&"string" | &"string.special") => Color::Rgb(158, 203, 255), // light blue
        Some(&"number" | &"constant" | &"constant.builtin") => Color::Rgb(121, 192, 255), // blue
        Some(&"function" | &"function.builtin" | &"function.method") => {
            Color::Rgb(210, 168, 255) // purple
        }
        Some(&"function.macro") => Color::Rgb(240, 160, 240), // light magenta
        Some(&"type" | &"type.builtin" | &"constructor") => Color::Rgb(255, 203, 107), // orange
        Some(&"variable.builtin") => Color::Rgb(255, 123, 114), // red
        Some(&"variable.member" | &"property") => Color::Rgb(121, 192, 255), // blue
        Some(&"module") => Color::Rgb(255, 203, 107),     // orange
        Some(&"operator") => Color::Rgb(255, 123, 114),   // red
        Some(&"tag") => Color::Rgb(126, 231, 135),        // green
        Some(&"attribute") => Color::Rgb(210, 168, 255),  // purple
        Some(&"label") => Color::Rgb(255, 203, 107),      // orange
        Some(&"punctuation" | &"punctuation.bracket" | &"punctuation.delimiter") => {
            Color::Rgb(150, 160, 170) // gray-ish
        }
        _ => Color::Rgb(201, 209, 217), // default text
    }
}

fn get_config_for_file(filename: &str) -> Option<&'static LanguageConfig> {
    let ext = Path::new(filename).extension().and_then(|e| e.to_str())?;
    CONFIGS.iter().find(|(e, _)| *e == ext).map(|(_, c)| c)
}

/// Pre-computed highlights for an entire file, organized by line number.
/// Handles multi-line constructs like JSDoc comments properly.
#[derive(Default)]
pub struct FileHighlighter {
    line_highlights: HashMap<usize, Vec<(String, Option<usize>)>>,
}

impl FileHighlighter {
    pub fn new(content: &str, filename: &str) -> Self {
        let Some(lang_config) = get_config_for_file(filename) else {
            return Self::default();
        };

        let mut highlighter = Highlighter::new();
        let highlights =
            highlighter.highlight(&lang_config.config, content.as_bytes(), None, |_| None);

        let Ok(highlights) = highlights else {
            return Self::default();
        };

        // Build byte offset -> line number map (1-based)
        let mut line_starts: Vec<usize> = vec![0];
        for (i, c) in content.char_indices() {
            if c == '\n' {
                line_starts.push(i + 1);
            }
        }

        let byte_to_line = |byte_offset: usize| -> usize {
            match line_starts.binary_search(&byte_offset) {
                Ok(line) => line + 1,
                Err(line) => line,
            }
        };

        let mut line_highlights: HashMap<usize, Vec<(String, Option<usize>)>> = HashMap::new();
        let mut current_highlight: Option<usize> = None;

        for event in highlights.flatten() {
            match event {
                HighlightEvent::Source { start, end } => {
                    let text = &content[start..end];
                    let start_line = byte_to_line(start);
                    let mut current_line = start_line;
                    let mut line_start = 0;

                    for (i, c) in text.char_indices() {
                        if c == '\n' {
                            let line_text = &text[line_start..i];
                            if !line_text.is_empty() {
                                line_highlights
                                    .entry(current_line)
                                    .or_default()
                                    .push((line_text.to_string(), current_highlight));
                            }
                            current_line += 1;
                            line_start = i + 1;
                        }
                    }

                    if line_start < text.len() {
                        let remaining = &text[line_start..];
                        line_highlights
                            .entry(current_line)
                            .or_default()
                            .push((remaining.to_string(), current_highlight));
                    }
                }
                HighlightEvent::HighlightStart(h) => {
                    current_highlight = Some(h.0);
                }
                HighlightEvent::HighlightEnd => {
                    current_highlight = None;
                }
            }
        }

        Self { line_highlights }
    }

    /// Get highlighted spans for a specific line (1-based line number).
    pub fn get_line_spans<'a>(&self, line_number: usize, bg: Option<Color>) -> Vec<Span<'a>> {
        let bg_color = bg.unwrap_or(Color::Reset);
        let default_fg = Color::Rgb(201, 209, 217);

        self.line_highlights
            .get(&line_number)
            .map(|spans| {
                spans
                    .iter()
                    .filter(|(text, _)| *text != "\n")
                    .map(|(text, highlight_idx)| {
                        let fg = highlight_idx.map(highlight_color).unwrap_or(default_fg);
                        Span::styled(text.clone(), Style::default().fg(fg).bg(bg_color))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}
