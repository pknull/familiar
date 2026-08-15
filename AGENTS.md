# Familiar

Personal companion for the Thallus decentralized AI infrastructure.

## Architecture

Familiar is the mind of Thallus — an always-on conversational companion that translates
natural language into egregore network actions. It publishes through the local egregore node.

## Module Map

| Module | Location | Purpose |
|--------|----------|---------|
| entry points | src/main.rs, src/lib.rs | CLI dispatch and library exports |
| agent | src/agent/ | Conversation/tool loop plus token estimation, summarization, and history compaction in `compaction.rs` |
| channel | src/channel/ | Transport abstraction, plain REPL, the `tui_channel.rs` TUI bridge, and Discord |
| cli | src/cli/ | Interactive session driver and `init` implementation |
| config | src/config/ | TOML types, defaults, loading, and path expansion |
| daemon | src/daemon.rs | Headless SSE feed watcher, contained query responses, and task-offer assignment flow |
| egregore | src/egregore/ | HTTP client for node identity, publish, query, and mesh status |
| heartbeat | src/heartbeat.rs | Background proactive checks, scheduled triggers, quiet hours, and housekeeping |
| hooks | src/hooks/ | Pre/post tool-use hook interfaces and currently unwired shell-hook scaffolding |
| identity | src/identity/ | Empty legacy directory, not a compiled module; shared identity types come from `thallus-core` and feed signing belongs to the local Egregore node |
| mcp | src/mcp/ | MCP client-pool wrapper around `thallus-core` |
| profile | src/profile/ | Local operator profile loading and signal extraction |
| store | src/store/ | Encrypted SQLite conversations, context, sessions, publish log, and usage accounting |
| tui | src/tui/ | Ratatui operator console, layout, and widgets |
| workspace | src/workspace/ | Seed files, prompt assembly, injection scanning, and `HEARTBEAT.md` parsing |
| error | src/error.rs | `FamiliarError` and crate result alias |

## Commands

```bash
cargo build --release
cargo test
./familiar init          # Create config
./familiar               # Interactive TUI (default)
./familiar --simple      # Plain REPL mode
./familiar exec "prompt" # Single execution
./familiar discord       # Run as Discord bot
./familiar daemon        # Watch the feed in headless mode
./familiar sessions      # List saved sessions
./familiar resume [id]   # Resume by ID/slug, or pick interactively
```

The TUI is the default interactive mode. The plain REPL is an alternate mode selected with
`--simple`; it is not the default.

## Configuration

Config at `~/.familiar/familiar.toml`. Key sections:

- `[egregore]` — daemon API URL
- `[llm]` — provider (anthropic, openai)
- `[mcp.*]` — local MCP servers
- `[agent]` — conversation limits, tool scope, compaction, and servitor assignment trust
- `[store]` — SQLite path
- `[heartbeat]` — interval, quiet hours
- `[repl]` — customizable prompts
- `[discord]` — bot token plus guild and DM trust allowlists
- `[daemon]` — feed author, content-type, and tag filters
- `[tui]` — theme, input behavior, status template, and sidebar panes
- `[tools]` — trusted versus suggestion-only MCP tool outputs
- `[operator]` — human-proxy capabilities and offer TTL configuration
