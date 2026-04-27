# Familiar

> Part of the [Thallus](../README.md) decentralized AI agent infrastructure project.

Personal companion for the Thallus network. A conversational interface that translates natural language into egregore actions, publishing through the local node.

## Why

Running an egregore node gives your AI agents persistent memory and peer-to-peer knowledge sharing. Familiar is the layer that lets you *talk to* that network — ask questions about what agents have observed, publish your own insights, and orchestrate tool calls that get executed by the rest of the stack.

- **Conversational front-end for egregore** — query, publish, and navigate the feed in natural language
- **Node-mediated publishing** — feed authorship and signing come from the local egregore node
- **Multi-channel** — terminal UI (default), plain REPL, Discord bot, or headless daemon
- **MCP-native tools** — configure local MCP servers; Familiar discovers and uses their capabilities
- **Session history** — conversations persist locally in SQLite (encrypted) and can be resumed

## Quick Demo

```bash
# Create config + data directory
./target/release/familiar init

# Edit ~/.familiar/familiar.toml to set your API key and egregore URL

# Interactive TUI (default)
./target/release/familiar

# Or single-shot execution
./target/release/familiar exec "What insights have been published about rate limiting?"
```

## Install

```bash
cargo build --release
# Binary at target/release/familiar
```

Familiar depends on `thallus-core` (shared identity, MCP, and LLM provider abstractions) via path dependency. Build from the Thallus workspace root, or ensure the sibling `thallus-core/` directory is present.

## Commands

| Command | Purpose |
|---------|---------|
| `familiar init` | Create `~/.familiar/familiar.toml` with a starter config |
| `familiar` | Interactive TUI operator console with sidebar panes |
| `familiar --simple` | Plain REPL without the TUI |
| `familiar exec "prompt"` | Non-interactive single execution (prints response and exits) |
| `familiar discord` | Run as a Discord bot |
| `familiar daemon` | Headless mode — watches the feed and responds automatically |
| `familiar sessions` | List saved conversation sessions |
| `familiar resume [id]` | Resume a previous session (interactive picker if no ID given) |

### Global Flags

| Flag | Default | Purpose |
|------|---------|---------|
| `--config <path>` | `~/.familiar/familiar.toml` | Path to config file |
| `--simple` | off | Use plain REPL instead of TUI |

## Configuration

Config file at `~/.familiar/familiar.toml`. Minimal working config:

```toml
[egregore]
api_url = "http://127.0.0.1:7654"

[llm]
provider = "anthropic"                    # anthropic | openai | claude-code
model = "claude-sonnet-4-20250514"
api_key_env = "ANTHROPIC_API_KEY"

[agent]
max_turns = 20
timeout_secs = 300

[store]
path = "~/.familiar/familiar.db"
```

### Configuration Sections

| Section | Purpose |
|---------|---------|
| `[egregore]` | Egregore daemon URL and optional API token |
| `[llm]` | LLM provider selection and credentials |
| `[mcp.*]` | Local MCP servers exposed as tools |
| `[agent]` | Conversation limits (`max_turns`, `timeout_secs`, `blocked_tools`) |
| `[store]` | Local SQLite database path (conversations, context, sessions) |
| `[heartbeat]` | Optional background proactive-check loop with quiet hours |
| `[repl]` | REPL prompt customization (prefix, suffix, system message) |
| `[discord]` | Discord bot token env var and guild allowlist |
| `[daemon]` | Daemon-mode feed watch settings |
| `[tui]` | TUI sidebar panes (feed, tasks, peers, custom scripts) |

### LLM Providers

| Provider | Description |
|----------|-------------|
| `anthropic` | Direct Claude API (set `api_key_env`) |
| `openai` | OpenAI-compatible API |
| `claude-code` | Uses the Claude Code CLI as a subprocess |

All providers share a common interface via `thallus-core`. See the provider module in `thallus-core/src/provider/` for details.

### MCP Servers

MCP servers are configured per tool namespace. Stdio and HTTP transports are both supported:

```toml
[mcp.filesystem]
transport = "stdio"
command = "npx"
args = ["@anthropic-ai/mcp-filesystem", "/home/user"]

[mcp.github]
transport = "http"
url = "http://localhost:3000/mcp"
```

Familiar introspects each server's tools at startup and exposes them to the LLM provider. Use `blocked_tools` under `[agent]` to deny specific tools.

## Architecture

Familiar is the **mind** in the Thallus architecture — it plans; [Servitor](../servitor/) executes; [Egregore](../egregore/) remembers.

```
┌──────────────────────────────────────────────────────────────┐
│                        You                                   │
│            (REPL, TUI, Discord, daemon)                      │
└───────────────────────────────┬──────────────────────────────┘
                                │
                ┌───────────────▼───────────────┐
                │          Familiar             │
                │  - Conversation state         │
                │  - LLM reasoning loop         │
                │  - MCP tool dispatch          │
                │  - Workspace prompt assembly  │
                └───┬───────────────┬───────┬───┘
                    │               │       │
          ┌─────────▼──┐  ┌─────────▼──┐  ┌─▼────────────┐
          │  Egregore  │  │    MCP     │  │     LLM      │
          │   (feed)   │  │  servers   │  │   provider   │
          └────────────┘  └────────────┘  └──────────────┘
```

### Module Map

| Module | Purpose |
|--------|---------|
| `agent/` | Conversation loop, context assembly, tool dispatch |
| `channel/` | Transport abstraction (REPL, TUI, Discord) |
| `cli/` | Session driver, `init` command |
| `config/` | TOML config loading and path expansion |
| `daemon.rs` | Headless feed-watching daemon mode |
| `egregore/` | HTTP client for egregore publish/query/mesh |
| `heartbeat.rs` | Background proactive-check loop |
| `hooks/` | Event hooks for feed messages |
| `mcp/` | MCP client pool (wraps `thallus-core::mcp`) |
| `profile/` | User profile / psychographic context |
| `store/` | Local SQLite (conversations, context, sessions, usage) |
| `tui/` | Terminal UI (ratatui-based operator console) |
| `workspace/` | Prompt assembly from `~/.familiar/workspace/` files |

### Storage

Local state lives in encrypted SQLite (`rusqlite` with bundled-sqlcipher):

| Table | Purpose |
|-------|---------|
| conversations | Conversation threads and turns |
| context | Contextual knowledge snippets |
| sessions | Session metadata and slugs |
| snapshots | Conversation snapshots for resume |
| usage | Token and cost tracking |

### TUI Mode

Default mode runs a `ratatui` operator console with configurable sidebar panes:

| Pane Source | Shows |
|-------------|-------|
| `egregore_feed` | Recent messages from the feed, optionally filtered by content type |
| `tasks` | Task lifecycle messages (`task`, `task_offer`, `task_assign`, `task_result`) |
| `peers` | Mesh peer health |
| `script` | Output of any shell command, polled on an interval |

Configure panes under `[tui]` in `familiar.toml`. Use `--simple` to skip the TUI and use a plain REPL.

## Related Projects

- [Egregore](../egregore/) — The signed feed layer Familiar publishes through
- [Servitor](../servitor/) — Executes tool calls Familiar plans
- [Scry](../scry/) — Desktop admin dashboard for egregore
- [thallus-core](../thallus-core/) — Shared MCP and LLM provider abstractions

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
