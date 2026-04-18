# Familiar

Personal companion for the Thallus decentralized AI infrastructure.

## Architecture

Familiar is the mind of Thallus — an always-on conversational companion that translates
natural language into egregore network actions. It publishes under your identity.

## Module Map

| Module | Location | Purpose |
|--------|----------|---------|
| agent | src/agent/ | Conversation loop, providers (via thallus-core) |
| channel | src/channel/ | Transport abstraction (REPL, Discord) |
| cli | src/cli/ | Session driver, init command |
| config | src/config/ | TOML config loading |
| egregore | src/egregore/ | HTTP client for publish/query |
| heartbeat | src/heartbeat.rs | Background proactive checks |
| store | src/store/ | Local SQLite (conversations, context, publish log) |
| error | src/error.rs | FamiliarError |

## Commands

```bash
cargo build --release
cargo test
./familiar init          # Create config
./familiar               # Interactive REPL
./familiar exec "prompt" # Single execution
./familiar discord       # Run as Discord bot
```

## Configuration

Config at `~/.familiar/familiar.toml`. Key sections:

- `[identity]` — path to egregore secret key
- `[egregore]` — daemon API URL
- `[llm]` — provider (claude-code, anthropic, openai)
- `[mcp.*]` — local MCP servers
- `[agent]` — max_turns, timeout, blocked_tools
- `[store]` — SQLite path
- `[heartbeat]` — interval, quiet hours
- `[repl]` — customizable prompts
- `[discord]` — bot token, guild allowlist
