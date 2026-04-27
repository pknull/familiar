# Thallus Ecosystem Architecture Review

**Reviewer**: The Cartographer
**Date**: 2026-03-28
**Scope**: All 8 projects under /home/pknull/Code/Thallus/

---

## Summary Table

| Project | Language | Source LoC | Files | Maturity | Verdict |
|---------|----------|-----------|-------|----------|---------|
| egregore | Rust | ~21,200 | 62 | v1.2.4, production | **KEEP** |
| servitor | Rust | ~15,300 | 72 | v0.1.0, complete v1 | **KEEP** |
| familiar | Rust | ~4,200 | 27 | v0.1.0, day-one | **KEEP (with extraction)** |
| egregore-rs | Rust | ~890 | 5 | v0.1.0, unused | **REMOVE** |
| egregore-js | TypeScript | ~690 | 4 | v0.1.0, unused | **REMOVE** |
| egregore-py | Python | ~520 | 5 | v0.1.0, unused | **REMOVE** |
| egregore-web | TS+Rust | ~10,700 | 56 | v0.0.0, functional | **KEEP (reassess later)** |
| docs | mdBook | ~15 md files | n/a | Built, current | **KEEP** |

---

## Project-by-Project Assessment

---

### 1. egregore/ (Rust daemon)

**Position**: KEEP -- this is the foundational infrastructure.

**Evidence**:

- 21,200 lines of Rust across 62 source files. Version 1.2.4 -- the most mature project.
- Full implementation: signed feeds, gossip replication (SHS + Box Stream), SQLite storage, schema registry, consumer groups, retention policies, bloom filter sync, credit-based flow control, MCP server, SSE events, Prometheus metrics, optional OTLP.
- Custom crypto stack: Ed25519 signing, X25519 key exchange, ChaCha20-Poly1305, owner-only private-key permissions, Private Box multi-recipient encryption.
- 17 API route files. Comprehensive feature surface.

**Risks**:

- `cli_admin.rs` at 841 lines and `main.rs` at 892 lines both exceed the 800-line hard limit. `feed/schema.rs` at 1,076 lines and `feed/engine.rs` at 1,203 lines are significantly over. These are maintenance liabilities.
- MCP server (`mcp.rs` at 606 lines + `mcp_tools.rs` at 417 lines) is embedded in the daemon. If Familiar or future clients need MCP-style access, this creates an awkward dependency.

**Unknowns**:

- How much of the gossip protocol is battle-tested vs. theoretical. Flow control (654 lines) and bloom filters (635 lines) are substantial -- are they exercised in real deployments?

**Recommendation**:

- Keep as-is. Break up oversized files in a future cleanup pass (engine.rs, schema.rs, cli_admin.rs, main.rs).
- No structural changes needed.

---

### 2. servitor/ (Rust task executor)

**Position**: KEEP -- the network's actor/executor, distinct role.

**Evidence**:

- 15,300 lines across 72 files. Complete v1 with authority system, A2A protocol, MCP pool, scope enforcement, multiple LLM providers (Anthropic, OpenAI, Claude CLI, Codex), cron scheduler, SSE event sources, Discord transport, session management, Prometheus metrics.
- Authority model (Person/Place/Skill) is unique to servitor -- this is where access control lives.
- A2A server (JSON-RPC 2.0 task delegation) is a significant differentiator for multi-agent workflows.
- Session system (`session/` -- 4 files, ~1,100 lines) with transcript storage and session watcher is servitor-specific.

**Risks**:

- `agent/loop.rs` at 1,060 lines is the largest single file. `egregore/messages.rs` at 886 lines is also oversized. Both are critical paths.
- Discord transport (`comms/discord.rs`, 336 lines) pulls in the `serenity` crate which is a heavy dependency. If Discord moves to Familiar, this dependency can be shed.

**Unknowns**:

- Whether the Codex provider (`agent/providers/codex.rs`, 545 lines) is actively used or experimental.
- Whether A2A is used by any external agents yet.

**Recommendation**:

- Keep as the headless network executor. Do not absorb personal/interactive features.
- Extract shared code with Familiar (see cross-cutting section).

---

### 3. familiar/ (Rust personal companion)

**Position**: KEEP -- genuinely distinct role, but requires immediate deduplication.

**Evidence**:

- 4,200 lines across 27 files. Built today, clean architecture.
- Unique features that justify existence:
  - **Channel abstraction** (`channel/mod.rs`): transport-agnostic I/O trait. REPL is first implementation, Discord/Slack/Matrix can follow.
  - **Conversation engine** (`agent/conversation.rs`, 475 lines): maintains dialogue state across turns. Servitor's loop is stateless/reactive; this is stateful/interactive.
  - **Heartbeat system** (`heartbeat.rs`, 120 lines): periodic proactive health checks with quiet hours. Novel concept.
  - **Local store** with conversation history (`store/conversations.rs`): turns are stored locally, never published to the feed. This is the privacy boundary.
  - **System prompt** explicitly defines what goes to the feed vs. stays local. This is the core design principle.
