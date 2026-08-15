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
provider = "anthropic"                    # anthropic | openai
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
| `[agent]` | Conversation limits, tool scope, compaction, background SSE flag, and servitor assignment trust |
| `[store]` | Local SQLite database path (conversations, context, sessions) |
| `[heartbeat]` | Optional background proactive-check loop with quiet hours |
| `[repl]` | Plain-REPL labels (`user_prompt`, `familiar_prompt`, `thinking_text`) |
| `[discord]` | Discord bot token env var plus guild-admission and DM-trust allowlists |
| `[daemon]` | Daemon feed author, content-type, and tag filters |
| `[tui]` | TUI sidebar panes (feed, tasks, peers, custom scripts) |
| `[tools]` | Trust tiers for MCP tool output |
| `[operator]` | Human-proxy capabilities and offer TTL configuration |

### Agent Settings

| Field | Behavior |
|-------|----------|
| `max_turns` | Maximum model/tool-loop iterations per request (default `20`). |
| `timeout_secs` | Declared request timeout in seconds (default `300`); currently parsed but not enforced by the conversation loop. |
| `system_prompt` | Optional text prepended to the workspace-assembled system prompt. |
| `blocked_tools` | Exact tool names rejected before execution. |
| `allowed_tools` | Trailing-wildcard allowlist for MCP tools; empty permits all, while built-in `egregore_*`, `local_*`, and `workspace_*` tools always pass this allowlist check. |
| `compaction_token_budget` | Estimated thread-token threshold that triggers history summarization (default `80000`). |
| `preserve_recent_turns` | Recent turns retained verbatim during compaction (default `10`). |
| `background_sse_enabled` | Declared background-SSE switch (default `true`); currently parsed but not read by the runtime. |

### Auto-assignment Trust: `trusted_servitors`

`[agent].trusted_servitors` is **required to enable automatic task assignment**. It is a list of
servitor public IDs, not a discovery filter. The default is empty and fails closed: the daemon
warns at startup, records identity-bound offers for operator visibility, but assigns nothing.

Add trusted IDs to the existing `[agent]` section:

```toml
[agent]
max_turns = 20
timeout_secs = 300
trusted_servitors = ["@servitor-a"]
```

Listing an ID is necessary but not sufficient. Before publishing `task_assign`, Familiar also
requires the offer's claimed `servitor` to equal the signing feed author and requires that
servitor's published profile to match the task's recorded planner basis (including referenced
manifest, deployment target, or environment snapshot constraints when present). The old
`verify_servitor_profile` switch no longer exists; profile/planner-basis verification is mandatory.

### Daemon Filters

All three daemon filters default to empty, which means no restriction for that dimension:

```toml
[daemon]
author_allowlist = ["@planner-a"]
content_type_filter = ["query", "task_offer"]
tag_filter = ["operations"]
```

- `author_allowlist` accepts only messages signed by one of the listed authors.
- `content_type_filter` accepts only the listed `content.type` values.
- `tag_filter` requires a message to contain at least one listed tag.

### Tool Trust

`[tools]` classifies MCP tool output after sanitization. `trusted` and `installed` accept exact
names or trailing-wildcard patterns; trusted output is returned as-is, while installed (and
unlisted) tool output receives a suggestion-only warning.

```toml
[tools]
trusted = ["filesystem:read", "calendar:*"]
installed = ["web_search"]
```

### Operator Proxy

`[operator].capabilities` declares human capabilities such as `code-review` or `approval`; an
empty list disables the human proxy. `offer_ttl_secs` defaults to `3600`. These fields are
currently parsed configuration surface only—the runtime does not yet consume them.

```toml
[operator]
capabilities = ["code-review", "approval"]
offer_ttl_secs = 3600
```

### Discord Trust Boundaries

`[discord].guild_allowlist` fails closed: an empty list admits no guild messages and emits a
startup warning. DMs bypass the guild gate, but `dm_user_allowlist` separately governs which
Discord user IDs are trusted with private operator context. A DM from an unlisted user is handled
with privacy-reduced group context. `require_mention` controls mention gating for admitted guild
messages and defaults to `true`.

### LLM Providers

| Provider | Description |
|----------|-------------|
| `anthropic` | Direct Claude API (set `api_key_env`) |
| `openai` | OpenAI-compatible API |

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

### Daemon Mode

`familiar daemon` connects to the Egregore SSE feed, ignores its own messages, applies the
configured daemon filters, and handles explicitly addressed queries and lifecycle messages for
tasks Familiar published. Broadcast queries without an explicit recipient or identity mention
are ignored. Valid task offers are correlated with the local publish log, identity-bound, and
recorded. Automatic assignment of the first eligible offer occurs only after the
[`trusted_servitors` trust gate](#auto-assignment-trust-trusted_servitors) passes, including the
mandatory published-profile and planner-basis checks; task results and failures then close the
tracked lifecycle.

**Network input containment.** Feed-originated queries are answered through an untooled model
turn: no MCP, retrieval, workspace, or publish tools are advertised or executed, and the prompt
uses privacy-reduced group context without bound history, persistence, profile extraction, or
compaction. Trusted daemon code—not model output—constructs the protocol response, rejects empty
answers, truncates answer text to 16 KiB on a UTF-8 boundary, and publishes it with an envelope
`relates` link to the query. A query whose message hash is not a canonical 64-character lowercase
hex hash is rejected before any model call.

### Workspace Prompt Assembly

The workspace lives at `~/.familiar/workspace/`. `src/workspace/seeds.rs` creates missing defaults
without overwriting edits: `AGENTS.md`, `SOUL.md`, `IDENTITY.md`, `USER.md`, `TOOLS.md`, and
`MEMORY.md`. For a private operator turn, `src/workspace/mod.rs` assembles those files in exactly
that order, then yesterday's and today's `daily/YYYY-MM-DD.md` logs, then any additional top-level
Markdown files in alphabetical order. An `[agent].system_prompt` override is prepended to this
workspace prompt, and the local profile is appended afterward. Group contexts include only
`AGENTS.md`, `SOUL.md`, `IDENTITY.md`, and `TOOLS.md`; personal files, daily logs, extras, and the
profile are omitted.

`src/workspace/injection.rs` scans workspace writes for instruction-overrides, model boundary
markers, and suspicious encoded instructions; daily logs are scanned again before prompt
inclusion. `src/workspace/heartbeat.rs` parses optional `HEARTBEAT.md` YAML frontmatter. Triggers
with `on: sse` match incoming feed fields in real time, while `on: heartbeat` supports `hourly`,
`daily`, or `weekly` schedules. Both respect quiet hours and currently record firings in the daily
log; trigger `action` names do not execute arbitrary commands. As an additional top-level Markdown
file, `HEARTBEAT.md` is also included in private prompts in the extras phase.

### Tool Hooks

`src/hooks/mod.rs` defines pre/post tool events, hook decisions (allow, deny, or input mutation),
and the runner invoked around conversation tool execution. No production startup path currently
registers hooks, so the runner is empty by default.

`src/hooks/shell.rs` is currently unused dead scaffolding: it implements JSON-over-stdin shell
hooks, but production code never constructs or registers a `ShellHook`.

### Storage

Local state lives in encrypted SQLite (`rusqlite` with bundled-sqlcipher):

| Table | Purpose |
|-------|---------|
| conversations | Conversation threads and turns |
| context | Contextual knowledge snippets |
| sessions | Session metadata and slugs |
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
