//! TUI operator console — ratatui-based default interface.
//!
//! Provides a multi-pane terminal UI with conversation display,
//! input widget, status bar, and configurable sidebar panes.

pub mod layout;
pub mod ui;
pub mod widgets;

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use ratatui_textarea::TextArea;
use tokio::sync::mpsc;
use tokio::sync::Mutex as TokioMutex;

use crate::channel::tui_channel::InputSender;
use crate::config::TuiConfig;
use crate::error::Result;

/// Events flowing into the TUI event loop.
#[derive(Debug)]
pub enum AppEvent {
    /// Crossterm terminal event (key, mouse, resize).
    Terminal(Event),
    /// Streaming LLM token chunk.
    LlmChunk(String),
    /// LLM response complete with token usage.
    LlmDone {
        input_tokens: u32,
        output_tokens: u32,
    },
    /// User submitted input text (from input widget).
    UserInput(String),
    /// Periodic tick for redraw.
    Tick,
}

/// Shared state for the TUI application.
pub struct AppState {
    /// Conversation messages (role, content).
    pub messages: Vec<ChatMessage>,
    /// Partial message being streamed.
    pub streaming: Option<String>,
    /// Whether we're currently waiting for LLM response.
    pub is_streaming: bool,
    /// Currently focused pane.
    pub focus: FocusTarget,
    /// Status bar variables.
    pub model: String,
    pub session: String,
    pub turn: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Scroll offset for conversation pane.
    pub scroll_offset: u16,
    /// Whether to auto-scroll (true until user scrolls up).
    pub auto_scroll: bool,
    /// Flag to exit the app.
    pub should_quit: bool,
    /// Whether the sidebar is visible.
    pub sidebar_visible: bool,
    /// Data for each configured sidebar pane.
    pub pane_data: Vec<widgets::sidebar::PaneData>,
    /// Status bar template string.
    pub status_template: String,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    Input,
    Conversation,
}

impl AppState {
    pub fn new(model: String, session: String, pane_count: usize, status_template: String) -> Self {
        Self {
            messages: Vec::new(),
            streaming: None,
            is_streaming: false,
            focus: FocusTarget::Input,
            model,
            session,
            turn: 0,
            input_tokens: 0,
            output_tokens: 0,
            scroll_offset: 0,
            auto_scroll: true,
            should_quit: false,
            sidebar_visible: pane_count > 0,
            pane_data: vec![widgets::sidebar::PaneData::Empty; pane_count],
            status_template,
        }
    }

    pub fn total_tokens(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }

    /// Finalize the streaming message into the conversation history.
    pub fn finalize_stream(&mut self) {
        if let Some(content) = self.streaming.take() {
            self.messages.push(ChatMessage {
                role: "assistant".into(),
                content,
            });
        }
        self.is_streaming = false;
        self.auto_scroll = true;
    }

    /// Append a chunk to the streaming buffer.
    pub fn append_chunk(&mut self, chunk: &str) {
        match &mut self.streaming {
            Some(buf) => buf.push_str(chunk),
            None => {
                self.streaming = Some(chunk.to_string());
                self.is_streaming = true;
            }
        }
    }
}

/// Sender half for pushing events into the TUI loop.
pub type AppEventSender = mpsc::UnboundedSender<AppEvent>;
/// Receiver half for the TUI event loop.
pub type AppEventReceiver = mpsc::UnboundedReceiver<AppEvent>;

/// Create a new event channel pair.
pub fn event_channel() -> (AppEventSender, AppEventReceiver) {
    mpsc::unbounded_channel()
}

