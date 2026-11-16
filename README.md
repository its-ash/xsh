# xsh

An AI-native terminal app and shell for macOS. It replaces Terminal and iTerm. The shell is written in Rust, and the app is native AppKit and SwiftUI.

- A **terminal app**: windows, native tabs, split panes, profiles, full color themes, GPU drawing, find, a global hotkey, and a **Settings window** for everything.
- A small shell: pipes, redirection, quoting, jobs, aliases, `$VAR`, `$?`, `$$`.
- Declarative **bundles** (plugins) for aliases, variables, prompt segments, and completions. They never run arbitrary shell code.
- **AI help** in the prompt: a command from a description, an agent that asks before every step, `explain`, and `fix`. Every AI command passes a safety guard first.
- A tmux-style **multiplexer**: sessions, windows, split panes, detach and re-attach.
- **Encrypted history** with fuzzy `Ctrl-R` search.
- **Fuzzy Tab completion** for commands, flags, folders, files, branches, and more: `cd dwn` finds `Downloads/`.
- Themes (the default is `neon`), a **theme editor** in the browser (`make theme`), a command palette, a status bar, and hot reload of config.

```text
ash@laptop ~/code/rushy  main* ❯ gs
## main...origin/main
 M README.md
ash@laptop ~/code/rushy  main* ❯ ai explain tar -xzf backup.tgz -C /tmp
Unpacks backup.tgz (gzip-compressed tar) into /tmp.
  -x extract   -z gunzip   -f read this file   -C change to /tmp first
```

Two panes in `--screen-reader` style (plain ASCII borders). The default style uses box-drawing characters and a `●` on the focused pane:

```text
+ %1 -------------------------++ %2 [active] --------------+
|ash@laptop ~ ❯ cargo test    ||ash@laptop ~ ❯ htop          |
|running 42 tests ... ok      ||                            |
+-----------------------------++----------------------------+
[work] win 1/2 pane %2 | bundles 3 | ai: anthropic     14:05
```

## Install

xsh runs on macOS 14 or later, on Apple silicon and Intel. From source (needs Rust **1.95** or newer and the Xcode command-line tools):

```sh
git clone https://github.com/its-ash/xsh
cd xsh
make install               # builds, copies xsh.app to /Applications, links /usr/local/bin/xsh
```

`make install` builds the release app, puts it in `/Applications`, and makes `/usr/local/bin/xsh` a link to the `xsh` inside it, so app updates update the command too. It asks for your password (sudo) only if `/usr/local/bin` is not writable, and it refuses to replace a `/usr/local/bin/xsh` that is not xsh. Choose other places with `make install APP_DIR=~/Applications BIN_DIR=~/.local/bin`. `make uninstall` removes both and keeps your settings, history, and bundles. `make build` alone only creates `target/xsh.app`.

For the shell only: `cargo install --path crates/rushy-cli --locked`.

Release builds (macOS x86_64 and aarch64) are attached to each GitHub release together with a `.sha256` file. Check the checksum before you run the app. The builds have an ad hoc signature, not a Developer ID, so the first start needs right-click > Open.

The app looks for a new release once a day and asks before it installs one. Use **xsh > Check for Updates…** to look now, or turn off the check in Settings > General. The update replaces only `xsh.app`: a copy of `xsh` that you installed elsewhere stays as it is.

## The app

Open **xsh.app**, then press `⌘,` for Settings. All settings are in the file `config.toml`: a change in the window, in the file, or with `xsh config set` shows in open windows at once.

| Page | What you can change |
| --- | --- |
| General | updates, last tabs at launch, folder of new tabs, close questions, default terminal, command-line tool |
| Appearance | themes, light and dark themes, import, color overrides, the 16 ANSI colors |
| Text | font, size, line height, smoothing, bold in bright colors, minimum contrast |
| Window | size, padding, title bar, tab bar, opacity, blur, always on top, dim panes (off by default) |
| Terminal | program, arguments, environment, `TERM`, scrollback, cursor, Option as Meta, mouse, copy on select, paste question, links, scrolling, bell, GPU |
| Shortcuts | every menu command and a global show or hide hotkey |
| Profiles | named sessions with their own program, folder, theme, font, and environment |
| Shell | key mode, suggestions, colors, `cd`, notices, start-up file, accessibility |
| Multiplexer | prefix key, pane program, status bar |
| History | recording rules, encryption key source, pause, export, delete |
| Bundles | enable, install, remove, registry, hooks, allowed hosts |
| AI | provider with its own model, address, and API key fields (keys are in the Keychain), model list for custom providers, safety review, request context |
| Advanced | open the files, every setting as raw TOML, reset |

