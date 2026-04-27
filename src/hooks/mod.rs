//! Pre/post tool-use hook system.
//!
//! Hooks run around tool execution in the conversation loop, enabling:
//! - Audit logging (who called what, when)
//! - User confirmation gates (block dangerous tools)
//! - Input mutation (rewrite tool inputs before execution)
//!
//! Two hook types are supported:
//! - **Trait hooks** — implement `Hook` in Rust for in-process hooks
//! - **Shell hooks** — external scripts receiving JSON on stdin

pub mod shell;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// When a hook fires relative to tool execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
}

/// Data passed to a hook.
#[derive(Debug, Clone, Serialize)]
pub struct HookPayload {
    pub event: HookEvent,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    /// Only present for `PostToolUse`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<String>,
    /// Only present for `PostToolUse`.
    pub is_error: bool,
}

/// Decision returned by a pre-tool-use hook.
#[derive(Debug, Clone)]
pub enum HookDecision {
    /// Allow the tool call to proceed.
    Allow,
    /// Block the tool call with a reason (returned to the LLM as an error).
    Deny { reason: String },
    /// Allow but use modified input instead.
    ModifyInput(serde_json::Value),
}

/// Hook trait — implement for custom in-process hooks.
#[async_trait]
pub trait Hook: Send + Sync {
    /// Hook identifier for logging.
    fn name(&self) -> &str;

    /// Which events this hook handles.
    fn events(&self) -> &[HookEvent];

    /// Process a hook event and return a decision.
    ///
    /// For `PostToolUse`, the return value is ignored (fire-and-forget).
    async fn on_event(&self, payload: &HookPayload) -> HookDecision;
}

/// Runs registered hooks around tool execution.
pub struct HookRunner {
    hooks: Vec<Box<dyn Hook>>,
}

impl HookRunner {
    /// Create an empty hook runner.
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Register a hook.
    pub fn add(&mut self, hook: Box<dyn Hook>) {
        self.hooks.push(hook);
    }

    /// True if no hooks are registered.
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    /// Run pre-tool-use hooks. Returns the final decision.
    ///
    /// Hooks run in registration order. First `Deny` or `ModifyInput` wins.
    /// If all return `Allow`, the tool proceeds with original input.
    pub async fn run_pre(&self, tool_name: &str, tool_input: &serde_json::Value) -> HookDecision {
        let payload = HookPayload {
            event: HookEvent::PreToolUse,
            tool_name: tool_name.to_string(),
            tool_input: tool_input.clone(),
            tool_output: None,
            is_error: false,
        };

        for hook in &self.hooks {
            if !hook.events().contains(&HookEvent::PreToolUse) {
                continue;
            }

            match hook.on_event(&payload).await {
                HookDecision::Allow => continue,
                decision @ HookDecision::Deny { .. } | decision @ HookDecision::ModifyInput(_) => {
                    tracing::debug!(
                        hook = hook.name(),
                        tool = tool_name,
                        "pre-tool-use hook returned non-allow decision"
                    );
                    return decision;
                }
            }
        }

        HookDecision::Allow
    }

    /// Run post-tool-use hooks (fire-and-forget, decisions are ignored).
    pub async fn run_post(
        &self,
        tool_name: &str,
        tool_input: &serde_json::Value,
        tool_output: &str,
        is_error: bool,
    ) {
        let payload = HookPayload {
            event: HookEvent::PostToolUse,
            tool_name: tool_name.to_string(),
            tool_input: tool_input.clone(),
            tool_output: Some(tool_output.to_string()),
            is_error,
        };

        for hook in &self.hooks {
            if !hook.events().contains(&HookEvent::PostToolUse) {
                continue;
            }

            // Fire-and-forget: log errors but don't propagate
            let _ = hook.on_event(&payload).await;
        }
    }
}

impl Default for HookRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DenyHook;

    #[async_trait]
    impl Hook for DenyHook {
        fn name(&self) -> &str {
            "deny-all"
        }
        fn events(&self) -> &[HookEvent] {
            &[HookEvent::PreToolUse]
        }
        async fn on_event(&self, _payload: &HookPayload) -> HookDecision {
            HookDecision::Deny {
                reason: "blocked by test hook".into(),
            }
        }
    }

    struct ModifyHook;

    #[async_trait]
    impl Hook for ModifyHook {
        fn name(&self) -> &str {
            "modify"
        }
        fn events(&self) -> &[HookEvent] {
            &[HookEvent::PreToolUse]
        }
        async fn on_event(&self, _payload: &HookPayload) -> HookDecision {
            HookDecision::ModifyInput(serde_json::json!({"modified": true}))
        }
    }

    struct AllowHook;

    #[async_trait]
    impl Hook for AllowHook {
        fn name(&self) -> &str {
            "allow"
        }
        fn events(&self) -> &[HookEvent] {
            &[HookEvent::PreToolUse, HookEvent::PostToolUse]
        }
        async fn on_event(&self, _payload: &HookPayload) -> HookDecision {
            HookDecision::Allow
        }
    }

    #[tokio::test]
    async fn deny_hook_blocks_tool() {
        let mut runner = HookRunner::new();
        runner.add(Box::new(DenyHook));

        let decision = runner
            .run_pre("dangerous_tool", &serde_json::json!({}))
            .await;

        assert!(matches!(decision, HookDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn modify_hook_changes_input() {
        let mut runner = HookRunner::new();
        runner.add(Box::new(ModifyHook));

        let decision = runner
            .run_pre("some_tool", &serde_json::json!({"original": true}))
            .await;

        match decision {
            HookDecision::ModifyInput(val) => {
                assert_eq!(val, serde_json::json!({"modified": true}));
            }
            _ => panic!("expected ModifyInput"),
        }
    }

    #[tokio::test]
    async fn allow_hooks_pass_through() {
        let mut runner = HookRunner::new();
        runner.add(Box::new(AllowHook));

        let decision = runner.run_pre("safe_tool", &serde_json::json!({})).await;

        assert!(matches!(decision, HookDecision::Allow));
    }

    #[tokio::test]
    async fn first_deny_wins() {
        let mut runner = HookRunner::new();
        runner.add(Box::new(AllowHook));
        runner.add(Box::new(DenyHook));
        runner.add(Box::new(ModifyHook)); // should never run

        let decision = runner.run_pre("tool", &serde_json::json!({})).await;

        assert!(matches!(decision, HookDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn empty_runner_allows() {
        let runner = HookRunner::new();
        let decision = runner.run_pre("tool", &serde_json::json!({})).await;

        assert!(matches!(decision, HookDecision::Allow));
    }

    #[tokio::test]
    async fn post_hooks_fire_and_forget() {
        let mut runner = HookRunner::new();
        runner.add(Box::new(AllowHook));

        // Should not panic even though DenyHook returns Deny for post events
        runner
            .run_post("tool", &serde_json::json!({}), "output", false)
            .await;
    }
}
