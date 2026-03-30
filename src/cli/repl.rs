//! Interactive session — drives a Channel + Conversation pair.

use crate::agent::conversation::Conversation;
use crate::channel::Channel;
use crate::config::ReplConfig;
use crate::error::Result;

/// Run an interactive session over any channel.
pub async fn run_session(
    mut channel: Box<dyn Channel>,
    conversation: &Conversation,
    repl_config: &ReplConfig,
) -> Result<()> {
    println!("familiar v{}", env!("CARGO_PKG_VERSION"));
    println!("Type /quit to exit, /context to show saved context.\n");

    while let Some(msg) = channel.next().await {
        let input = msg.content.trim();

        // Handle commands — clear thinking indicator first
        if input.starts_with('/') {
            let _ = channel.respond("").await; // clears thinking state
            match input {
                "/quit" | "/exit" | "/q" => break,
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
                "/help" => {
                    let _ = channel
                        .respond("Commands:\n  /quit     Exit familiar\n  /context  Show saved personal context\n  /help     Show this help")
                        .await;
                    continue;
                }
                _ => {
                    let _ = channel
                        .respond(&format!("Unknown command: {}. Type /help for commands.", input))
                        .await;
                    continue;
                }
            }
        }

        // The thinking indicator is already showing from channel.next().
        // When the first LLM chunk arrives, stream_chunk clears it and
        // prints the familiar prompt prefix + chunk.
        let prefix = repl_config.familiar_prompt.clone();
        let first_chunk = std::sync::atomic::AtomicBool::new(true);

        match conversation
            .send(input, Some(&|chunk| {
                use std::io::Write;
                if first_chunk.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    // First chunk: clear thinking, print prefix
                    print!("\r\x1b[2K\n{}{}", prefix, chunk);
                } else {
                    print!("{}", chunk);
                }
                let _ = std::io::stdout().flush();
            }))
            .await
        {
            Ok(_) => {
                // If we never got a chunk, clear thinking anyway
                if first_chunk.load(std::sync::atomic::Ordering::SeqCst) {
                    print!("\r\x1b[2K");
                }
                let _ = channel.respond("\n").await;
            }
            Err(e) => {
                print!("\r\x1b[2K");
                let _ = channel.respond(&format!("\nerror: {}\n", e)).await;
            }
        }
    }

    println!("goodbye.");
    Ok(())
}
