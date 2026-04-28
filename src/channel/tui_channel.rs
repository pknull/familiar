//! TUI channel — bridges the Channel trait with the ratatui event loop.
//!
//! The TuiChannel sends LLM output as AppEvents into the TUI event loop
//! and receives user input via a separate mpsc channel.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, Mutex as TokioMutex};

use super::{Channel, ChannelMessage, TextCallback};
use crate::error::Result;
use crate::tui::{AppEvent, AppEventSender, AppState, ChatMessage};

/// TUI channel — renders conversation through the ratatui TUI.
pub struct TuiChannel {
    /// Sender for pushing events into the TUI event loop.
    event_tx: AppEventSender,
    /// Receiver for user input from the TUI input widget.
    input_rx: mpsc::UnboundedReceiver<String>,
    /// Shared app state for direct mutations (e.g., adding user messages).
    state: Arc<TokioMutex<AppState>>,
}

/// Sender half for the input widget to push submitted text.
pub type InputSender = mpsc::UnboundedSender<String>;

impl TuiChannel {
    pub fn new(event_tx: AppEventSender, state: Arc<TokioMutex<AppState>>) -> (Self, InputSender) {
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let channel = Self {
            event_tx,
            input_rx,
            state,
        };
        (channel, input_tx)
    }
}

#[async_trait]
impl Channel for TuiChannel {
    fn name(&self) -> &str {
        "tui"
    }

    async fn next(&mut self) -> Option<ChannelMessage> {
        // Wait for user to submit text via the TUI input widget.
        let input = self.input_rx.recv().await?;

        // Add the user message to app state.
        {
            let mut state = self.state.lock().await;
            state.messages.push(ChatMessage {
                role: "user".into(),
                content: input.clone(),
            });
            state.is_streaming = true;
        }

        Some(ChannelMessage {
            content: input,
            sender: "user".to_string(),
            channel_id: "tui".to_string(),
        })
    }

    async fn respond(&self, text: &str) -> Result<()> {
        // Finalize any streaming content and add the complete response.
        let _ = self.event_tx.send(AppEvent::LlmDone {
            input_tokens: 0,
            output_tokens: 0,
        });

        // If there was no streaming, add the response directly.
        let mut state = self.state.lock().await;
        if state.streaming.is_none() && !text.is_empty() {
            state.messages.push(ChatMessage {
                role: "assistant".into(),
                content: text.to_string(),
            });
        }
        state.is_streaming = false;

        Ok(())
    }

    async fn respond_error(&self, text: &str) -> Result<()> {
        // Always surface the error as a message, even if streaming had
        // started. Finalize the partial assistant content SYNCHRONOUSLY
        // (under the same state lock) before pushing the error, otherwise
        // the LlmDone event is queued for the TUI loop and could be
        // processed AFTER our error push — visually reordering them.
        let _ = self.event_tx.send(AppEvent::LlmDone {
            input_tokens: 0,
            output_tokens: 0,
        });

        let mut state = self.state.lock().await;
        // Move any partial streaming content to messages first; if there
        // was none, this is a no-op.
        state.finalize_stream();
        if !text.is_empty() {
            state.messages.push(ChatMessage {
                role: "assistant".into(),
                content: text.to_string(),
            });
        }
        state.is_streaming = false;

        Ok(())
    }

    async fn stream_chunk(&self, chunk: &str) -> Result<()> {
        let _ = self.event_tx.send(AppEvent::LlmChunk(chunk.to_string()));
        Ok(())
    }

    fn stream_callback(&self) -> Option<TextCallback> {
        // Clone the unbounded sender into the closure; mpsc::UnboundedSender
        // is Clone + Send + Sync, so the resulting Arc<dyn Fn> is sound.
        let tx = self.event_tx.clone();
        Some(Arc::new(move |chunk: &str| {
            let _ = tx.send(AppEvent::LlmChunk(chunk.to_string()));
        }))
    }
}