**Full screen** (`⌃⌘F`, the View menu, or the green button) makes the window cover the screen below the camera housing, hides the title and the window drag, and hides the menu bar and Dock until the pointer reaches them. The close, minimize, and zoom buttons stay. xsh does not use the separate full-screen Space of macOS: there, macOS showed typed characters hundreds of milliseconds late.

The default font is Consolas when it is installed (it comes with Microsoft Office), and the system monospaced font otherwise.

With more than one pane in a tab, each pane has a bar with its title and a close button. Click a pane to type in it. In the app, `ai agent` and `ai claude` open a pane to the right: the left pane shows the model's text and asks for approval, and the right pane runs the commands. The right pane closes when the agent stops; the left pane keeps the end of each command's output.

The search field finds a setting on any page. **General > Make xsh the Default** opens scripts (`.command`, `.tool`) and `ssh://` links in xsh. Finder also gets a **New xsh Tab Here** service, and you can drop files on a pane to paste their paths.

A repository's `.xsh.toml` cannot change the app settings. It can only set `[shell]`, `[ui]`, and `[mux]`.

## Quick start

```sh
xsh                        # start the shell in any terminal
xsh -c 'echo hi | wc -c'   # run one command
xsh bundle install git     # install a bundle from the registry
xsh new work               # start a multiplexer session
xsh attach work            # come back to it later
```

## Feature matrix

