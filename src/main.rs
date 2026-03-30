mod agent;
mod channel;
mod cli;
mod config;
mod daemon;
mod egregore;
mod error;
mod heartbeat;
mod identity;
mod mcp;
mod store;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::agent::conversation::Conversation;
use crate::agent::providers::create_provider;
use crate::config::Config;
use crate::egregore::EgregoreClient;
use crate::error::{FamiliarError, Result};
use crate::identity::Identity;
use crate::mcp::McpPool;
use crate::store::Store;

#[derive(Parser)]
#[command(name = "familiar", version, about = "Personal companion for Thallus")]
struct Cli {
    /// Path to config file
    #[arg(short, long, default_value = "~/.familiar/familiar.toml")]
    config: String,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize config directory
    Init,
    /// Execute a single prompt (non-interactive)
    Exec {
        /// The prompt to execute
        prompt: String,
    },
    /// Run as Discord bot
    Discord,
    /// Run as persistent daemon (watches feed, responds automatically)
    Daemon,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // In interactive REPL mode, suppress logs to avoid interleaving with output.
    // In exec/init/daemon mode or when RUST_LOG is set, show info-level logs.
    let is_interactive = cli.command.is_none();
    let default_filter = if is_interactive {
        "familiar=error"
    } else {
        "familiar=info"
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter)),
        )
        .with_target(false)
        .init();

    if let Err(e) = run(cli).await {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let config_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".familiar");

    // Handle init command before loading config
    if let Some(Commands::Init) = &cli.command {
        return cli::init::run_init(&config_dir);
    }

    // Load config
    let config_path = PathBuf::from(Config::expand_path(&cli.config));
    if !config_path.exists() {
        eprintln!("Config not found at {}", config_path.display());
        eprintln!("Run `familiar init` to create one.");
        return Err(FamiliarError::Config {
            reason: format!("config not found: {}", config_path.display()),
        });
    }
    let config = Config::load(&config_path)?;

    // Load identity
    let key_path = PathBuf::from(Config::expand_path(&config.identity.secret_key));
    let identity = Identity::load(&key_path)?;
    tracing::info!(id = %identity.public_id(), "loaded identity");

    // Connect to egregore
    let egregore = EgregoreClient::new(&config.egregore.api_url, config.egregore.api_token.clone());
    match egregore.health_check().await {
        Ok(true) => tracing::info!(url = %config.egregore.api_url, "egregore connected"),
        _ => tracing::warn!(url = %config.egregore.api_url, "egregore not reachable (continuing anyway)"),
    }

    // Initialize MCP pool
    let mut mcp_pool = McpPool::new();
    for (name, server_config) in &config.mcp {
        mcp_pool.add_client(name, server_config)?;
    }
    if !config.mcp.is_empty() {
        mcp_pool.initialize_all().await;
        tracing::info!(servers = config.mcp.len(), "MCP servers initialized");
    }

    // Open local store
    let store_path = PathBuf::from(Config::expand_path(&config.store.path));
    let store = Store::open(&store_path)?;
    tracing::info!(path = %store_path.display(), "local store opened");

    // Create LLM provider
    let llm_config = config.llm.as_ref().ok_or_else(|| FamiliarError::Config {
        reason: "LLM configuration required. Add [llm] section to config.".into(),
    })?;
    let provider = create_provider(llm_config)?;
    tracing::info!(provider = provider.name(), "LLM provider ready");

    // Start heartbeat background task if enabled
    if config.heartbeat.enabled {
        let hb_llm_config = llm_config.clone();
        let hb_provider = create_provider(&hb_llm_config)?;
        let hb_store_path = Config::expand_path(&config.store.path);
        let hb = heartbeat::Heartbeat::new(
            hb_provider,
            hb_store_path,
            std::time::Duration::from_secs(config.heartbeat.interval_secs),
            config.heartbeat.quiet_start,
            config.heartbeat.quiet_end,
        );
        tokio::spawn(hb.run());
        tracing::info!(
            interval_secs = config.heartbeat.interval_secs,
            "heartbeat started"
        );
    }

    // Build conversation engine
    let conversation = Conversation::new(
        provider,
        mcp_pool,
        egregore,
        store,
        config.agent.clone(),
    );

    // Dispatch command
    match cli.command {
        Some(Commands::Exec { prompt }) => {
            let response = conversation.send(&prompt, None).await?;
            println!("{}", response);
        }
        Some(Commands::Daemon) => {
            let store_path = Config::expand_path(&config.store.path);
            let daemon = daemon::Daemon::new(
                conversation,
                config.egregore.api_url.clone(),
                identity.public_id().to_string(),
                store_path,
            );
            tracing::info!("running as daemon");
            daemon.run().await?;
        }
        Some(Commands::Discord) => {
            let discord_config = config.discord.as_ref().ok_or_else(|| FamiliarError::Config {
                reason: "Discord requires [discord] section in config. Add token_env at minimum.".into(),
            })?;
            let channel = channel::discord::DiscordChannel::new(discord_config).await?;
            tracing::info!("running as Discord bot");
            cli::repl::run_session(Box::new(channel), &conversation, &config.repl).await?;
        }
        _ => {
            // Default: interactive REPL channel
            let channel = channel::repl::ReplChannel::new(config.repl.clone())?;
            cli::repl::run_session(Box::new(channel), &conversation, &config.repl).await?;
        }
    }

    Ok(())
}