- Clear architectural intent: conversation.rs comment says "Unlike servitor's task loop (stateless, reactive), this maintains dialogue state across turns and assembles personal context."

**Risks**:

- ~1,500 lines are copy-pasted from servitor with only error type names changed:
  - `identity/mod.rs`: 203 lines, identical except `FamiliarError` vs `ServitorError`
  - `mcp/client.rs`: 171 lines, identical
  - `mcp/circuit_breaker.rs`: 254 lines, identical
  - `mcp/pool.rs`: 475 vs 543 lines, near-identical (servitor has scope enforcement, familiar doesn't)
  - `mcp/stdio.rs`: 223 lines, identical
  - `mcp/http.rs`: 126 lines, identical
  - `agent/providers/mod.rs`: 231 vs 234 lines, near-identical (servitor has Codex)
  - `agent/providers/anthropic.rs`: 146 lines, identical
  - `agent/providers/openai.rs`: 330 lines, identical
  - `agent/providers/claude_cli.rs`: 254 vs 268 lines, near-identical
- This is approximately 2,200 lines duplicated (over 50% of Familiar's codebase). Any bug fix or feature addition to MCP or providers must be applied twice. This will diverge within weeks.

**Unknowns**:

- Whether the Channel trait will actually get Discord/Slack implementations, or if REPL is the only planned transport.
- Whether heartbeat is validated as useful or speculative.

**Recommendation**:

1. **Extract shared crate immediately** (see cross-cutting section).
2. Keep as distinct binary with its own config, identity, and conversation model.
3. Update the component model doc to include Familiar's role.

---

### 4. egregore-rs/ (Rust SDK)

**Position**: REMOVE.

**Evidence**:

- 888 lines across 5 files. Standard HTTP client wrapper.
- **Not used by any project in the ecosystem**. Neither servitor nor familiar depend on it as a crate. Both implement their own egregore HTTP clients directly (`servitor/src/egregore/`, `familiar/src/egregore/client.rs`).
- Not published to crates.io (v0.1.0, local only).
- The client code in servitor (`egregore/` module, ~1,480 lines across 6 files) is far more sophisticated than the SDK, including hook handling, context fetching, message formatting, and publish workflows.

**Risks**:

- Removing it means no standalone Rust SDK for third-party egregore clients. If external Rust consumers emerge, this would need to be rebuilt.
- Minimal risk: the code is trivial and could be recreated from servitor's egregore module in a day.

**Unknowns**:

- Whether there are external consumers outside this repo.

**Recommendation**:

- Archive or delete. If a Rust SDK is needed later, extract it from the shared crate (see cross-cutting section) rather than maintaining a separate project.

---

### 5. egregore-js/ (TypeScript SDK)

**Position**: REMOVE.

**Evidence**:

- 693 lines across 4 files. HTTP client + Ed25519 identity + models.
- **Not used by egregore-web**. The web app makes direct HTTP calls through Tauri's reqwest proxy.
- Not published to npm (v0.1.0, no dist/ directory).
- No tests.

**Risks**:

- Same as egregore-rs: no SDK for JS consumers. But egregore's MCP server (embedded in the daemon) provides a transport-agnostic interface that any language can use via JSON-RPC. The SDK model may be unnecessary.

**Unknowns**:

- Whether future Node.js agents or web apps would need this.

**Recommendation**:

- Archive or delete. MCP provides a better integration surface than language-specific SDKs.

---

### 6. egregore-py/ (Python SDK)

**Position**: REMOVE.

**Evidence**:

- 523 lines across 5 files. httpx client + PyNaCl identity + Pydantic models.
- 1 test file (`test_identity.py`). Better tested than egregore-js, but still minimal.
- Not published to PyPI. Not referenced by any project.
- Development status listed as "Alpha" in pyproject.toml.

**Risks**:

- Python is the dominant LLM ecosystem language. Removing the Python SDK closes a door for Python-based agents connecting to egregore. However, egregore's HTTP API is simple enough that a Python consumer can use raw httpx.

**Unknowns**:

- Whether Python agents are on the roadmap.

**Recommendation**:

- Archive or delete. Same reasoning as egregore-js: MCP and the HTTP API are sufficient integration surfaces. If needed, a Python MCP client is more reusable than an egregore-specific SDK.

---

### 7. egregore-web/ (Tauri desktop app)

**Position**: KEEP -- serves a different audience (human operator), but reassess after Familiar matures.

**Evidence**:

- ~10,700 lines of TypeScript/React across 53 files, plus ~340 lines of Rust (Tauri backend). Substantial, functional application.
- Full feature coverage: feed viewer, task management, trace visualization, peer management, schema registry UI, consumer group CRUD, retention policies, topic subscriptions, systemd service control, config editing.
- Serves the **operator/admin** role: observe feed, manage peers, control the daemon. This is fundamentally different from Familiar's **personal companion** role.
- Component model doc explicitly defines Web-UI as "Observer" -- observes and controls, doesn't act autonomously.

**Risks**:

- Overlap with Familiar is minimal today but could grow. If Familiar gets a web channel, some admin features might migrate there.
- The app is "private" (package.json), suggesting it's single-user tooling, not a public product.
- Test files exist (tasks.test.ts: 711 lines, traces.test.ts: 437 lines, validation.test.ts: 796 lines, useSchemas.test.ts: 458 lines) -- about 2,400 lines of tests. Well-maintained.

**Unknowns**:

- Whether you use the web UI regularly or if it was a one-time build.
- Whether admin functions (peer management, systemd control) will be exposed through Familiar's REPL.

**Recommendation**:

- Keep for now. It fills the "operator dashboard" role that neither servitor nor familiar targets.
- Revisit in 3-6 months: if Familiar grows a web channel with admin capabilities, merge the relevant features.

---

### 8. docs/ (mdBook documentation)

**Position**: KEEP.

**Evidence**:

- mdBook site with 15 markdown source files covering specification, architecture, deployment topologies, and per-project documentation.
- Built output in `book/` directory (HTML, JS, CSS, fonts).
- Well-structured SUMMARY.md with clear navigation.
- Covers egregore and servitor in depth. Does NOT yet cover Familiar.
- Includes research docs (multi-agent deployment patterns, trust cartographer).

**Risks**:

- The `book/` build output is committed to the repo. This is unusual -- most projects gitignore build artifacts and use CI to deploy docs.
- Missing Familiar documentation means the component model is incomplete.

**Unknowns**:

- Whether this is deployed anywhere (GitHub Pages, etc.) or local-only.

**Recommendation**:

- Keep. Add Familiar section. Consider gitignoring `book/` and building in CI.

---

## Cross-Cutting Analysis

### Q1: Should ~1,500 lines duplicated between Familiar and Servitor be extracted?

**Answer: Yes, urgently. The actual duplication is closer to 2,200 lines.**

Duplicated modules (confirmed by diff):

| Module | Familiar Lines | Servitor Lines | Difference |
|--------|---------------|----------------|------------|
| identity/ | 203 | 203 | Error type name only |
| mcp/client.rs | 171 | 171 | Identical |
| mcp/circuit_breaker.rs | 254 | 254 | Identical |
| mcp/pool.rs | 475 | 543 | Servitor adds scope enforcement |
| mcp/stdio.rs | 223 | 223 | Identical |
| mcp/http.rs | 126 | 126 | Identical |
| agent/providers/mod.rs | 231 | 234 | Servitor adds Codex |
| agent/providers/anthropic.rs | 146 | 146 | Identical |
| agent/providers/openai.rs | 330 | 330 | Identical |
| agent/providers/claude_cli.rs | 254 | 268 | Minor differences |
| **Total** | **~2,413** | **~2,498** | |

**Proposed extraction: `thallus-core` crate**

```
thallus-core/
  Cargo.toml
  src/
    lib.rs
    error.rs          # Generic error trait or enum
    identity/
      mod.rs          # Ed25519 keypair, SSB wire format
    mcp/
      client.rs       # McpClient trait + types
      circuit_breaker.rs
      pool.rs         # McpPool (scope as optional/generic)
      stdio.rs        # StdioMcpClient
      http.rs         # HttpMcpClient
    providers/
      mod.rs          # Provider trait + types
      anthropic.rs
      openai.rs
      claude_cli.rs
```

The crate would:

- Define a generic `Error` trait or use `thiserror` with a parameterized error type
- Make scope enforcement in McpPool optional (feature flag or trait-based)
- Keep Codex provider in servitor (servitor-specific) until/unless Familiar needs it
- Both servitor and familiar depend on `thallus-core` as a path dependency

This reduces Familiar from 4,200 to ~1,800 lines of genuinely unique code and ensures bug fixes propagate automatically.

### Q2: Do three separate client SDKs (js, py, rs) make sense?

**Answer: No. Remove all three.**

Evidence:

- None are used by any project in the ecosystem
- None are published to their respective registries
- egregore-web builds its own HTTP client layer through Tauri
- servitor and familiar each implement their own egregore client
- egregore itself exposes an MCP server (JSON-RPC 2.0) which is a better integration surface than language-specific HTTP wrappers

The SDKs solve a problem that does not yet exist (third-party developers integrating with egregore). When that problem materializes, the MCP protocol is a better answer because:

1. One MCP server implementation covers all languages
2. MCP is an emerging standard with existing client libraries
3. SDKs would need to track every API change across three languages

### Q3: Does egregore-web serve a purpose now that Familiar exists?

**Answer: Yes, different purposes.**

| Capability | egregore-web | familiar |
|------------|-------------|----------|
| Feed browsing/search | Full UI with threading | Via egregore MCP tools |
| Peer management | Add/remove/health dashboard | Not implemented |
| Schema management | List/register/validate | Not implemented |
| Consumer groups | Full CRUD | Not implemented |
| Retention policies | Full CRUD | Not implemented |
| Systemd control | Start/stop/enable/install | Not implemented |
| Config editing | YAML editor with backup | Not applicable |
| Task management | Queue/detail/offers | Not implemented |
| Trace visualization | Waterfall/span view | Not implemented |
| Interactive conversation | N/A | Core feature |
| Tool execution | N/A | Via MCP pool |
| Proactive health checks | N/A | Heartbeat system |
| Personal context | N/A | Conversation history |

egregore-web is an **admin dashboard**. Familiar is a **personal assistant**. There is almost no overlap today.

### Q4: Should servitor's Discord transport move to Familiar?

**Answer: Yes, eventually, but not urgently.**

Argument for moving:

- Discord is a human communication channel. Familiar's Channel trait (`channel/mod.rs`) was explicitly designed for this: "Implement this for REPL, Discord, Slack, HTTP, Matrix, etc."
- Servitor's Discord transport (`comms/discord.rs`, 336 lines) integrates with the authority model (person/place/skill). Moving it means Familiar would need some form of authorization.
- The `serenity` crate is a heavy dependency. Removing it from servitor simplifies that binary.

Argument for waiting:

- Familiar was built today. It needs to stabilize before absorbing more transports.
- Servitor's Discord integration is coupled to its authority model and task execution. Familiar doesn't have either.
- The move requires Familiar to gain: (a) a way to delegate tasks to servitor, (b) authorization awareness, (c) the comms abstraction beyond just Channel.

**Recommendation**: Plan for it. Build Familiar's Channel implementation for Discord as a separate step after the shared crate extraction. Then deprecate servitor's Discord transport.

---

## Recommended Action Plan (Priority Order)

1. **Extract `thallus-core` crate** (blocks everything else)
   - Create workspace at `/home/pknull/Code/Thallus/thallus-core/`
   - Move identity, MCP, and provider modules
   - Update servitor and familiar to depend on it
   - Estimated effort: 1-2 sessions

2. **Archive the three SDKs**
   - Move egregore-js, egregore-py, egregore-rs to an `_archived/` directory or delete
   - Update CLAUDE.md to remove references
   - Estimated effort: minutes

3. **Update docs**
   - Add Familiar to `docs/architecture/component-model.md`
   - Add Familiar section to `docs/SUMMARY.md`
   - Update umbrella `CLAUDE.md` project table
   - Estimated effort: 1 session

4. **Break up oversized files** (maintenance debt)
   - egregore: `engine.rs` (1,203), `schema.rs` (1,076), `main.rs` (892), `cli_admin.rs` (841)
   - servitor: `agent/loop.rs` (1,060), `egregore/messages.rs` (886)
   - Estimated effort: 2-3 sessions

5. **Plan Discord migration** to Familiar (future)
   - Define Channel::Discord implementation
   - Design authorization pass-through
   - Deprecation timeline for servitor's comms module

---

## Architecture Diagram (Post-Extraction)

```
                    HUMAN (Keeper)
                        |
         +--------------+--------------+
         |              |              |
    [familiar]     [egregore-web]  [servitor]
    interactive    admin dashboard  headless executor
    companion      (observe/ctrl)   (act/attest)
         |              |              |
         +---+     +----+        +----+---+
             |     |             |        |
         [thallus-core]         |    [authority]
         identity, MCP,         |    [a2a]
         providers              |    [scope]
                                |    [events]
                                |    [session]
                                |
                           [egregore]
                           feed daemon
                           gossip, crypto
                           storage, API

    [docs] -- mdBook documentation (all projects)
```

---

## Final Assessment

The ecosystem has a clean separation of concerns at the top level: egregore is the pipe, servitor is the actor, familiar is the companion, egregore-web is the dashboard. The three SDKs are dead weight. The urgent problem is the 2,200-line duplication between familiar and servitor, which will become a source of bugs within weeks if not extracted into a shared crate.
