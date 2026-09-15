# Objective

Familiar is the mind of Thallus: the always-on conversational companion that
owns LLM reasoning and planning, translates natural language into Egregore
actions and concrete tool_calls for Servitor, and publishes through the local
Egregore node. Channels: REPL, TUI, Discord, daemon.

# State

Verified 2026-09-15. Version 0.5.0; main at 5809683 (rustfmt test
additions, 2026-08-15); CI green on main. Familiar is a decision client of
RFC 0003: it plans and requests, Servitor authorizes and executes. Automated
assignment still uses bare task_assign; AssignTaskV1 remains supported but
not yet adopted here. Local conversation history lives in SQLite. Consumes
thallus-core for identity, MCP client and LLM providers. This repo now
carries its own Memory v2 pair; cross-component decisions stay in the
Thallus umbrella (private repo pknull/Thallus).

# Next

- Optional when next touched: migrate automated assignment to AssignTaskV1
  (bare task_assign remains supported).
- Keep the retrieval/effect boundary tests current when adding channels.

# Blockers

- None.
