//! Status bar widget — displays model, tokens, turn, session info via configurable template.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::AppState;

/// Interpolate the status bar template with state variables.
/// Supported placeholders: {model}, {turn}, {tokens}, {session}, {input_tokens}, {output_tokens}
fn interpolate(template: &str, state: &AppState) -> String {
    template
        .replace("{model}", &state.model)
        .replace("{turn}", &state.turn.to_string())
        .replace("{tokens}", &state.total_tokens().to_string())
        .replace("{session}", &state.session)
        .replace("{input_tokens}", &state.input_tokens.to_string())
        .replace("{output_tokens}", &state.output_tokens.to_string())
}

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState) {
    let streaming_indicator = if state.is_streaming { " ..." } else { "" };

    let left = format!(" {}", interpolate(&state.status_template, state));
    let right = format!("{} ", streaming_indicator);

    // Pad middle to fill the bar.
    let padding = area
        .width
        .saturating_sub(left.len() as u16 + right.len() as u16);
    let middle = " ".repeat(padding as usize);

    let line = Line::from(vec![
        Span::styled(left, Style::default().fg(Color::White)),
        Span::raw(middle),
        Span::styled(right, Style::default().fg(Color::Yellow)),
    ]);

    let bar = Paragraph::new(line).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(bar, area);
}
