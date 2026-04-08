//! Channel abstraction — transport-agnostic I/O for Familiar.
//!
//! Channels handle how messages get in and out. The Conversation engine
//! doesn't care if you're typing in a terminal, messaging on Discord,
//! or hitting an HTTP endpoint.

pub mod discord;
pub mod repl;
pub mod tui_channel;

use async_trait::async_trait;

use crate::error::Result;

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
    async fn respond(&self, text: &str) -> Result<()>;

    /// Send a streaming text chunk (partial response).
    /// For REPL this prints immediately. For Discord this might edit a message.
    async fn stream_chunk(&self, chunk: &str) -> Result<()>;

    /// Present a network task to the operator for accept/reject decision.
    ///
    /// Returns `Some(decision)` if the channel supports task presentation,
    /// or `None` if the channel is headless (daemon mode).
    async fn present_task(&self, _task: &TaskPresentation) -> Option<TaskDecision> {
        None // Default: headless, no operator interaction
    }
}
