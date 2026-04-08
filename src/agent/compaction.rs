//! Context compaction — summarize conversation history when approaching token limit.
//!
//! Uses the configured LLM provider to distill conversation turns into a structured
//! summary with labeled sections, preserving high-signal context while pruning noise.

use crate::agent::providers::{Message, Provider};
use crate::error::Result;

const COMPACTION_PROMPT: &str = r#"You are compacting a conversation history to save context space.

Produce a structured summary with EXACTLY these sections:

## Current Focus
What the user is actively working on right now (1-3 bullet points).

## Key Decisions
Decisions made during this conversation that affect future behavior (1-5 bullets).

## Learned Facts
Things learned about the user, their preferences, or their environment (1-5 bullets).

## Pending Work
Tasks in progress, commitments, or next steps that haven't been completed (1-5 bullets).

## Tool History
Which tools were used and their outcomes — keep results, drop raw output (1-5 bullets).

Rules:
- Max 5 bullet points per section
- Omit empty sections entirely
- Keep total under 2000 characters
- Discard: greetings, verbatim code, intermediate reasoning, raw tool output
- Preserve: decisions, facts, preferences, commitments, current state"#;

const TITLE_PROMPT: &str = r#"Summarize this conversation in 3-5 words for use as a session name/slug.
Output ONLY the slug words, lowercase, separated by hyphens. No explanation.
Example: "rust-auth-refactor" or "egregore-feed-debugging""#;

/// Maximum characters for a compaction summary.
const MAX_SUMMARY_CHARS: usize = 2000;

/// Maximum lines in a compacted summary.
const MAX_SUMMARY_LINES: usize = 30;

/// Estimate token count from text (rough: ~4 chars per token).
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64) / 4
}

/// Compact a list of conversation turns into a structured summary.
pub async fn compact(
    provider: &dyn Provider,
    turns: &[(String, String)], // (role, content) pairs
    existing_summary: Option<&str>,
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

    let input = if let Some(prev) = existing_summary {
        format!(
            "## Previously Compacted Context\n{}\n\n---\n\n## New Conversation to Compact\n{}",
            prev, history
        )
    } else {
        history
    };

    let messages = vec![Message::user(&input)];
    let response = provider.chat(COMPACTION_PROMPT, &messages, &[]).await?;
    let raw = response.text();

    Ok(compress_summary(&raw))
}

/// Generate a session title slug from the first exchange.
pub async fn generate_title(
    provider: &dyn Provider,
    first_user_message: &str,
    first_assistant_message: &str,
) -> Result<String> {
    let exchange = format!(
        "User: {}\nAssistant: {}",
        first_user_message, first_assistant_message
    );
    let messages = vec![Message::user(&exchange)];
    let response = provider.chat(TITLE_PROMPT, &messages, &[]).await?;

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

/// Compress a summary to fit within budget: dedup lines, cap length and line count.
fn compress_summary(raw: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut lines: Vec<&str> = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            lines.push(line);
            continue;
        }
        if seen.contains(trimmed) {
            continue;
        }
        seen.insert(trimmed);

        // Truncate long lines
        if trimmed.len() > 160 {
            // Find a safe char boundary
            let mut end = 157;
            while end > 0 && !trimmed.is_char_boundary(end) {
                end -= 1;
            }
            lines.push(&trimmed[..end]);
            // We lose the "..." suffix but that's fine for budget control
        } else {
            lines.push(line);
        }

        if lines.len() >= MAX_SUMMARY_LINES {
            break;
        }
    }

    let mut result = lines.join("\n");
    if result.len() > MAX_SUMMARY_CHARS {
        // Hard truncate at char boundary
        let mut end = MAX_SUMMARY_CHARS;
        while end > 0 && !result.is_char_boundary(end) {
            end -= 1;
        }
        result.truncate(end);
        result.push_str("\n[summary truncated]");
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_roughly_correct() {
        // 400 chars ≈ 100 tokens
        let text = "a".repeat(400);
        assert_eq!(estimate_tokens(&text), 100);
    }

    #[test]
    fn compress_deduplicates_lines() {
        let input = "## Focus\n- item 1\n- item 1\n- item 2";
        let result = compress_summary(input);
        assert_eq!(
            result.matches("- item 1").count(),
            1,
            "duplicate not removed"
        );
        assert!(result.contains("- item 2"));
    }

    #[test]
    fn compress_respects_max_lines() {
        let lines: Vec<String> = (0..50).map(|i| format!("line {}", i)).collect();
        let input = lines.join("\n");
        let result = compress_summary(&input);
        assert!(
            result.lines().count() <= MAX_SUMMARY_LINES,
            "too many lines: {}",
            result.lines().count()
        );
    }

    #[test]
    fn compress_respects_max_chars() {
        let long_lines: Vec<String> = (0..100).map(|i| format!("line {} {}", i, "x".repeat(100))).collect();
        let input = long_lines.join("\n");
        let result = compress_summary(&input);
        // Allow for the "[summary truncated]" suffix
        assert!(
            result.len() <= MAX_SUMMARY_CHARS + 30,
            "too long: {}",
            result.len()
        );
    }
}
