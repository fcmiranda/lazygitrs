use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use crate::config::Theme;
use crate::model::Model;

pub fn render_reflog_list<'a>(model: &Model, _theme: &Theme) -> Vec<ListItem<'a>> {
    model
        .reflog_commits
        .iter()
        .map(|commit| {
            let spans = vec![
                Span::styled(
                    format!("{} ", commit.short_hash()),
                    Style::default().fg(Color::Blue),
                ),
                Span::styled(
                    commit.name.clone(),
                    Style::default().fg(Color::White),
                ),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect()
}
