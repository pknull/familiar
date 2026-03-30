//! First-run initialization — create config directory and example config.

use std::path::Path;

use crate::error::{FamiliarError, Result};

const EXAMPLE_CONFIG: &str = r#"# Familiar configuration
# See: https://github.com/pknull/familiar

[identity]
# Path to your egregore secret key (reuses your existing identity)
secret_key = "~/egregore-data/secret.key"

[egregore]
api_url = "http://127.0.0.1:7654"

[llm]
provider = "anthropic"
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"

[agent]
max_turns = 20
timeout_secs = 300

[store]
path = "~/.familiar/familiar.db"

# MCP servers for local capabilities
# [mcp.filesystem]
# transport = "stdio"
# command = "npx"
# args = ["@anthropic-ai/mcp-filesystem", "/home/user"]
"#;

/// Initialize Familiar's config directory.
pub fn run_init(config_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(config_dir)?;

    let config_path = config_dir.join("familiar.toml");
    if config_path.exists() {
        return Err(FamiliarError::Config {
            reason: format!(
                "Config already exists at {}. Remove it to reinitialize.",
                config_path.display()
            ),
        });
    }

    std::fs::write(&config_path, EXAMPLE_CONFIG)?;
    tracing::info!(path = %config_path.display(), "created config file");

    println!("Initialized familiar at {}", config_dir.display());
    println!("Edit {} to configure.", config_path.display());

    Ok(())
}
