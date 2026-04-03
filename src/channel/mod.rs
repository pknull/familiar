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
}
