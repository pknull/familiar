# Contributing to Familiar

Thanks for your interest. Familiar is the conversational companion for [Thallus](../) — it translates natural language into egregore actions, publishing through the local node.

## Before You Start

- Read the [README](README.md) for what Familiar does.
- Read [CLAUDE.md](CLAUDE.md) for module layout.
- For large changes (new channels, LLM providers, daemon behaviours), open an issue first.

## Development Setup

```bash
git clone <repo>
cd familiar
cargo build
cargo test
./target/release/familiar init
# Edit ~/.familiar/familiar.toml to add your API key and egregore path
./target/release/familiar
```

Familiar depends on `thallus-core` via path dependency. Build from the Thallus workspace root or ensure `thallus-core/` is a sibling directory.

## Pre-Submit Checklist

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build --release
```

CI also runs `cargo audit`.

## Areas That Need Care

### Node-Mediated Publishing
Familiar does not own a separate network signing identity. Anything touching `src/egregore/`, daemon content construction, or feed-publish flows needs to preserve the invariant that egregore is the signing principal and Familiar is only shaping requests sent through the local node.

### LLM Providers
New LLM providers go in `thallus-core` (shared across Familiar and future consumers), not directly in Familiar. Familiar consumes the `Provider` trait; it doesn't define its own.

### Conversation Loop
`src/agent/conversation.rs` is the main orchestration loop. It's large. Changes there should add tests, not just rely on manual validation.

### Storage
SQLite is bundled with sqlcipher for encryption. Don't swap the backend without discussion.

## Code Style

- Rust 2021 edition
- `cargo fmt`, `cargo clippy --all-targets`
- `thiserror` for `FamiliarError`, `anyhow` elsewhere as appropriate
- Tokio async
- Keep config-driven behaviour in `src/config/`; don't sprinkle constants across modules

## New Channels

To add a new channel (after REPL, Discord, TUI):

1. Implement the `Channel` trait in `src/channel/`
2. Wire it up in `src/main.rs` under a new `Commands` variant if it needs its own entry point
3. Document the config section in `[<channel>]` under the README config table
4. Add tests for the channel-specific logic

## Pull Request Process

1. Fork and branch from `master`
2. Make your change; add tests where practical
3. Run the pre-submit checklist
4. Open a PR with a clear description
5. Solo maintainer — review turnaround varies

## License

By contributing, you agree that your contributions will be licensed under [MIT OR Apache-2.0](../LICENSE-MIT).
