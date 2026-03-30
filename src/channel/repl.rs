//! REPL channel — interactive terminal I/O with thinking indicator.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

use super::{Channel, ChannelMessage};
use crate::config::ReplConfig;
use crate::error::{FamiliarError, Result};

/// REPL channel — reads from stdin, writes to stdout.
/// Blocks input during LLM response and shows a thinking indicator.
pub struct ReplChannel {
    editor: DefaultEditor,
    history_path: std::path::PathBuf,
    config: ReplConfig,
    /// Shared flag: true while the LLM is generating a response.
    responding: Arc<AtomicBool>,
}

impl ReplChannel {
    pub fn new(config: ReplConfig) -> Result<Self> {
        let editor = DefaultEditor::new().map_err(|e| FamiliarError::Internal {
            reason: format!("failed to initialize readline: {}", e),
        })?;

        let history_path = dirs::data_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join("familiar")
            .join("history.txt");

        if let Some(parent) = history_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let mut channel = Self {
            editor,
            history_path,
            config,
            responding: Arc::new(AtomicBool::new(false)),
        };

        let _ = channel.editor.load_history(&channel.history_path);

        Ok(channel)
    }

    /// Show the thinking indicator (spinner on a single line).
    fn show_thinking(&self) {
        use std::io::Write;
        self.responding.store(true, Ordering::SeqCst);
        print!("\r\x1b[2K{}", self.config.thinking_text);
        let _ = std::io::stdout().flush();
    }

    /// Clear the thinking indicator line.
    fn clear_thinking(&self) {
        use std::io::Write;
        print!("\r\x1b[2K");
        let _ = std::io::stdout().flush();
    }

    /// Mark response as complete, re-enable input.
    fn done_responding(&self) {
        self.responding.store(false, Ordering::SeqCst);
    }
}

#[async_trait]
impl Channel for ReplChannel {
    fn name(&self) -> &str {
        "repl"
    }

    async fn next(&mut self) -> Option<ChannelMessage> {
        // Don't accept input while responding
        while self.responding.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        loop {
            match self.editor.readline(&self.config.user_prompt) {
                Ok(line) => {
                    let input = line.trim().to_string();
                    if input.is_empty() {
                        continue;
                    }

                    let _ = self.editor.add_history_entry(&input);

                    // Show thinking indicator immediately
                    self.show_thinking();

                    return Some(ChannelMessage {
                        content: input,
                        sender: "user".to_string(),
                        channel_id: "repl".to_string(),
                    });
                }
                Err(ReadlineError::Interrupted) => {
                    println!("^C");
                    continue;
                }
                Err(ReadlineError::Eof) => {
                    return None;
                }
                Err(e) => {
                    eprintln!("readline error: {}", e);
                    return None;
                }
            }
        }
    }

    async fn respond(&self, text: &str) -> Result<()> {
        self.done_responding();
        if !text.is_empty() {
            println!("{}", text);
        }
        Ok(())
    }

    async fn stream_chunk(&self, chunk: &str) -> Result<()> {
        use std::io::Write;
        // First chunk clears the thinking indicator
        if self.responding.load(Ordering::SeqCst) {
            self.clear_thinking();
            self.done_responding();
        }
        print!("{}", chunk);
        let _ = std::io::stdout().flush();
        Ok(())
    }
}

impl Drop for ReplChannel {
    fn drop(&mut self) {
        let _ = self.editor.save_history(&self.history_path);
    }
}
