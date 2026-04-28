//! Interactive session — drives a Channel + Conversation pair.
//!
//! All channel-specific UX (terminal chrome, streaming render, banners) lives
//! in the channel implementations. This driver is transport-agnostic: it
//! orchestrates `next` → `conversation.send` → `respond`, asks the channel
//! for a streaming callback, and surfaces session-start / session-end
//! banners through optional channel hooks (default no-op for non-REPL
//! surfaces).

use crate::agent::conversation::Conversation;
use crate::channel::Channel;
use crate::config::ReplConfig;
use crate::error::Result;

/// Run an interactive session over any channel.
pub async fn run_session(
    mut channel: Box<dyn Channel>,
    conversation: &mut Conversation,
    _repl_config: &ReplConfig,
) -> Result<()> {
    let _ = channel.session_banner(&format!(
        "familiar v{}\nType /quit to exit, /context to show saved context.\n",
        env!("CARGO_PKG_VERSION")
    ));

    while let Some(msg) = channel.next().await {
        let input = msg.content.trim();

        // Handle commands
        if input.starts_with('/') {
            match input {
                "/quit" | "/exit" | "/q" => {
                    // Channels (e.g. REPL) may have set a thinking
                    // indicator inside next(); calling respond("") clears
                    // it so the goodbye banner doesn't print into a
                    // dirty line.
                    let _ = channel.respond("").await;
                    break;
                }
                "/context" => {
                    match conversation.list_context() {
                        Ok(pairs) => {
                            if pairs.is_empty() {
                                let _ = channel.respond("(no saved context)").await;
                            } else {
                                let formatted: String = pairs
                                    .iter()
                                    .map(|(k, v)| format!("  {} = {}", k, v))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                let _ = channel.respond(&formatted).await;
                            }
                        }
                        Err(e) => {
                            let _ = channel.respond(&format!("error: {}", e)).await;
                        }
                    }
                    continue;
                }
                "/cost" => {
                    match conversation.cost_summary() {
                        Ok((daily, total, input_tok, output_tok)) => {
                            let formatted = format!(
                                "  Today:    ${:.4}\n  All-time: ${:.4}\n  Tokens:   {} input, {} output",
                                daily, total, input_tok, output_tok
                            );
                            let _ = channel.respond(&formatted).await;
                        }
                        Err(e) => {
                            let _ = channel.respond(&format!("error: {}", e)).await;
                        }
                    }
                    continue;
                }
                "/help" => {
                    let _ = channel
                        .respond("Commands:\n  /quit     Exit familiar\n  /context  Show saved personal context\n  /cost     Show token usage and cost\n  /fork     Fork current session at last turn\n  /help     Show this help")
                        .await;
                    continue;
                }
                cmd if cmd.starts_with("/fork") => {
                    match conversation.fork_session(i64::MAX, "forked") {
                        Ok(Some(new_id)) => {
                            let _ = channel
                                .respond(&format!("Session forked: {}", new_id))
                                .await;
                        }
                        Ok(None) => {
                            let _ = channel.respond("No active session to fork.").await;
                        }
                        Err(e) => {
                            let _ = channel.respond(&format!("Fork failed: {}", e)).await;
                        }
                    }
                    continue;
                }
                _ => {
                    let _ = channel
                        .respond(&format!(
                            "Unknown command: {}. Type /help for commands.",
                            input
                        ))
                        .await;
                    continue;
                }
            }
        }

        // Ask the channel for its streaming callback. Each channel decides
        // how chunks render (REPL: stdout with first-chunk ANSI clear + prompt
        // prefix; TUI: mpsc events into the TUI loop; Discord: noop, but the
        // provider still uses its streaming endpoint for latency).
        let stream_cb = channel.stream_callback();

        match conversation.send(input, stream_cb).await {
            Ok((response_text, _usage)) => {
                // Hand the canonical full response to the channel. Each
                // channel's respond() decides whether to render the text
                // (no streaming happened) or just finalize without
                // duplicating chunks already shown.
                let _ = channel.respond(&response_text).await;
            }
            Err(e) => {
                let _ = channel.respond_error(&format!("error: {}", e)).await;
            }
        }
    }

    let _ = channel.session_goodbye("goodbye.");
    Ok(())
}
