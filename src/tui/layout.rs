//! Layout engine — computes pane regions from config and terminal size.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::config::PaneConfig;

/// Layout regions for the TUI.
pub struct AppLayout {
    pub status_bar: Rect,
    pub conversation: Rect,
    pub input: Rect,
    pub sidebar: Option<Rect>,
}

/// Compute the layout. If panes are configured and sidebar is visible,
/// splits the main area into conversation+input (left) and sidebar (right).
pub fn compute_layout(area: Rect, panes: &[PaneConfig], sidebar_visible: bool) -> AppLayout {
    // Vertical split: status bar (1) + main body (fill) + input (3).
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status bar
            Constraint::Fill(1),   // main body
            Constraint::Min(3),    // input
        ])
        .split(area);

    let status_bar = vertical[0];
    let body = vertical[1];
    let input = vertical[2];

    // If sidebar is visible and panes configured, horizontal split the body.
    if sidebar_visible && !panes.is_empty() {
        let horizontal = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(70), // conversation
                Constraint::Percentage(30), // sidebar
            ])
            .split(body);

        AppLayout {
            status_bar,
            conversation: horizontal[0],
            input,
            sidebar: Some(horizontal[1]),
        }
    } else {
        AppLayout {
            status_bar,
            conversation: body,
            input,
            sidebar: None,
        }
    }
}

/// Split sidebar area into pane regions based on pane configs.
pub fn split_sidebar(sidebar: Rect, pane_count: usize) -> Vec<Rect> {
    if pane_count == 0 {
        return Vec::new();
    }

    let constraints: Vec<Constraint> = (0..pane_count)
        .map(|_| Constraint::Ratio(1, pane_count as u32))
        .collect();

    Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(sidebar)
        .to_vec()
}