| Area | What you get | Checked by |
| --- | --- | --- |
| Shell | pipes, `>` `>>` `<`, quotes, `\` continuation, `&` jobs, `jobs`/`fg`/`bg`/`kill`, `cd` `pwd` `exit` `export` `unset` `alias` `unalias` `history` `help` | automated tests |
| Signals | `SIGTERM` is forwarded to the running command; `Ctrl-C` reaches it through the terminal | automated test (`SIGTERM`); manual (`Ctrl-C`) |
| Line editor | editing, multi-line input, Tab completion, `Ctrl-R`, `Ctrl-K` | manual checklist |
| Completion | prefix, any-case, and fuzzy matches for commands, subcommands, flags, files, folders, branches, make targets, npm scripts, hosts, variables | automated tests |
| Bundles | `install` `remove` `list` `search` `update` `info`, registry cache with offline fallback, SHA-256 and host checks | automated tests |
| Reference bundles | git, rsync, adb, npm, pip, postgres, sbt, scala, ubuntu, node, python, history, vscode, docker | automated tests |
| AI | `Ctrl-Space`, `Ctrl-Shift-Space` / `Alt-G`, `ai explain`, `ai fix`, safety guard, 10-step cap, encrypted transcripts | automated tests for the logic; the CLI flow runs in CI only where sockets are allowed; keys are manual |
| Providers | OpenAI, Anthropic, Ollama | request bodies by snapshot tests; live calls are manual |
| Multiplexer | sessions, windows, splits, focus, resize, kill, detach, attach | layout, protocol, and key handling by automated tests; the PTY and socket round trip runs in CI only where allowed |
| History | SQLite, XChaCha20-Poly1305, keychain or Argon2id key, dedupe, pause, export/import, fuzzy search | automated tests |
| Themes and UI | TOML themes, `mono`, `NO_COLOR`, `--screen-reader`, palette, status bar, hot reload | automated render tests |
| Config | layered: defaults, user, project, `XSH_*`, flags; errors with line numbers | automated tests |

## Files

| Path | Purpose |
| --- | --- |
| `config.toml` | settings (see below) |
| `ai.toml` | AI provider and context settings |
| `themes/*.toml` | your themes |
| `bundles/<name>/` | installed bundles |
| `<data>/history.db` | encrypted history |
| `<data>/ai/` | encrypted AI transcripts |
| `<data>/mux/` | multiplexer socket |

The config directory is `~/Library/Application Support/xsh`. The data directory is `$XDG_DATA_HOME/xsh` (default `~/.local/share/xsh`). The two are never the same folder, because the config folder is watched for hot reload. `XSH_CONFIG_DIR` and `XSH_DATA_DIR` override both.

## Configuration

Layers, later ones win: defaults, `config.toml`, `.xsh.toml` in the working directory, `XSH_<SECTION>__<KEY>` variables, command-line flags.

```toml
[shell]
capture_stderr = true       # lets `ai fix` see error text (see the note below)

[ui]
theme = "neon"
screen_reader = false
status_bar = true
hot_reload = true

[mux]
prefix = "C-b"
shell = ""                  # empty: panes run xsh

[bundles]
registry_url = "https://raw.githubusercontent.com/its-ash/xsh/main/registry.json"
allowed_hosts = ["raw.githubusercontent.com", "github.com", "objects.githubusercontent.com"]
allow_hooks = true
disabled = []

[history]
enabled = true
key_source = "auto"         # auto | keyring | passphrase
```

A project file (`.xsh.toml`) may only set `[shell]`, `[ui]`, and `[mux]`. A repository you clone cannot change the registry or the history settings. Example variable: `XSH_UI__THEME=mono`.

Note on `capture_stderr`: when it is on and xsh runs in a terminal, a foreground command's standard error is a pseudo-terminal that xsh reads, so programs still see a terminal and keep their colors and progress bars (`git clone`, `curl`, `cargo`). xsh prints everything unchanged. Set it to `false` if you see a difference; `ai fix` then has no error text to send.

### AI

```toml
# ai.toml
[default]
provider = "anthropic"        # openai | anthropic | ollama | custom
model = "claude-opus-5-5"
temperature = 0.2
max_context_tokens = 8000
safety_review = true          # second opinion from the model
approve_each_step = true      # agent mode asks before every command
autopilot = false             # agent mode asks nothing, up to 50 steps; safety checks stay on

[providers.anthropic]
api_key = { keyring = "xsh-anthropic" }   # or { env = "ANTHROPIC_API_KEY" }

[providers.ollama]
base_url = "http://localhost:11434"
model = "llama3.1:8b"

[providers.custom]                          # any OpenAI-compatible service
base_url = "http://127.0.0.1:9694/v1"      # Settings > AI > Fetch Models reads <base_url>/models
model = "my-model"
# api_key = { keyring = "xsh-custom" }      # optional

[context]
listing = true      # depth-1 directory listing
shell = true        # $SHELL
bundles = true      # active bundle names
history = 20        # last N commands
git_status = true   # for `ai fix`
```

Store a key in the OS keychain with the platform tool, for example on macOS:
`security add-generic-password -s xsh-anthropic -a xsh -w`.

Every request includes the context you enable above. Prompts and transcripts are stored only on your machine, encrypted. xsh never logs API keys and prints them nowhere.

`ai --model NAME ...` uses another model for one command, such as `ai --model gpt-4o explain ls`. At the prompt, `ai <Tab>` lists the modes and `ai --model <Tab>` lists the models of the configured provider (for Ollama, also the models you pulled).

### Claude Code as the agent

`ai claude <goal>` (or `ai claude --auto <goal>`, `ai claude --model sonnet <goal>`) gives the goal to [Claude Code](https://claude.com/claude-code), which must be installed and signed in. Claude Code starts with all of its own tools turned off: no Bash, no file tools, no web access, and no MCP servers but one. That one is `xsh mcp`, which sends every tool call back to your xsh session:

| Tool | What it does | Approval |
| --- | --- | --- |
| `run_command` | runs a command in your shell (or the app's agent pane) | safety guard, then you (unless `--auto`) |
| `write_file`, `edit_file` | change a file inside the working directory | you (unless `--auto`) |
| `read_file`, `list_dir`, `find_files`, `search_text` | read inside the working directory | none |
| `shell_info` | working directory, last failure, recent commands, active bundles | none |
| `check_command` | asks the safety guard about a command without running it | none |

The file tools refuse paths outside the working directory, including through symbolic links. Set `XSH_CLAUDE` to use a `claude` program that is not on `PATH`.

## Keys

| Key | Action |
| --- | --- |
| `Tab` | complete commands, bundle subcommands, files; when nothing starts with the word, fuzzy matches (`cd dwn` → `Downloads/`) |
| `-` then `Tab` | options of the command you are typing, read from its man page, each with a short description |
| `Ctrl-R` | fuzzy history search under the prompt |
| `Ctrl-K` | command palette: built-ins, bundles, aliases, themes, AI actions, recent commands |
| `Ctrl-Space` | turn the text on the line into a command; confirm; insert or run |
| `Ctrl-Shift-Space`, `Alt-G` | agent mode for the text on the line (`Ctrl-Shift-Space` needs a terminal with the kitty keyboard protocol) |
| `Alt-A` | agent on autopilot for the text on the line: no questions, up to 50 steps, safety checks stay on |
| `Ctrl-C` | stops the running command, the agent, or a wait for the model |
| `Ctrl-b %` / `Ctrl-b "` | split side by side / top and bottom |
| `Ctrl-b h j k l`, `Ctrl-b` + arrows | move focus |
| `Ctrl-b Ctrl-` + arrows (or `Alt`) | resize |
| `Ctrl-b c`, `n`, `p` | new window, next, previous |
| `Ctrl-b x` | kill the pane |
| `Ctrl-b d` | detach |

## Option suggestions from man pages

After you run a command, xsh reads its man page in the background and keeps the options in memory. Later, type `-` and press `Tab` to see that command's options with a short description:

```text
❯ tar -<Tab>
--create  create a new archive    -x  extract files from an archive    ...
```

Options match fuzzily too: `git commit --nv<Tab>` finds `--no-verify`. For `git commit -<Tab>` it uses the `git-commit` page when one exists. If a page was not read yet, xsh reads it on the spot (at most 2 seconds). There is no cache on disk, and nothing is sent anywhere. If the command has no man page, `Tab` completes file names as before.

## Bundles

```sh
xsh bundle install <name>
xsh bundle remove <name>
xsh bundle list
xsh bundle search <query>
xsh bundle update <name>   # or --all
xsh bundle info <name>
```

A bundle is a directory with a `bundle.toml`, an optional `init.rushy`, and a README. See [`plugins/`](plugins) for 14 examples and the schema in `crates/rushy-bundles/src/manifest.rs`.

```toml
[package]
name = "git"
version = "0.1.0"
description = "Git prompt segment, aliases, and completions."
min_rushy_version = "0.1.0"

[bundle]
detect = ["**/.git"]          # all conditions must match
requires_commands = ["git"]

