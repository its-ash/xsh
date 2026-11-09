# Security

## Report a problem

Please do not open a public issue for a security problem. Use GitHub's private
reporting: **Security → Report a vulnerability** on the repository page. Give
the version, your system, and the steps to reproduce. We aim to answer within
7 days.

## What xsh protects, and how

| Area | Design |
| --- | --- |
| Bundles | Declarative only. No bundle runs arbitrary shell code. `init.rushy` is parsed for literal `alias` and `export` lines and is never executed. |
| Bundle hooks | Each `[[hooks]]` command passes the rule-based safety guard first. `bundles.allow_hooks = false` turns them off. |
| Downloads | HTTPS only, on an allowed host, at most 1 MiB per file, and the SHA-256 must match the registry index. A remote registry cannot point to local files. Names are limited to `a-z 0-9 - _`, so a bundle cannot write outside its directory. |
| Project config | `.xsh.toml` can set only `[shell]`, `[ui]`, and `[mux]`. A cloned repository cannot change the registry, hooks, or history settings. |
| History | Commands and directories are encrypted (XChaCha20-Poly1305) before they reach SQLite. The key comes from the OS keychain, or from Argon2id over `XSH_HISTORY_PASSPHRASE`. A wrong key is refused. |
| Man pages | For option suggestions xsh runs `man <name>` for the commands you ran. The name must use only letters, digits, `.`, `_`, `+`, and `-`, and must not start with `-`, so it cannot become a `man` option or a path. `man` runs with a 2-second limit, no pager, and its output is only read as text. |
| AI transcripts | Stored only on your machine, encrypted with the same kind of key. If they cannot be encrypted they are not saved. |
| API keys | Read from the keychain or an environment variable. Types that hold keys print `<redacted>` in `Debug`. Keys are not written to logs, errors, or transcripts. |
| AI requests | HTTPS only (plain HTTP is allowed for `localhost`, for Ollama). TLS uses rustls. |
| Multiplexer | The socket is in a directory with mode `0700`. |
| App updates | HTTPS only, from a fixed GitHub repository. The address comes from the version number. The download must match its `.sha256` file and hold an app with a valid signature and the right version. xsh asks before it installs. |

## What the safety guard is not

The guard checks AI-made commands and bundle hooks before they run. It parses
the command with the shell's own parser, looks through `sudo`, `env`, `sh -c`,
and `eval`, and blocks known-destructive patterns (`rm -rf /`, `dd of=/dev/…`,
fork bombs, `curl … | sh` without `--dry-run`, writes to system files, disk
tools, power commands). For AI-made commands a second model review runs after
it; bundle hooks get the rule-based checks only. A command the guard cannot
parse is blocked.

It is a **safety net, not a sandbox**. It cannot know everything a program
does. A model can also be wrong or tricked by text it reads. That is why agent
mode asks before every command by default. Keep `approve_each_step = true`
unless you work in a throw-away environment.

## Known limits

- With `shell.capture_stderr = true`, error text of foreground commands is kept in memory (last 16 KiB) and may be sent to your AI provider when you run `ai fix`. It can contain secrets that a program printed. Turn the option off if that is a risk.
- Context sent to a provider can include a directory listing and recent commands. Use the `[context]` switches in `ai.toml` to limit it. Use a local model (Ollama) to keep everything on your machine.
- The history passphrase mode needs the passphrase in an environment variable, which other processes of your user may read. Prefer the keychain.
- Hooks and AI commands run with your user's rights.
- App updates have an ad hoc signature, not a Developer ID, and the checksum is in the same release. So these checks find a damaged download, but they do not stop an attacker who controls the GitHub repository.

## Supported versions

Only the latest release gets security fixes while the project is pre-1.0.
