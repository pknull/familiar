//! Sidebar pane widgets — renders configured data sources.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::config::PaneConfig;

/// Draw a sidebar pane based on its source type.
pub fn draw_pane(frame: &mut Frame, area: Rect, config: &PaneConfig, data: &PaneData) {
    let title = format!(" {} ", config.source);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title);

    let lines = match data {
        PaneData::Feed(items) => items
            .iter()
            .map(|item| {
                Line::from(vec![
                    Span::styled(
                        format!("[{}] ", item.content_type),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::raw(&item.summary),
                ])
            })
            .collect(),
        PaneData::Tasks(tasks) => tasks
            .iter()
            .map(|t| {
                let color = match t.status.as_str() {
                    "active" => Color::Green,
                    "pending" => Color::Yellow,
                    "failed" => Color::Red,
                    _ => Color::Gray,
                };
                Line::from(vec![
                    Span::styled(format!("[{}] ", t.status), Style::default().fg(color)),
                    Span::raw(&t.summary),
                ])
            })
            .collect(),
        PaneData::Peers(peers) => peers
            .iter()
            .map(|p| {
                let color = match p.health.as_str() {
                    "recent" => Color::Green,
                    "stale" => Color::Yellow,
                    "suspected" => Color::Red,
                    _ => Color::Gray,
                };
                Line::from(vec![
                    Span::styled("● ", Style::default().fg(color)),
                    Span::raw(&p.name),
                ])
            })
            .collect(),
        PaneData::Script(output) => output.lines().map(|l| Line::raw(l.to_string())).collect(),
        PaneData::Empty => {
            vec![Line::styled(
                "(no data)",
                Style::default().fg(Color::DarkGray),
            )]
        }
    };

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

/// Data for a sidebar pane — populated by background fetchers.
#[derive(Debug, Clone)]
pub enum PaneData {
    Feed(Vec<FeedItem>),
    Tasks(Vec<TaskItem>),
    Peers(Vec<PeerItem>),
    Script(String),
    Empty,
}

#[derive(Debug, Clone)]
pub struct FeedItem {
    pub content_type: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct TaskItem {
    pub status: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct PeerItem {
    pub name: String,
    pub health: String,
}
