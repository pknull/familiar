mod agent;
mod channel;
mod cli;
mod config;
mod daemon;
mod egregore;
mod error;
mod heartbeat;
mod hooks;
mod mcp;
mod profile;
mod store;
mod tui;
mod workspace;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::agent::conversation::Conversation;
use thallus_core::provider::create_provider;
use crate::config::Config;
use crate::egregore::EgregoreClient;
use crate::error::{FamiliarError, Result};
use crate::mcp::McpPool;
use crate::store::Store;

#[derive(Parser)]
#[command(name = "familiar", version, about = "Personal companion for Thallus")]
struct Cli {
    /// Path to config file
    #[arg(short, long, default_value = "~/.familiar/familiar.toml")]
    config: String,

    /// Use simple REPL mode instead of TUI
    #[arg(long)]
    simple: bool,

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
    /// List all sessions
    Sessions,
    /// Resume a previous session
    Resume {
        /// Session ID or slug (interactive picker if omitted)
        session: Option<String>,
    },
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

    // Connect to egregore
    let egregore = EgregoreClient::new(&config.egregore.api_url, config.egregore.api_token.clone());
    match egregore.health_check().await {
        Ok(true) => tracing::info!(url = %config.egregore.api_url, "egregore connected"),
        _ => {
            tracing::warn!(url = %config.egregore.api_url, "egregore not reachable (continuing anyway)")
        }
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

    // Handle commands that don't need LLM provider.
    match &cli.command {
        Some(Commands::Sessions) => {
            let sessions = store.list_sessions()?;
            if sessions.is_empty() {
                println!("No sessions found.");
            } else {
                println!("{:<38} {:<30} {}", "ID", "Slug", "Updated");
                println!("{}", "-".repeat(80));
                for s in &sessions {
                    println!("{:<38} {:<30} {}", s.id, s.slug, s.updated_at);
                }
            }
            return Ok(());
        }
        Some(Commands::Resume { session }) if session.is_none() => {
            // Interactive picker — check if there are sessions before requiring LLM.
            let sessions = store.list_sessions()?;
            if sessions.is_empty() {
                return Err(FamiliarError::Internal {
                    reason: "No sessions to resume.".into(),
                });
            }
            // Fall through to full startup for the actual resume with history loading.
        }
        _ => {}
    }

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
            Config::expand_path("~/.familiar/workspace/daily"),
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

    // Initialize workspace (prompt assembly from ~/.familiar/workspace/)
    let workspace_dir = Config::expand_path("~/.familiar/workspace");
    let workspace = workspace::Workspace::new(&workspace_dir)?;
    tracing::info!(path = %workspace_dir, "workspace initialized");

    // Clone egregore client for sidebar pane pollers (before move into conversation).
    let egregore_for_panes = egregore.clone();

    // Build conversation engine
    let model_name = llm_config.model.clone();
    let mut conversation = Conversation::new(
        provider,
        &model_name,
        mcp_pool,
        egregore,
        store,
        config.agent.clone(),
        config.tools.clone(),
        workspace,
    );

    // Dispatch command
    match cli.command {
        Some(Commands::Exec { prompt }) => {
            let (response, _usage) = conversation.send(&prompt, None).await?;
            println!("{}", response);
        }
        Some(Commands::Daemon) => {
            let store_path = Config::expand_path(&config.store.path);
            let identity_id = egregore_for_panes.get_public_id().await?;
            let daemon = daemon::Daemon::new(
                conversation,
                egregore_for_panes.clone(),
                config.egregore.api_url.clone(),
                identity_id,
                store_path,
                config.daemon.clone(),
                config.agent.clone(),
            );
            tracing::info!("running as daemon");
            daemon.run().await?;
        }
        Some(Commands::Discord) => {
            let discord_config = config
                .discord
                .as_ref()
                .ok_or_else(|| FamiliarError::Config {
                    reason:
                        "Discord requires [discord] section in config. Add token_env at minimum."
                            .into(),
                })?;
            let channel = channel::discord::DiscordChannel::new(discord_config).await?;
            tracing::info!("running as Discord bot");
            cli::repl::run_session(Box::new(channel), &mut conversation, &config.repl).await?;
        }
        Some(Commands::Sessions) => {
            // Handled early (before LLM provider), should not reach here.
            unreachable!();
        }
        Some(Commands::Resume { session }) => {
            let store_path = PathBuf::from(Config::expand_path(&config.store.path));
            let resume_store = Store::open(&store_path)?;
            let sessions = resume_store.list_sessions()?;

            let session_id = match session {
                Some(ref arg) => {
                    // Match by ID or slug
                    sessions
                        .iter()
                        .find(|s| s.id == *arg || s.slug == *arg)
                        .map(|s| s.id.clone())
                        .ok_or_else(|| FamiliarError::Internal {
                            reason: format!("No session found matching '{}'", arg),
                        })?
                }
                None => {
                    // Interactive picker
                    if sessions.is_empty() {
                        return Err(FamiliarError::Internal {
                            reason: "No sessions to resume.".into(),
                        });
                    }
                    println!("Select a session to resume:\n");
                    for (i, s) in sessions.iter().enumerate() {
                        println!("  {} — {} ({})", i + 1, s.slug, s.updated_at);
                    }
                    println!();
                    print!("Enter number: ");
                    use std::io::Write;
                    std::io::stdout().flush().unwrap();

                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).map_err(|e| {
                        FamiliarError::Internal {
                            reason: format!("Failed to read input: {}", e),
                        }
                    })?;

                    let idx: usize = input.trim().parse().map_err(|_| FamiliarError::Internal {
                        reason: "Invalid selection".into(),
                    })?;

                    if idx == 0 || idx > sessions.len() {
                        return Err(FamiliarError::Internal {
                            reason: format!("Selection out of range (1-{})", sessions.len()),
                        });
                    }

                    sessions[idx - 1].id.clone()
                }
            };

            // Load session history into conversation store.
            let resume_thread = resume_store.resolve_thread(&session_id, "repl", None)?;
            let turns = resume_store.thread_recent_turns(&resume_thread, 100)?;
            let store_path = PathBuf::from(Config::expand_path(&config.store.path));
            let active_store = Store::open(&store_path)?;
            for (role, content, tool_calls) in &turns {
                active_store.add_turn(role, content, tool_calls.as_deref())?;
            }
            resume_store.touch_session(&session_id)?;
            println!(
                "Resumed session: {} ({} turns loaded)",
                session_id,
                turns.len()
            );

            let channel = channel::repl::ReplChannel::new(config.repl.clone())?;
            cli::repl::run_session(Box::new(channel), &mut conversation, &config.repl).await?;
        }
        _ => {
            if cli.simple {
                // --simple: bare REPL, unchanged from original
                let channel = channel::repl::ReplChannel::new(config.repl.clone())?;
                cli::repl::run_session(Box::new(channel), &mut conversation, &config.repl).await?;
            } else {
                // Default: TUI operator console
                let model_name = llm_config.model.clone();
                let session_name = format!("session-{}", chrono::Local::now().format("%H%M"));

                let state = std::sync::Arc::new(tokio::sync::Mutex::new(tui::AppState::new(
                    model_name,
                    session_name,
                    config.tui.panes.len(),
                    config.tui.status_template.clone(),
                )));
                let (event_tx, event_rx) = tui::event_channel();
                let (tui_channel, input_tx) = channel::tui_channel::TuiChannel::new(
                    event_tx.clone(),
                    std::sync::Arc::clone(&state),
                );

                // Spawn sidebar pane pollers.
                for (i, pane) in config.tui.panes.iter().enumerate() {
                    let pane_state = std::sync::Arc::clone(&state);
                    let pane_egregore = egregore_for_panes.clone();
                    let pane_source = pane.source.clone();
                    let pane_filter = pane.filter_content_type.clone();
                    let pane_command = pane.command.clone();
                    let pane_restart = pane.restart;
                    let poll_secs = pane.poll_interval_secs.unwrap_or(10);

                    tokio::spawn(async move {
                        use crate::tui::widgets::sidebar::{
                            FeedItem, PaneData, PeerItem, TaskItem,
                        };
                        let mut interval =
                            tokio::time::interval(std::time::Duration::from_secs(poll_secs));
                        loop {
                            interval.tick().await;
                            let data = match pane_source.as_str() {
                                "egregore_feed" => {
                                    let ct = pane_filter.as_deref();
                                    match pane_egregore
                                        .query_messages(None, ct, None, None, 20)
                                        .await
                                    {
                                        Ok(msgs) => PaneData::Feed(
                                            msgs.iter()
                                                .map(|m| FeedItem {
                                                    content_type: m
                                                        .get("content")
                                                        .and_then(|c| c.get("type"))
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("?")
                                                        .to_string(),
                                                    summary: m
                                                        .get("content")
                                                        .and_then(|c| {
                                                            c.get("title")
                                                                .or(c.get("observation"))
                                                                .or(c.get("question"))
                                                        })
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("")
                                                        .chars()
                                                        .take(80)
                                                        .collect(),
                                                })
                                                .collect(),
                                        ),
                                        Err(_) => PaneData::Empty,
                                    }
                                }
                                "tasks" => {
                                    // Query task lifecycle messages (all task-related content types).
                                    let tag_query = pane_egregore
                                        .query_messages(None, None, Some("task"), None, 30)
                                        .await;
                                    match tag_query {
                                        Ok(msgs) => PaneData::Tasks(
                                            msgs.iter()
                                                .map(|m| {
                                                    let content_type = m
                                                        .get("content")
                                                        .and_then(|c| c.get("type"))
                                                        .and_then(|t| t.as_str())
                                                        .unwrap_or("unknown");
                                                    let status = match content_type {
                                                        "task" => "pending",
                                                        "task_offer" => "offered",
                                                        "task_assign" => "active",
                                                        "task_result" => m
                                                            .get("content")
                                                            .and_then(|c| c.get("status"))
                                                            .and_then(|s| s.as_str())
                                                            .unwrap_or("completed"),
                                                        _ => "unknown",
                                                    };
                                                    TaskItem {
                                                        status: status.to_string(),
                                                        summary: m
                                                            .get("content")
                                                            .and_then(|c| {
                                                                c.get("task_id")
                                                                    .or(c.get("prompt"))
                                                                    .or(c.get("summary"))
                                                            })
                                                            .and_then(|v| v.as_str())
                                                            .unwrap_or("")
                                                            .chars()
                                                            .take(60)
                                                            .collect(),
                                                    }
                                                })
                                                .collect(),
                                        ),
                                        Err(_) => PaneData::Empty,
                                    }
                                }
                                "peers" => match pane_egregore.get_mesh().await {
                                    Ok(peers) => PaneData::Peers(
                                        peers
                                            .iter()
                                            .map(|p| PeerItem {
                                                name: p
                                                    .get("peer_id")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("?")
                                                    .chars()
                                                    .take(20)
                                                    .collect(),
                                                health: p
                                                    .get("status")
                                                    .and_then(|v| v.as_str())
                                                    .unwrap_or("unknown")
                                                    .to_string(),
                                            })
                                            .collect(),
                                    ),
                                    Err(_) => PaneData::Empty,
                                },
                                "script" => {
                                    if let Some(ref cmd) = pane_command {
                                        match tokio::process::Command::new("sh")
                                            .arg("-c")
                                            .arg(cmd)
                                            .output()
                                            .await
                                        {
                                            Ok(output) => {
                                                let stdout =
                                                    String::from_utf8_lossy(&output.stdout);
                                                let stderr =
                                                    String::from_utf8_lossy(&output.stderr);
                                                let combined = if stderr.is_empty() {
                                                    stdout.to_string()
                                                } else {
                                                    format!(
                                                        "{}\n--- stderr ---\n{}",
                                                        stdout, stderr
                                                    )
                                                };
                                                PaneData::Script(combined)
                                            }
                                            Err(e) => PaneData::Script(format!("error: {}", e)),
                                        }
                                    } else {
                                        PaneData::Script("(no command configured)".into())
                                    }
                                }
                                _ => PaneData::Empty,
                            };

                            let mut s = pane_state.lock().await;
                            if let Some(slot) = s.pane_data.get_mut(i) {
                                *slot = data;
                            }
                        }
                    });
                }

                // Spawn TUI renderer in background (it's Send-safe).
                let tui_state = std::sync::Arc::clone(&state);
                let tui_config = config.tui.clone();
                let tui_handle = tokio::spawn(async move {
                    tui::run(tui_state, event_rx, input_tx, &tui_config).await
                });

                // Run conversation on main thread (Conversation is not Send).
                let conv_result =
                    cli::repl::run_session(Box::new(tui_channel), &mut conversation, &config.repl)
                        .await;

                // If conversation ended, signal TUI to quit.
                {
                    let mut s = state.lock().await;
                    s.should_quit = true;
                }
                let _ = tui_handle.await;

                conv_result?;
            }
        }
    }

    Ok(())
}
