# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **xsh.app**: a native macOS terminal app (AppKit, SwiftUI, SwiftTerm) with windows, native tabs, split panes, profiles, session restore, find, a global hotkey, a default-terminal option, a Finder service, and `ssh://` links.
- A dedicated Settings window with search. It covers the app, the shell, the multiplexer, history, bundles, and AI.
- `config.toml` sections for the app: `[window]`, `[font]`, `[colors]`, `[terminal]`, `[keys]`, and `[[profiles]]`.
- `xsh app`: a hidden bridge between the app and the settings.
- `make deploy`: builds, commits with a message from a local model, pushes, and publishes a GitHub release.
- Updates from GitHub releases: a daily check and **Check for Updates…**. A download must match its checksum and contain a signed app of the right version, and xsh asks before it installs.
- A new app icon: a dark terminal screen with a prompt, in a metal frame.
- Split panes have a title bar with a close button. Panes without focus are no longer dim by default, and a click on any pane gives it the focus.
- In the app, `ai agent` opens a pane to the right for its commands. The left pane shows the model's text and the approvals.
- Agent autopilot: `ai agent --auto`, `Alt-A`, or the `autopilot` setting. Commands run without a question, up to 50 steps, and the safety checks stay on.
- `Ctrl-C` stops an agent run in either pane, a wait for the model, and a question from the agent.
- A `custom` AI provider for any OpenAI-compatible service. In Settings > AI, enter the endpoint (such as `http://127.0.0.1:9694/v1`), click Fetch Models, and select a model.
- Themes can set `cursor`, `selection`, and 16 `ansi` colors. New built-in themes: `dracula`, `nord`, `solarized-dark`, `solarized-light`.
- Shell core: quote-aware parser, pipes, redirection, `$VAR`, `${VAR}`, `$?`, `$$`, aliases, background jobs, and the built-ins `cd`, `pwd`, `exit`, `export`, `unset`, `alias`, `unalias`, `history`, `jobs`, `fg`, `bg`, `kill`, `help`.
- `SIGTERM` and `SIGHUP` are forwarded to the running foreground command.
- Interactive prompt with line editing, `\` and open-quote continuation, Tab completion, `Ctrl-R` fuzzy history search, and a `Ctrl-K` command palette.
- Bundle system: `bundle install|remove|list|search|update|info`, `bundle.toml` schema, static `init.rushy`, registry client with cache and offline fallback, SHA-256 and host allowlist checks.
- 14 reference bundles: git, rsync, adb, npm, pip, postgres, sbt, scala, ubuntu, node, python, history, vscode, docker.
- AI layer: OpenAI, Anthropic, and Ollama providers, `Ctrl-Space` command suggestion, agent mode (`Ctrl-Shift-Space` or `Alt-G`, 10-step cap, approval per step), `ai explain`, `ai fix`, `ai suggest`, `ai agent`, `ai status`.
- Safety guard for AI-made commands and bundle hooks: rule-based checks, plus a second model review for AI-made commands.
- Encrypted history (SQLite, XChaCha20-Poly1305, keychain or Argon2id key) with dedupe, pause, JSON export and import.
- Encrypted AI transcripts.
- Multiplexer: sessions, windows, split panes, focus, resize, kill, detach, and `xsh attach`, with a background server.
- Themes (TOML), `mono` theme, `NO_COLOR`, `--screen-reader`, status bar, hot reload of config and bundles.
- Layered configuration with located error messages.
- Release workflow for macOS (x86_64 and aarch64).
- Option suggestions from man pages: `-` then `Tab` offers the options of the command being typed, with descriptions. Man pages are read in the background after each command.

### Changed

- xsh is macOS only. Windows and Linux code, CI jobs, and release builds are removed. The multiplexer uses Unix sockets directly, so `interprocess` is removed.
- `make run` and `make build` now create `target/xsh.app`.
- A change to app-only settings no longer prints "configuration reloaded" in the shell.
- The command is now `xsh`. Messages start with `xsh:`, the config and data folders are named `xsh`, environment variables start with `XSH_`, and the project file is `.xsh.toml`. The internal crates keep the `rushy-` names and bundles keep `init.rushy` and `min_rushy_version`.
- Data files (history, AI transcripts, registry cache, multiplexer socket) now live under `$XDG_DATA_HOME/xsh` or `~/.local/share/xsh`. History from an earlier build in `~/Library/Application Support/rushy` is not moved.

### Fixed

- "configuration reloaded" printed after every command on macOS: the data folder was inside the watched config folder, so each history write looked like a config edit. The folders are now separate, and the watcher ignores database, socket, and cache files.

### Known limits

- `rushy-cli` needs Rust 1.95 (the line editor requires it); the library crates declare 1.85.
- The PTY, socket, keyboard, and terminal-drawing paths are covered by `docs/manual-tests.md`, because automated tests cannot run them in every environment.
