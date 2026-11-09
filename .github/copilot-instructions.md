# xsh

The command is `xsh`. Internal crates keep the `rushy-` names (renaming them would only churn imports).

AI-native shell and terminal multiplexer in Rust (edition 2024). Library crates: MSRV 1.85. `rushy-cli`: Rust 1.95 (reedline 0.52 needs it).

## Layout
- `crates/rushy-core`: parser, executor, jobs, state.
- `crates/rushy-config`: layered config (`Paths`, `Loader`, `parse_toml` with line numbers).
- `crates/rushy-history`: encrypted SQLite history, crypto (`Key`, `KeySource`, `Cipher`), fuzzy ranking.
- `crates/rushy-bundles`: manifest, registry client, store, activation (`Effects`).
- `crates/rushy-ai`: providers, safety guard (`safety.rs`), `Assistant`, agent loop, encrypted transcripts.
- `crates/rushy-pty`: panes, layout tree, `SessionManager`, multiplexer server and protocol.
- `crates/rushy-tui`: themes, prompt, palette, status bar, mux drawing, config watcher.
- `crates/rushy-cli`: the `xsh` binary (`repl.rs`, `exec.rs`, `cmds.rs`, `client.rs`, `keys.rs`, `man.rs` for man-page option suggestions).
- `plugins/`: 14 reference bundles. `registry.json` holds their SHA-256 hashes.
- `docs/manual-tests.md`: checks that need a real terminal.
- `assets/rust.svg`: project icon.

## Rules
- No `unwrap()`, `expect()`, `panic!`, `unreachable!` in library crates (tests included).
- `#![forbid(unsafe_code)]` and `#![deny(missing_docs)]` in every library crate.
- `#![warn(clippy::pedantic)]`; CI runs clippy with `-D warnings`. Doc words that need no backticks go in `clippy.toml`.
- Errors: `thiserror` in libraries, `anyhow` in `rushy-cli` only.
- Bundles are declarative. Never add a way for a bundle to run arbitrary shell code.
- Secrets: manual `Debug` that prints `<redacted>`; never log or print keys.
- Data files must never live inside the config directory (it is watched for hot reload).
- After editing `plugins/*`, update the hashes in `registry.json` (the test `shipped_registry_matches_plugins` shows the new values).

## Commands
- `make run`: run the `xsh` binary.
- `make build`: `cargo build --release`.
- `cargo test --workspace`, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`.

## Testing notes
- CLI tests isolate state with `XSH_CONFIG_DIR`, `XSH_DATA_DIR`, `XSH_HISTORY__KEY_SOURCE=passphrase`, and `XSH_HISTORY_PASSPHRASE`. Use these to avoid the OS keychain.
- Tests that need a PTY or a local socket print `skipped: ...` and pass when the environment forbids it. Check for that line before you trust a green run.
- Snapshot tests in `rushy-ai` use a small helper (no `insta`): set `UPDATE_SNAPSHOTS=1` to rewrite `tests/snapshots/*.snap`.
