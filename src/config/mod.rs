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
    pub identity: IdentityConfig,

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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdentityConfig {
    /// Path to the secret key file (reuses your egregore identity).
    #[serde(default = "default_secret_key")]
    pub secret_key: String,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            secret_key: default_secret_key(),
        }
    }
}

fn default_secret_key() -> String {
    "~/.familiar/secret.key".to_string()
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
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            timeout_secs: default_timeout(),
            system_prompt: None,
            blocked_tools: Vec::new(),
        }
    }
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
