//! Input widget — wraps tui-textarea for multiline text entry.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;
use ratatui_textarea::TextArea;

use crate::tui::{AppState, FocusTarget};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, textarea: &TextArea) {
    let border_color = if state.focus == FocusTarget::Input {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let title = if state.is_streaming {
        " streaming... (waiting) "
    } else if state.focus == FocusTarget::Input {
        " input (Enter=send, Shift+Enter=newline, Tab=scroll) "
    } else {
        " input "
    };

    // The TextArea manages its own block, so we clone and set our styling.
    let mut ta = textarea.clone();
    ta.set_block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(title),
    );

    if state.focus == FocusTarget::Input {
        ta.set_cursor_line_style(Style::default());
        ta.set_cursor_style(Style::default().fg(Color::White).bg(Color::Cyan));
    } else {
        ta.set_cursor_style(Style::default());
    }

    frame.render_widget(&ta, area);
}
