# Changelog

All notable changes to familiar are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this crate's pre-1.0 versioning treats minor bumps as the breaking-change signal.

## [0.5.0] - 2026-04-27

### ⚠ Breaking

- **Familiar no longer owns its own signing identity.** The `[identity]` section of `~/.familiar/familiar.toml` and the `IdentityConfig` struct have been removed. Public feed authorship now comes from the local Egregore node, which is the sole network signing principal in the post-reconciliation contract. Existing config files containing an `[identity]` block continue to parse — the section is silently ignored — but the `secret_key` path it pointed at is no longer consulted.
- **`pub mod identity` removed from the crate.** Code that imported `familiar::identity::Identity` should switch to `thallus_core::identity::Identity`. Within familiar's own code paths, the daemon now obtains its routing identifier by calling `EgregoreClient::get_public_id()` against the local node's `GET /v1/identity`.

### Added

- `EgregoreClient::get_public_id()` queries `GET /v1/identity` on the local Egregore node and returns the node's canonical public ID for routing / correlation.
- `README.md`, `CONTRIBUTING.md`, dual `LICENSE-APACHE` / `LICENSE-MIT`, and `.github/workflows/ci.yml`.

### Changed

- `egregore_publish` tool description now reads "publish through the local node. Public feed authorship comes from the local egregore node identity" instead of "Content is signed under your person's identity."
- `familiar init`'s emitted example config no longer contains an `[identity]` block.
- `tests/smoke.rs` no longer generates Ed25519 keypair files during config setup.
- `cargo fmt` sweep across the daemon, channels, hooks, profile, and conversation paths. No semantic changes beyond what's listed above.

## [0.4.0] - prior

Earlier history is preserved in `git log`. Highlights: streaming, hooks, cost tracking, smart compaction, agent communication; runtime depth via profile wiring, heartbeat triggers, and session lifecycle; store schema updates for profile and published task tracking.
