//! Channel abstraction — transport-agnostic I/O for Familiar.
//!
//! Channels handle how messages get in and out. The Conversation engine
//! doesn't care if you're typing in a terminal, messaging on Discord,
//! or hitting an HTTP endpoint.

pub mod discord;
pub mod repl;
pub mod tui_channel;

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::Result;

/// Sync callback invoked by the LLM provider on each streamed text chunk.
///
/// Channels construct one of these per response (`Channel::stream_callback`)
/// to decide how partial text is rendered. The provider-side streaming path
/// is engaged whenever this is `Some(_)`; passing `None` falls back to the
/// non-streaming chat call. Discord, which has no live-streaming surface,
/// returns `Some(noop)` so the provider streaming path is preserved (lower
/// time-to-first-byte) without producing visible chunk output.
pub type TextCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// An incoming message from any channel.
#[derive(Debug, Clone)]
pub struct ChannelMessage {
    /// The message content.
    pub content: String,
    /// Display name of the sender.
    pub sender: String,
    /// Channel-specific identifier (e.g., "repl", "discord:guild:channel").
    pub channel_id: String,
}

/// A network task presented to the operator for accept/reject.
#[derive(Debug, Clone)]
pub struct TaskPresentation {
    /// Hash of the task message (correlation key).
    pub task_hash: String,
    /// Public ID of the task requestor.
    pub requestor: String,
    /// Task prompt / description.
    pub prompt: String,
    /// Required capabilities.
    pub required_caps: Vec<String>,
    /// Task timeout in seconds (if specified).
    pub timeout_secs: Option<u64>,
}

/// Operator's decision on a presented task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskDecision {
    Accept,
    Reject,
}

/// Channel trait — abstracts I/O for different transports.
///
/// Implement this for REPL, Discord, Slack, HTTP, Matrix, etc.
/// The Conversation engine calls none of these methods directly —
/// the driver loop mediates between Channel and Conversation.
#[async_trait]
pub trait Channel: Send {
    /// Channel name for logging.
    fn name(&self) -> &str;

    /// Wait for the next message. Returns None when the channel closes.
    async fn next(&mut self) -> Option<ChannelMessage>;

    /// Send a final response back through the channel.
    ///
    /// Driver passes the canonical full response text after
    /// `conversation.send`. Each implementation decides whether to render
    /// (no streaming happened), suppress (streaming already showed it), or
    /// finalize a streaming surface.
    async fn respond(&self, text: &str) -> Result<()>;

    /// Send an error message that must always be rendered, regardless of
    /// whether streaming was in progress when the error occurred. Channels
    /// that suppress `respond` text after streaming must override this so
    /// the error is still visible.
    async fn respond_error(&self, text: &str) -> Result<()>;

    /// Send a streaming text chunk (partial response).
    ///
    /// Legacy hook retained for compatibility; the canonical streaming path is
    /// `stream_callback`, which produces a sync `TextCallback` the LLM
    /// provider can invoke directly. New channel implementations should
    /// prefer `stream_callback` and treat this method as a no-op.
    async fn stream_chunk(&self, chunk: &str) -> Result<()>;

    /// Construct a fresh streaming callback for one response.
    ///
    /// Returning `Some(_)` engages the provider's streaming path (lower
    /// time-to-first-byte) and lets the channel decide how chunks render.
    /// Returning `None` uses the non-streaming chat call. Channels with no
    /// streaming UX but that still want streaming for latency should return
    /// `Some(noop_callback)`. The closure is created fresh per call so any
    /// per-response state (e.g. "first chunk") lives on the closure, not on
    /// the channel itself.
    fn stream_callback(&self) -> Option<TextCallback> {
        None
    }

    /// Optional one-shot session-start banner. Default no-op. REPL prints to
    /// stdout; TUI/Discord ignore (those surfaces aren't a place for chrome).
    /// Sync because every current implementation is either println or a no-op;
    /// a future async-banner channel can spawn from inside.
    fn session_banner(&self, _text: &str) -> Result<()> {
        Ok(())
    }

    /// Optional one-shot session-end banner. Default no-op.
    fn session_goodbye(&self, _text: &str) -> Result<()> {
        Ok(())
    }

    /// Present a network task to the operator for accept/reject decision.
    ///
    /// Returns `Some(decision)` if the channel supports task presentation,
    /// or `None` if the channel is headless (daemon mode).
    async fn present_task(&self, _task: &TaskPresentation) -> Option<TaskDecision> {
        None // Default: headless, no operator interaction
    }
}
