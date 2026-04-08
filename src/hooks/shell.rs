//! Shell hook adapter — runs external scripts as hooks.
//!
//! The script receives a JSON payload on stdin and writes a JSON response to stdout.
//!
//! Exit codes:
//!   0 = Allow (continue with tool execution)
//!   2 = Deny (block the tool call)
//!   1, 3+ = Hook error (allow with warning)
//!
//! Response format (stdout, optional):
//! ```json
//! {
//!   "reason": "why this was denied",
//!   "updated_input": { ... }
//! }
//! ```

use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{Hook, HookDecision, HookEvent, HookPayload};

/// Default timeout for shell hooks (seconds).
const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// A hook that runs an external shell command.
pub struct ShellHook {
    name: String,
    command: String,
    events: Vec<HookEvent>,
    timeout: Duration,
}

impl ShellHook {
    /// Create a new shell hook.
    pub fn new(
        name: impl Into<String>,
        command: impl Into<String>,
        events: Vec<HookEvent>,
    ) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            events,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Set custom timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait]
impl Hook for ShellHook {
    fn name(&self) -> &str {
        &self.name
    }

    fn events(&self) -> &[HookEvent] {
        &self.events
    }

    async fn on_event(&self, payload: &HookPayload) -> HookDecision {
        let payload_json = match serde_json::to_string(payload) {
            Ok(json) => json,
            Err(e) => {
                tracing::warn!(
                    hook = self.name,
                    error = %e,
                    "failed to serialize hook payload"
                );
                return HookDecision::Allow;
            }
        };

        // Spawn the command
        let mut child = match Command::new("sh")
            .arg("-c")
            .arg(&self.command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                tracing::warn!(
                    hook = self.name,
                    command = self.command,
                    error = %e,
                    "failed to spawn shell hook"
                );
                return HookDecision::Allow;
            }
        };

        // Write payload to stdin
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = stdin.write_all(payload_json.as_bytes()).await {
                tracing::warn!(hook = self.name, error = %e, "failed to write to hook stdin");
                return HookDecision::Allow;
            }
            // Drop stdin to signal EOF
        }

        // Wait with timeout. On timeout, `child` is dropped which sends SIGKILL.
        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => {
                tracing::warn!(hook = self.name, error = %e, "hook process error");
                return HookDecision::Allow;
            }
            Err(_) => {
                tracing::warn!(
                    hook = self.name,
                    timeout_secs = self.timeout.as_secs(),
                    "hook timed out, allowing tool call"
                );
                // child is dropped here, which kills the process
                return HookDecision::Allow;
            }
        };

        let exit_code = output.status.code().unwrap_or(1);

        match exit_code {
            0 => {
                // Allow — check for updated_input in stdout
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Ok(response) = serde_json::from_str::<serde_json::Value>(&stdout) {
                    if let Some(updated) = response.get("updated_input") {
                        return HookDecision::ModifyInput(updated.clone());
                    }
                }
                HookDecision::Allow
            }
            2 => {
                // Deny
                let stdout = String::from_utf8_lossy(&output.stdout);
                let reason = serde_json::from_str::<serde_json::Value>(&stdout)
                    .ok()
                    .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(String::from))
                    .unwrap_or_else(|| "denied by shell hook".to_string());

                tracing::info!(
                    hook = self.name,
                    tool = payload.tool_name,
                    reason = reason,
                    "tool call denied by shell hook"
                );

                HookDecision::Deny { reason }
            }
            code => {
                // Hook error — allow with warning
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(
                    hook = self.name,
                    exit_code = code,
                    stderr = %stderr,
                    "shell hook exited with error, allowing tool call"
                );
                HookDecision::Allow
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allow_hook_returns_allow() {
        let hook = ShellHook::new("test-allow", "cat > /dev/null; exit 0", vec![HookEvent::PreToolUse]);
        let payload = HookPayload {
            event: HookEvent::PreToolUse,
            tool_name: "test".into(),
            tool_input: serde_json::json!({}),
            tool_output: None,
            is_error: false,
        };

        let decision = hook.on_event(&payload).await;
        assert!(matches!(decision, HookDecision::Allow));
    }

    #[tokio::test]
    async fn deny_hook_returns_deny() {
        let hook = ShellHook::new(
            "test-deny",
            r#"cat > /dev/null; echo '{"reason":"nope"}'; exit 2"#,
            vec![HookEvent::PreToolUse],
        );
        let payload = HookPayload {
            event: HookEvent::PreToolUse,
            tool_name: "test".into(),
            tool_input: serde_json::json!({}),
            tool_output: None,
            is_error: false,
        };

        let decision = hook.on_event(&payload).await;
        match decision {
            HookDecision::Deny { reason } => assert_eq!(reason, "nope"),
            _ => panic!("expected Deny"),
        }
    }

    #[tokio::test]
    async fn modify_hook_returns_updated_input() {
        let hook = ShellHook::new(
            "test-modify",
            r#"cat > /dev/null; echo '{"updated_input":{"key":"modified"}}'"#,
            vec![HookEvent::PreToolUse],
        );
        let payload = HookPayload {
            event: HookEvent::PreToolUse,
            tool_name: "test".into(),
            tool_input: serde_json::json!({"key": "original"}),
            tool_output: None,
            is_error: false,
        };

        let decision = hook.on_event(&payload).await;
        match decision {
            HookDecision::ModifyInput(val) => {
                assert_eq!(val, serde_json::json!({"key": "modified"}));
            }
            _ => panic!("expected ModifyInput"),
        }
    }

    #[tokio::test]
    async fn error_hook_allows_with_warning() {
        let hook = ShellHook::new("test-error", "cat > /dev/null; exit 1", vec![HookEvent::PreToolUse]);
        let payload = HookPayload {
            event: HookEvent::PreToolUse,
            tool_name: "test".into(),
            tool_input: serde_json::json!({}),
            tool_output: None,
            is_error: false,
        };

        let decision = hook.on_event(&payload).await;
        assert!(matches!(decision, HookDecision::Allow));
    }

    #[tokio::test]
    async fn timeout_hook_allows() {
        let hook = ShellHook::new("test-timeout", "sleep 30", vec![HookEvent::PreToolUse])
            .with_timeout(Duration::from_millis(100));

        let payload = HookPayload {
            event: HookEvent::PreToolUse,
            tool_name: "test".into(),
            tool_input: serde_json::json!({}),
            tool_output: None,
            is_error: false,
        };

        let decision = hook.on_event(&payload).await;
        assert!(matches!(decision, HookDecision::Allow));
    }
}
