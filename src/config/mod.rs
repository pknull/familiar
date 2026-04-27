//! Configuration loading and validation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::error::{FamiliarError, Result};

// Re-export shared config types from thallus-core
pub use thallus_core::config::{LlmConfig, McpServerConfig};

/// Root configuration structure.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub egregore: EgregoreConfig,

    #[serde(default)]
    pub llm: Option<LlmConfig>,

    #[serde(default)]
    pub mcp: HashMap<String, McpServerConfig>,

    #[serde(default)]
    pub agent: AgentConfig,

    #[serde(default)]
    pub store: StoreConfig,

    #[serde(default)]
    pub heartbeat: HeartbeatConfig,

    #[serde(default)]
    pub repl: ReplConfig,

    #[serde(default)]
    pub discord: Option<DiscordConfig>,

    #[serde(default)]
    pub tools: ToolTrustConfig,

    #[serde(default)]
    pub daemon: DaemonConfig,

    #[serde(default)]
    pub tui: TuiConfig,

    #[serde(default)]
    pub operator: OperatorConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EgregoreConfig {
    #[serde(default = "default_egregore_url")]
    pub api_url: String,

    /// Bearer token for egregore API authentication.
    /// Required when egregore has api_auth_enabled = true.
    #[serde(default)]
    pub api_token: Option<String>,
}

impl Default for EgregoreConfig {
    fn default() -> Self {
        Self {
            api_url: default_egregore_url(),
            api_token: None,
        }
    }
}

fn default_egregore_url() -> String {
    "http://127.0.0.1:7654".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Tools blocked by scope policy. Any tool name in this list will be
    /// rejected before execution.
    #[serde(default)]
    pub blocked_tools: Vec<String>,
    /// Allowed tool patterns (glob). When non-empty, only MCP tools matching
    /// a pattern are permitted. Built-in tools (egregore_*, local_*) are
    /// always allowed regardless of this setting.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Token budget before compaction triggers (default: 80,000).
    #[serde(default = "default_compaction_token_budget")]
    pub compaction_token_budget: u64,
    /// Number of recent turns to preserve during compaction (default: 10).
    #[serde(default = "default_preserve_recent_turns")]
    pub preserve_recent_turns: usize,
    /// Public IDs of trusted servitors for auto-assignment. Empty = accept all.
    #[serde(default)]
    pub trusted_servitors: Vec<String>,
    /// Require servitor to have published a matching servitor_profile before accepting offer.
    #[serde(default)]
    pub verify_servitor_profile: bool,
    /// Seconds to wait for task_started after publishing task_assign (default: 30).
    #[serde(default = "default_assign_confirm_timeout")]
    pub assign_confirm_timeout_secs: u64,
    /// Enable background SSE watching in all modes (default: true).
    #[serde(default = "default_background_sse")]
    pub background_sse_enabled: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            timeout_secs: default_timeout(),
            system_prompt: None,
            blocked_tools: Vec::new(),
            allowed_tools: Vec::new(),
            compaction_token_budget: default_compaction_token_budget(),
            preserve_recent_turns: default_preserve_recent_turns(),
            trusted_servitors: Vec::new(),
            verify_servitor_profile: false,
            assign_confirm_timeout_secs: default_assign_confirm_timeout(),
            background_sse_enabled: default_background_sse(),
        }
    }
}

fn default_assign_confirm_timeout() -> u64 {
    30
}

fn default_background_sse() -> bool {
    true
}

fn default_compaction_token_budget() -> u64 {
    80_000
}

fn default_preserve_recent_turns() -> usize {
    10
}

fn default_max_turns() -> u32 {
    20
}

fn default_timeout() -> u64 {
    300
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StoreConfig {
    #[serde(default = "default_store_path")]
    pub path: String,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            path: default_store_path(),
        }
    }
}

fn default_store_path() -> String {
    "~/.familiar/familiar.db".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HeartbeatConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_heartbeat_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_quiet_start")]
    pub quiet_start: u32,
    #[serde(default = "default_quiet_end")]
    pub quiet_end: u32,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_secs: default_heartbeat_interval(),
            quiet_start: default_quiet_start(),
            quiet_end: default_quiet_end(),
        }
    }
}

fn default_heartbeat_interval() -> u64 {
    1800
}

fn default_quiet_start() -> u32 {
    22
}

fn default_quiet_end() -> u32 {
    8
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReplConfig {
    /// Prompt shown for user input.
    #[serde(default = "default_user_prompt")]
    pub user_prompt: String,

    /// Prefix for familiar's responses.
    #[serde(default = "default_familiar_prompt")]
    pub familiar_prompt: String,

    /// Text shown while waiting for LLM response.
    #[serde(default = "default_thinking_text")]
    pub thinking_text: String,
}

impl Default for ReplConfig {
    fn default() -> Self {
        Self {
            user_prompt: default_user_prompt(),
            familiar_prompt: default_familiar_prompt(),
            thinking_text: default_thinking_text(),
        }
    }
}

fn default_user_prompt() -> String {
    "you: ".to_string()
}

fn default_familiar_prompt() -> String {
    "familiar: ".to_string()
}

fn default_thinking_text() -> String {
    "thinking...".to_string()
}

/// TUI operator console configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TuiConfig {
    /// Color theme.
    #[serde(default = "default_tui_theme")]
    pub theme: String,

    /// Use vim keybindings in input widget.
    #[serde(default)]
    pub vim_mode: bool,

    /// Enable mouse support (click focus, scroll).
    #[serde(default = "default_true")]
    pub mouse: bool,

    /// Status bar template with {model}, {session}, {turn}, {tokens} variables.
    #[serde(default = "default_status_template")]
    pub status_template: String,

    /// Configurable sidebar panes.
    #[serde(default)]
    pub panes: Vec<PaneConfig>,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            theme: default_tui_theme(),
            vim_mode: false,
            mouse: true,
            status_template: default_status_template(),
            panes: Vec::new(),
        }
    }
}

