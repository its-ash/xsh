# Contributing

Thank you for helping. This page lists the rules and the commands.

## Set up

```sh
git clone https://github.com/its-ash/xsh
cd xsh
cargo build --workspace
swift build --package-path app
```

xsh is macOS only. The library crates target Rust 1.85. `rushy-cli` needs 1.95. The app in `app/` needs Swift 6.2 and macOS 14.

To add a setting: add the field in `crates/rushy-config/src/lib.rs`, one line in `app/Sources/XshApp/SettingsCatalog.swift`, and the code that uses it.

## Before you open a pull request

Run all four. CI runs them on macOS.

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

Also run the relevant parts of [docs/manual-tests.md](docs/manual-tests.md) when you change the prompt, the multiplexer, or key handling. Automated tests cannot cover those.

## Rules

- No `unwrap()`, `expect()`, `panic!`, or `unreachable!` in library crates. Return a `Result`.
- `#![forbid(unsafe_code)]` and `#![deny(missing_docs)]` in every library crate. If a platform API truly needs `unsafe`, put it in one module with a `SAFETY:` comment and discuss it first.
- `#![warn(clippy::pedantic)]`. Fix a lint or add a narrow `#[allow(..., reason = "...")]`.
- Errors: `thiserror` in libraries; `anyhow` only in `rushy-cli`.
- Do not block the async runtime. Use `spawn_blocking` for blocking work.
- Every public type has a `Debug` impl. A type that holds a secret implements `Debug` by hand and prints `<redacted>`.
- Network: HTTPS only (rustls), and check the host. Do not add OpenSSL.
- Keep secrets out of logs, errors, and test output.
- Every user-facing text uses plain, short sentences.

## Layout

| Crate | Purpose |
| --- | --- |
| `rushy-core` | parser, executor, jobs, state |
| `rushy-config` | layered configuration |
| `rushy-history` | encrypted history and fuzzy search |
| `rushy-bundles` | manifests, registry client, activation |
| `rushy-ai` | providers, safety guard, agent loop |
| `rushy-pty` | panes, sessions, multiplexer server |
| `rushy-tui` | themes, prompt, palette, status bar, drawing |
| `rushy-cli` | the `rushy` binary |

## Adding or changing a bundle

1. Edit `plugins/<name>/bundle.toml` (and `init.rushy`, `README.md`).
2. Run `cargo test -p rushy-bundles`. The test `shipped_registry_matches_plugins` fails when a hash in `registry.json` is stale; its message shows the new value. You can also run `shasum -a 256 plugins/<name>/*`.
3. Copy the new hashes into `registry.json`.
4. Bump `version` when behavior changes.

A bundle may only use the fields of the schema. It cannot run scripts. `init.rushy` holds only `alias NAME='value'` and `export NAME=value` lines.

## Tests that skip themselves

Some tests need a PTY or a local socket. They print `skipped: ...` and pass when the environment forbids it, for example inside a sandbox. Run them in a normal terminal at least once before a release:

```sh
cargo test -p rushy-pty -p rushy-cli -- --nocapture | grep skipped
```

No `skipped` line means they really ran.

## Commits and releases

Write a short commit message in the imperative mood. To release: update `CHANGELOG.md`, start the local model, and run `make deploy` (add `VERSION=X.Y.Z` for more than a patch version). It runs the checks, builds both architectures, commits with a message from the model, pushes `main` and the tag, and publishes the GitHub release. If a release must be built on GitHub instead, run the release workflow by hand on the tag.

## License

By contributing you agree that your work is licensed under Apache-2.0 OR MIT, like the rest of the project.
