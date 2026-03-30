//! Discord channel — talk to Familiar via Discord bot.
//!
//! Unlike servitor's Discord transport, this is purely conversational.
//! No authority checks, no task conversion. Just chat.

use std::sync::Arc;

use async_trait::async_trait;
use serenity::all::{
    ChannelId, Client, Context, CreateMessage, EventHandler, GatewayIntents, Http, Message,
    MessageId, Ready, UserId,
};
use tokio::sync::{mpsc, RwLock};

use super::{Channel, ChannelMessage};
use crate::config::DiscordConfig;
use crate::error::{FamiliarError, Result};

/// Discord channel — receives messages from Discord, sends responses back.
pub struct DiscordChannel {
    rx: mpsc::Receiver<(ChannelMessage, DiscordResponder)>,
    current_responder: Option<DiscordResponder>,
    _client_handle: tokio::task::JoinHandle<()>,
}

/// Holds the context needed to reply to a Discord message.
#[derive(Clone)]
struct DiscordResponder {
    http: Arc<Http>,
    channel_id: ChannelId,
    message_id: MessageId,
}

impl DiscordChannel {
    pub async fn new(config: &DiscordConfig) -> Result<Self> {
        let token = std::env::var(&config.token_env).map_err(|_| FamiliarError::Config {
            reason: format!("environment variable {} not set", config.token_env),
        })?;

        let (tx, rx) = mpsc::channel(100);

        let handler = FamiliarDiscordHandler {
            tx,
            config: config.clone(),
            bot_id: Arc::new(RwLock::new(None)),
        };

        let intents = GatewayIntents::GUILDS
            | GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;

        let mut client = Client::builder(&token, intents)
            .event_handler(handler)
            .await
            .map_err(|e| FamiliarError::Config {
                reason: format!("failed to create Discord client: {}", e),
            })?;

        let handle = tokio::spawn(async move {
            if let Err(e) = client.start().await {
                tracing::error!(error = %e, "Discord client error");
            }
        });

        tracing::info!("Discord channel connected");

        Ok(Self {
            rx,
            current_responder: None,
            _client_handle: handle,
        })
    }
}

#[async_trait]
impl Channel for DiscordChannel {
    fn name(&self) -> &str {
        "discord"
    }

    async fn next(&mut self) -> Option<ChannelMessage> {
        let (msg, responder) = self.rx.recv().await?;
        self.current_responder = Some(responder);
        Some(msg)
    }

    async fn respond(&self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        if let Some(ref responder) = self.current_responder {
            let chunks = split_message(text, 2000);
            for (i, chunk) in chunks.iter().enumerate() {
                let mut builder = CreateMessage::new().content(chunk);
                if i == 0 {
                    builder =
                        builder.reference_message((responder.channel_id, responder.message_id));
                }
                if let Err(e) = responder
                    .channel_id
                    .send_message(&responder.http, builder)
                    .await
                {
                    tracing::error!(error = %e, "failed to send Discord response");
                }
            }
        }
        Ok(())
    }

    async fn stream_chunk(&self, _chunk: &str) -> Result<()> {
        // Discord doesn't support real-time streaming.
        // Chunks accumulate and send on respond().
        // For now, no-op. Could edit a message in-place for streaming UX later.
        Ok(())
    }
}

/// Serenity event handler for Familiar.
struct FamiliarDiscordHandler {
    tx: mpsc::Sender<(ChannelMessage, DiscordResponder)>,
    config: DiscordConfig,
    bot_id: Arc<RwLock<Option<UserId>>>,
}

#[async_trait]
impl EventHandler for FamiliarDiscordHandler {
    async fn ready(&self, _ctx: Context, ready: Ready) {
        tracing::info!(bot = %ready.user.name, "Discord bot ready");
        let mut bot_id = self.bot_id.write().await;
        *bot_id = Some(ready.user.id);
    }

    async fn message(&self, ctx: Context, msg: Message) {
        // Ignore bots
        if msg.author.bot {
            return;
        }

        // Guild allowlist
        if let Some(guild_id) = msg.guild_id {
            if !self.config.guild_allowlist.is_empty()
                && !self.config.guild_allowlist.contains(&guild_id.to_string())
            {
                return;
            }
        }

        // Get bot ID
        let bot_id = {
            let id = self.bot_id.read().await;
            match *id {
                Some(id) => id,
                None => return,
            }
        };

        // Mention requirement
        let is_mentioned = msg.mentions_user_id(bot_id);
        let is_dm = msg.guild_id.is_none();

        if self.config.require_mention && !is_mentioned && !is_dm {
            return;
        }

        // Strip bot mention from content
        let content = if is_mentioned {
            msg.content
                .replace(&format!("<@{}>", bot_id), "")
                .replace(&format!("<@!{}>", bot_id), "")
                .trim()
                .to_string()
        } else {
            msg.content.clone()
        };

        if content.is_empty() {
            return;
        }

        let channel_msg = ChannelMessage {
            content,
            sender: msg.author.name.clone(),
            channel_id: format!("discord:{}", msg.channel_id),
        };

        let responder = DiscordResponder {
            http: ctx.http.clone(),
            channel_id: msg.channel_id,
            message_id: msg.id,
        };

        if let Err(e) = self.tx.send((channel_msg, responder)).await {
            tracing::error!(error = %e, "failed to forward Discord message");
        }
    }
}

/// Split long messages to respect Discord's 2000 char limit.
fn split_message(content: &str, max_len: usize) -> Vec<String> {
    if content.len() <= max_len {
        return vec![content.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in content.lines() {
        if current.len() + line.len() + 1 > max_len {
            if !current.is_empty() {
                chunks.push(current);
                current = String::new();
            }
            if line.len() > max_len {
                let mut remaining = line;
                while remaining.len() > max_len {
                    chunks.push(remaining[..max_len].to_string());
                    remaining = &remaining[max_len..];
                }
                if !remaining.is_empty() {
                    current = remaining.to_string();
                }
            } else {
                current = line.to_string();
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}
