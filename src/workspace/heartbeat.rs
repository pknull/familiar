//! HEARTBEAT.md parser — hybrid format with YAML frontmatter triggers
//! and markdown body for LLM-interpreted checks.
//!
//! Format:
//! ```markdown
//! ---
//! triggers:
//!   - match: "content_type=task_result AND status=failed"
//!     action: notify
//!     on: sse
//!   - schedule: daily
//!     action: summarize_insights
//!     on: heartbeat
//! ---
//!
//! # Heartbeat Checklist
//!
//! - Check for failed task attestations in the last hour
//! - Summarize new insights if any were published today
//! ```

use serde::{Deserialize, Serialize};

/// Parsed HEARTBEAT.md contents.
#[derive(Debug, Clone, Default)]
pub struct HeartbeatConfig {
    /// Structured triggers from YAML frontmatter.
    pub triggers: Vec<Trigger>,
    /// Markdown body for LLM-interpreted checks.
    pub checklist: String,
}

/// A structured trigger rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trigger {
    /// Match expression: "field=value AND field=value"
    #[serde(default)]
    pub r#match: Option<String>,
    /// Schedule expression: "daily", "hourly", etc.
    #[serde(default)]
    pub schedule: Option<String>,
    /// Action to take: "notify", "summarize_insights", custom
    pub action: String,
    /// When to evaluate: "sse" (real-time) or "heartbeat" (batched, default)
    #[serde(default = "default_on")]
    pub on: String,
}

fn default_on() -> String {
    "heartbeat".to_string()
}

/// Parsed match condition.
#[derive(Debug, Clone)]
pub struct MatchCondition {
    pub field: String,
    pub value: String,
}

impl Trigger {
    /// Check if this trigger matches an SSE event.
    pub fn matches_event(&self, event_fields: &[(&str, &str)]) -> bool {
        let conditions = match self.parse_conditions() {
            Some(c) => c,
            None => return false,
        };

        conditions.iter().all(|cond| {
            event_fields
                .iter()
                .any(|(k, v)| *k == cond.field && *v == cond.value)
        })
    }

    /// Parse match expression into conditions.
    fn parse_conditions(&self) -> Option<Vec<MatchCondition>> {
        let expr = self.r#match.as_ref()?;
        let conditions: Vec<MatchCondition> = expr
            .split(" AND ")
            .filter_map(|part| {
                let mut kv = part.trim().splitn(2, '=');
                let field = kv.next()?.trim().to_string();
                let value = kv.next()?.trim().to_string();
                Some(MatchCondition { field, value })
            })
            .collect();

        if conditions.is_empty() {
            None
        } else {
            Some(conditions)
        }
    }

    /// Check if this trigger is SSE-driven (real-time).
    pub fn is_sse(&self) -> bool {
        self.on == "sse"
    }

    /// Check if this trigger is heartbeat-driven (batched).
    pub fn is_heartbeat(&self) -> bool {
        self.on == "heartbeat"
    }
}

/// Parse HEARTBEAT.md content into structured config.
pub fn parse(content: &str) -> HeartbeatConfig {
    // Check for YAML frontmatter
    if content.starts_with("---") {
        if let Some(end) = content[3..].find("---") {
            let yaml = &content[3..3 + end].trim();
            let body = content[3 + end + 3..].trim().to_string();

            // Parse YAML frontmatter
            let triggers = parse_yaml_triggers(yaml);
            return HeartbeatConfig {
                triggers,
                checklist: body,
            };
        }
    }

    // No frontmatter — entire content is the checklist
    HeartbeatConfig {
        triggers: Vec::new(),
        checklist: content.to_string(),
    }
}

/// Parse YAML triggers section.
fn parse_yaml_triggers(yaml: &str) -> Vec<Trigger> {
    #[derive(Deserialize)]
    struct YamlFrontmatter {
        #[serde(default)]
        triggers: Vec<Trigger>,
    }

    match serde_yaml::from_str::<YamlFrontmatter>(yaml) {
        Ok(fm) => fm.triggers,
        Err(e) => {
            tracing::warn!(error = %e, "failed to parse HEARTBEAT.md frontmatter");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hybrid_format() {
        let content = r#"---
triggers:
  - match: "content_type=task_result AND status=failed"
    action: notify
    on: sse
  - schedule: daily
    action: summarize
    on: heartbeat
---

# Checklist

- Check for failures
- Summarize new insights
"#;

        let config = parse(content);
        assert_eq!(config.triggers.len(), 2);
        assert!(config.triggers[0].is_sse());
        assert!(config.triggers[1].is_heartbeat());
        assert!(config.checklist.contains("Check for failures"));
    }

    #[test]
    fn parse_plain_markdown() {
        let content = "# Checklist\n\n- Check something\n";
        let config = parse(content);
        assert!(config.triggers.is_empty());
        assert!(config.checklist.contains("Check something"));
    }

    #[test]
    fn trigger_matches_event() {
        let trigger = Trigger {
            r#match: Some("content_type=task_result AND status=failed".to_string()),
            schedule: None,
            action: "notify".to_string(),
            on: "sse".to_string(),
        };

        let event = vec![("content_type", "task_result"), ("status", "failed")];
        assert!(trigger.matches_event(&event));

        let non_match = vec![("content_type", "insight"), ("status", "ok")];
        assert!(!trigger.matches_event(&non_match));
    }

    #[test]
    fn trigger_partial_match_fails() {
        let trigger = Trigger {
            r#match: Some("content_type=task_result AND status=failed".to_string()),
            schedule: None,
            action: "notify".to_string(),
            on: "sse".to_string(),
        };

        // Only one condition met
        let partial = vec![("content_type", "task_result"), ("status", "ok")];
        assert!(!trigger.matches_event(&partial));
    }
}