[env]
GIT_PAGER = "less -FRX"

[aliases]
gs = "git status --short --branch"

[prompt]
segment = "git"
color = "cyan"
format = " {branch}{dirty} "
priority = 80                 # higher stays when the prompt is too wide
max_width = 32

[completion]
commands = ["git"]

[[hooks]]
on = "activate"
run = "git status --short"
```

### Why bundles cannot run shell code

Rushy is not bash. A bundle can only add aliases, variables, one prompt segment, completion words, and **hook commands**. `init.rushy` may hold only literal `alias` and `export` lines; it is parsed and never executed. Every hook command goes through the rule-based safety guard before it runs (the extra model review is only for AI-made commands), and `bundles.allow_hooks = false` turns hooks off. Downloads must use HTTPS from an allowed host, are limited to 1 MiB, and must match the SHA-256 in the registry index.

### Registry

`registry.json` lists the bundles: `name`, `version`, `description`, `repo`, `sha256` of `bundle.toml`, `url`, and the extra `files` with their hashes. The client caches the index and falls back to the cache when you are offline. The URL in this repository is a placeholder until you publish your own registry.

## Themes

```toml
# ~/Library/Application Support/xsh/themes/mine.toml
name = "mine"
[colors]
background = "#1a1b26"
foreground = "#c0caf5"
accent = "#7aa2f7"
error = "#f7768e"
success = "#9ece6a"
prompt_user = "#7aa2f7"
prompt_path = "#bb9af7"
cursor = "#c0caf5"           # optional, for the app
selection = "#33467c"        # optional, for the app
ansi = []                    # optional, for the app: 16 colors
[prompt]
format = "{user} {path} {git}{node}{python}❯ "
```

Built-in themes: `neon` (the default), `tokyo-night`, `dracula`, `nord`, `solarized-dark`, `solarized-light`, and `mono`.

### Theme editor

`make theme` opens a theme editor in the browser. It shows a prompt, highlighted commands, the completion menu, the status bar, `ls`, `git`, and `cargo` output, and the 16 terminal colors in the colors of the theme. Click any text to change the color it uses, or edit the colors in the side panel; colors that are hard to read on the background get a warning. Start from a built-in theme or one of yours, or import a `.toml` file. **Download .toml** saves the theme; put it in `~/Library/Application Support/xsh/themes/` and run `xsh theme <name>`.

`{status}`, `{user}`, `{host}`, `{path}`, and any bundle segment name are available. Run `theme mine` to switch (the choice is saved).

Accessibility: `NO_COLOR` or `--no-color` removes colors; `--theme mono` is the built-in plain theme; `--screen-reader` uses ASCII borders and symbols and never animates.

## Multiplexer notes

Sessions live in a background server (`xsh server`, started on demand) that owns the PTYs, so they survive when you detach or close the terminal. The socket is in a directory only you can read. One client is attached at a time; a new attach detaches the old one.

External commands in the plain shell inherit your terminal directly, so they see a real TTY. Inside the multiplexer each pane has its own PTY.

## Build and test

```sh
make run              # build target/xsh.app (debug) and open it
make build            # build target/xsh.app (release)
make theme            # open the theme editor
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

See [CONTRIBUTING.md](CONTRIBUTING.md) and [docs/manual-tests.md](docs/manual-tests.md).

The app uses a copy of [SwiftTerm](https://github.com/migueldeicaza/SwiftTerm) in `app/Vendor/SwiftTerm` with a small fix; [`PATCHES.md`](app/Vendor/SwiftTerm/PATCHES.md) lists the changes and how to upgrade.

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
