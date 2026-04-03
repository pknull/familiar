//! Conversation widget — scrollable message display with streaming support.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::{AppState, FocusTarget};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState) {
    let border_style = if state.focus == FocusTarget::Conversation {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" conversation ");

    let inner = block.inner(area);

    // Build lines from message history.
    let mut lines: Vec<Line> = Vec::new();

    for msg in &state.messages {
        let (prefix, style) = match msg.role.as_str() {
            "user" => (
                "you: ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            "assistant" => ("familiar: ", Style::default().fg(Color::Blue)),
            _ => ("system: ", Style::default().fg(Color::DarkGray)),
        };

        lines.push(Line::from(Span::styled(prefix, style)));
        for text_line in msg.content.lines() {
            lines.push(Line::from(text_line.to_string()));
        }
        lines.push(Line::from("")); // blank line between messages
    }

    // Append streaming content.
    if let Some(ref partial) = state.streaming {
        lines.push(Line::from(Span::styled(
            "familiar: ",
            Style::default().fg(Color::Blue),
        )));
        for text_line in partial.lines() {
            lines.push(Line::from(text_line.to_string()));
        }
        // Cursor indicator.
        lines.push(Line::from(Span::styled(
            "\u{2588}",
            Style::default().fg(Color::Yellow),
        )));
    }

    // Compute scroll: auto-scroll to bottom unless user has scrolled up.
    let content_height = lines.len() as u16;
    let visible_height = inner.height;
    let scroll = if state.auto_scroll {
        content_height.saturating_sub(visible_height)
    } else {
        content_height
            .saturating_sub(visible_height)
            .saturating_sub(state.scroll_offset)
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(paragraph, area);
}
