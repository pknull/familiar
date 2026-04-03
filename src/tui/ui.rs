//! Top-level draw function — composes widgets into the layout.

use ratatui::Frame;
use ratatui_textarea::TextArea;

use super::layout::{compute_layout, split_sidebar};
use super::widgets::{conversation, input, sidebar, status_bar};
use super::AppState;
use crate::config::PaneConfig;

/// Draw the full TUI frame with the TextArea input widget.
pub fn draw(frame: &mut Frame, state: &AppState, textarea: &TextArea, panes: &[PaneConfig]) {
    let layout = compute_layout(frame.area(), panes, state.sidebar_visible);

    status_bar::draw(frame, layout.status_bar, state);
    conversation::draw(frame, layout.conversation, state);
    input::draw(frame, layout.input, state, textarea);

    // Draw sidebar panes if visible.
    if let Some(sidebar_area) = layout.sidebar {
        let pane_areas = split_sidebar(sidebar_area, panes.len());
        for (i, (config, area)) in panes.iter().zip(pane_areas.iter()).enumerate() {
            let data = state
                .pane_data
                .get(i)
                .cloned()
                .unwrap_or(sidebar::PaneData::Empty);
            sidebar::draw_pane(frame, *area, config, &data);
        }
    }
}