fn default_tui_theme() -> String {
    "dark".to_string()
}

fn default_status_template() -> String {
    "{model} | t:{turn} | {tokens}tok | {session}".to_string()
}

/// Configuration for a sidebar pane.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PaneConfig {
    /// Pane data source: egregore_feed, tasks, peers, script.
    pub source: String,

    /// Position in the layout.
    #[serde(default = "default_pane_position")]
    pub position: String,

    /// Height as percentage of sidebar space (e.g. "40%").
    #[serde(default)]
    pub height: Option<String>,

    /// Filter by content type (comma-separated, for feed panes).
    #[serde(default)]
    pub filter_content_type: Option<String>,

    /// External command to run (for script panes).
    #[serde(default)]
    pub command: Option<String>,

    /// Auto-restart script on exit.
    #[serde(default)]
    pub restart: bool,

    /// Poll interval in seconds (for peers pane).
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,

    /// TTL for completed tasks before fading (for task pane).
    #[serde(default)]
    pub completed_ttl_secs: Option<u64>,
}

fn default_pane_position() -> String {
    "right".to_string()
}

/// Discord bot configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiscordConfig {
    /// Environment variable containing bot token.
    pub token_env: String,

    /// Guild IDs to allow (empty = all guilds).
    #[serde(default)]
    pub guild_allowlist: Vec<String>,

    /// Require @mention to respond.
    #[serde(default = "default_true")]
    pub require_mention: bool,
}

fn default_true() -> bool {
    true
}

/// Tool trust configuration — controls which tools get full authority vs suggestion-only.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ToolTrustConfig {
    /// Tools with full system prompt authority. Glob patterns (e.g., "docker:*").
    #[serde(default)]
    pub trusted: Vec<String>,
    /// Tools with suggestion-only authority (disclaimer appended to their output).
    #[serde(default)]
    pub installed: Vec<String>,
}

impl ToolTrustConfig {
    /// Determine the trust level of a tool by name.
    pub fn trust_level(&self, tool_name: &str) -> TrustLevel {
        for pattern in &self.trusted {
            if glob_match(pattern, tool_name) {
                return TrustLevel::Trusted;
            }
        }
        for pattern in &self.installed {
            if glob_match(pattern, tool_name) {
                return TrustLevel::Installed;
            }
        }
        TrustLevel::Installed // Default: suggestion-only for unlisted tools
    }
}

/// Trust level for a tool.
#[derive(Debug, Clone, PartialEq)]
pub enum TrustLevel {
    /// Full authority — tool output injected as-is.
    Trusted,
    /// Suggestion only — disclaimer appended.
    Installed,
}

/// Simple glob matching: "*" at end matches any suffix.
/// Only trailing wildcards are supported (e.g., "docker:*").
/// Mid-string wildcards like "do*cker" are not supported.
pub(crate) fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern.contains('*') {
        let prefix = pattern.trim_end_matches('*');
        name.starts_with(prefix)
    } else {
        name == pattern
    }
}

/// Daemon mode configuration — scope limits for broadcast query handling.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DaemonConfig {
    /// Only respond to queries from these authors (empty = all).
    #[serde(default)]
    pub author_allowlist: Vec<String>,
    /// Only respond to queries with these content types (empty = all).
    #[serde(default)]
    pub content_type_filter: Vec<String>,
    /// Only respond to queries with these tags (empty = all).
    #[serde(default)]
    pub tag_filter: Vec<String>,
}

impl DaemonConfig {
    /// Check if a message matches the daemon's scope filters.
    pub fn matches_scope(
        &self,
        author: Option<&str>,
        content_type: Option<&str>,
        tags: &[String],
    ) -> bool {
        // Author filter
        if !self.author_allowlist.is_empty() {
            if let Some(a) = author {
                if !self.author_allowlist.iter().any(|allowed| allowed == a) {
                    return false;
                }
            } else {
                return false;
            }
        }
        // Content type filter
        if !self.content_type_filter.is_empty() {
            if let Some(ct) = content_type {
                if !self.content_type_filter.iter().any(|f| f == ct) {
                    return false;
                }
            } else {
                return false;
            }
        }
        // Tag filter
        if !self.tag_filter.is_empty() {
            if !self.tag_filter.iter().any(|t| tags.contains(t)) {
                return false;
            }
        }
        true
    }
}

/// Operator configuration — human proxy for task delegation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OperatorConfig {
    /// Human capabilities for task matching (e.g., ["code-review", "approval"]).
    /// Empty = human proxy disabled.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Offer TTL for human work in seconds (default: 3600 = 1 hour).
    #[serde(default = "default_operator_offer_ttl")]
    pub offer_ttl_secs: u64,
}

impl Default for OperatorConfig {
    fn default() -> Self {
        Self {
            capabilities: Vec::new(),
            offer_ttl_secs: default_operator_offer_ttl(),
        }
    }
}

fn default_operator_offer_ttl() -> u64 {
    3600
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| FamiliarError::Config {
            reason: format!("failed to read config file {}: {}", path.display(), e),
        })?;
        let config: Config = toml::from_str(&content).map_err(|e| FamiliarError::Config {
            reason: format!("failed to parse config: {}", e),
        })?;
        Ok(config)
    }

    pub fn expand_path(path: &str) -> String {
        shellexpand::tilde(path).to_string()
    }
}
