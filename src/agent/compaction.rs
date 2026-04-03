//! Context compaction — summarize conversation history when approaching token limit.
//!
//! Uses the configured LLM provider (with optional [compaction] model override)
//! to distill conversation turns into a concise summary, preserving key facts
//! while pruning tool output and repetition.

use crate::agent::providers::{Message, Provider};
use crate::error::Result;

const COMPACTION_PROMPT: &str = r#"You are compacting a conversation history to save context space.

Produce a concise summary that preserves:
- Key decisions made
- Important facts learned about the user
- Current task state and progress
- Any commitments or next steps

Discard:
- Tool call details (keep outcomes, drop raw output)
- Repeated greetings or pleasantries
- Intermediate reasoning that led to a final answer
- Verbatim code blocks (keep file names and what changed)

Output a summary in bullet points, max 500 words."#;

const TITLE_PROMPT: &str = r#"Summarize this conversation in 3-5 words for use as a session name/slug.
Output ONLY the slug words, lowercase, separated by hyphens. No explanation.
Example: "rust-auth-refactor" or "egregore-feed-debugging""#;

/// Compact a list of conversation turns into a summary.
pub async fn compact(
    provider: &dyn Provider,
    turns: &[(String, String)], // (role, content) pairs
) -> Result<String> {
    let history: String = turns
        .iter()
        .map(|(role, content)| {
            let truncated = if content.len() > 500 {
                let safe_end: String = content.chars().take(500).collect();
                format!("{}... [truncated]", safe_end)
            } else {
                content.clone()
            };
            format!("{}: {}", role, truncated)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let messages = vec![Message::user(&history)];
    let response = provider.chat(COMPACTION_PROMPT, &messages, &[]).await?;
    Ok(response.text())
}

/// Generate a session title slug from the first exchange.
pub async fn generate_title(
    provider: &dyn Provider,
    first_user_message: &str,
    first_assistant_message: &str,
) -> Result<String> {
    let exchange = format!("User: {}\nAssistant: {}", first_user_message, first_assistant_message);
    let messages = vec![Message::user(&exchange)];
    let response = provider.chat(TITLE_PROMPT, &messages, &[]).await?;

    // Clean the response: lowercase, hyphens only, max 40 chars
    let slug = response
        .text()
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");

    let slug: String = slug.chars().take(40).collect();
    let slug = slug.trim_end_matches('-').to_string();

    Ok(if slug.is_empty() {
        "untitled".to_string()
    } else {
        slug
    })
}