/// Run the TUI application. Blocks until the user quits.
pub async fn run(
    state: Arc<TokioMutex<AppState>>,
    mut rx: AppEventReceiver,
    input_tx: InputSender,
    config: &TuiConfig,
) -> Result<()> {
    // Enter alternate screen and enable raw mode.
    let mut terminal = ratatui::init();

    // TextArea lives here — owned by the event loop, not shared state.
    let mut textarea = TextArea::default();
    textarea.set_placeholder_text("Type a message...");

    // Command history for up/down navigation.
    let mut history: Vec<String> = Vec::new();
    let mut history_idx: Option<usize> = None;

    let mut event_stream = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(50));

    loop {
        // Draw.
        {
            let state = state.lock().await;
            terminal
                .draw(|frame| ui::draw(frame, &state, &textarea, &config.panes))
                .map_err(|e| crate::error::FamiliarError::Internal {
                    reason: format!("draw error: {e}"),
                })?;
        }

        // Wait for next event.
        tokio::select! {
            // Terminal events (keyboard, mouse, resize).
            Some(Ok(event)) = event_stream.next() => {
                let mut state = state.lock().await;
                handle_terminal_event(
                    &mut state,
                    event,
                    &mut textarea,
                    &input_tx,
                    &mut history,
                    &mut history_idx,
                );
            }
            // App events (LLM chunks, user input, etc).
            Some(event) = rx.recv() => {
                let mut state = state.lock().await;
                match event {
                    AppEvent::LlmChunk(chunk) => {
                        state.append_chunk(&chunk);
                    }
                    AppEvent::LlmDone { input_tokens, output_tokens } => {
                        state.finalize_stream();
                        state.turn += 1;
                        state.input_tokens += input_tokens;
                        state.output_tokens += output_tokens;
                    }
                    AppEvent::UserInput(_) => {
                        // Handled by TuiChannel, not here.
                    }
                    AppEvent::Terminal(event) => {
                        handle_terminal_event(
                            &mut state,
                            event,
                            &mut textarea,
                            &input_tx,
                            &mut history,
                            &mut history_idx,
                        );
                    }
                    AppEvent::Tick => {}
                }
            }
            // Periodic tick.
            _ = tick.tick() => {}
        }

        // Check quit.
        let state = state.lock().await;
        if state.should_quit {
            break;
        }
    }

    // Restore terminal.
    ratatui::restore();
    Ok(())
}

fn handle_terminal_event(
    state: &mut AppState,
    event: Event,
    textarea: &mut TextArea,
    input_tx: &InputSender,
    history: &mut Vec<String>,
    history_idx: &mut Option<usize>,
) {
    let Event::Key(key) = event else {
        return; // Ignore resize/mouse for now
    };

    // Ctrl+C always quits.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.should_quit = true;
        return;
    }

    // Ctrl+B toggles sidebar.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('b') {
        state.sidebar_visible = !state.sidebar_visible;
        return;
    }

    match state.focus {
        FocusTarget::Input => {
            // Tab switches to conversation pane.
            if key.code == KeyCode::Tab {
                state.focus = FocusTarget::Conversation;
                return;
            }

            // Don't accept input while streaming.
            if state.is_streaming {
                return;
            }

            // Enter submits (without shift/ctrl modifier).
            if key.code == KeyCode::Enter
                && !key.modifiers.contains(KeyModifiers::SHIFT)
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
            {
                let lines: Vec<String> = textarea.lines().to_vec();
                let text = lines.join("\n").trim().to_string();

                if !text.is_empty() {
                    // Push to history.
                    history.push(text.clone());
                    *history_idx = None;

                    // Send to conversation engine.
                    let _ = input_tx.send(text);
                }

                // Clear textarea.
                *textarea = TextArea::default();
                textarea.set_placeholder_text("Type a message...");
                return;
            }

            // Up arrow at first line: cycle history backward.
            if key.code == KeyCode::Up && textarea.cursor().0 == 0 {
                if !history.is_empty() {
                    let idx = match *history_idx {
                        Some(i) => i.saturating_sub(1),
                        None => history.len() - 1,
                    };
                    *history_idx = Some(idx);
                    *textarea = TextArea::new(vec![history[idx].clone()]);
                    textarea.set_placeholder_text("Type a message...");
                }
                return;
            }

            // Down arrow at last line: cycle history forward.
            let last_line = textarea.lines().len().saturating_sub(1);
            if key.code == KeyCode::Down && textarea.cursor().0 == last_line {
                if let Some(idx) = *history_idx {
                    if idx + 1 < history.len() {
                        *history_idx = Some(idx + 1);
                        *textarea = TextArea::new(vec![history[idx + 1].clone()]);
                    } else {
                        *history_idx = None;
                        *textarea = TextArea::default();
                    }
                    textarea.set_placeholder_text("Type a message...");
                }
                return;
            }

            // All other keys: forward to textarea.
            textarea.input(event);
        }
        FocusTarget::Conversation => {
            match key.code {
                KeyCode::Tab | KeyCode::Esc => {
                    state.focus = FocusTarget::Input;
                }
                KeyCode::Up => {
                    state.auto_scroll = false;
                    // Cap at message count to prevent unbounded growth.
                    let max_scroll = state.messages.len() as u16;
                    if state.scroll_offset < max_scroll {
                        state.scroll_offset = state.scroll_offset.saturating_add(1);
                    }
                }
                KeyCode::Down => {
                    state.scroll_offset = state.scroll_offset.saturating_sub(1);
                    if state.scroll_offset == 0 {
                        state.auto_scroll = true;
                    }
                }
                _ => {}
            }
        }
    }
}
