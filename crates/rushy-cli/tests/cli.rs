use std::error::Error;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use sha2::{Digest as _, Sha256};

type TestResult = Result<(), Box<dyn Error>>;

const BUNDLES: &[&str] = &[
    "git", "rsync", "adb", "npm", "pip", "postgres", "sbt", "scala", "ubuntu", "node", "python",
    "history", "vscode", "docker",
];

fn plugins() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

/// An isolated config and data directory.
struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> std::io::Result<Self> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("config"))?;
        Ok(Self { dir })
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_xsh"));
        c.env("XSH_CONFIG_DIR", self.path("config"))
            .env("XSH_DATA_DIR", self.path("data"))
            .env("XSH_HISTORY__KEY_SOURCE", "passphrase")
            .env("XSH_HISTORY_PASSPHRASE", "correct horse")
            .env_remove("NO_COLOR")
            .current_dir(self.dir.path());
        c
    }

    fn run(&self, args: &[&str]) -> std::io::Result<Output> {
        self.command().args(args).output()
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

// ----------------------------------------------------------------- core

#[test]
fn echo_pipe_wc_counts_six_bytes() -> TestResult {
    let e = Env::new()?;
    let o = e.run(&["-c", "echo hello | wc -c"])?;
    assert!(o.status.success());
    assert_eq!(out(&o).trim(), "6");
    Ok(())
}

#[test]
fn exit_status_propagates() -> TestResult {
    let e = Env::new()?;
    assert_eq!(e.run(&["-c", "exit 3"])?.status.code(), Some(3));
    assert_eq!(e.run(&["-c", "false"])?.status.code(), Some(1));
    let missing = e.run(&["-c", "rushy-definitely-missing-command"])?;
    assert_eq!(missing.status.code(), Some(127));
    assert!(err(&missing).contains("command not found"));
    Ok(())
}

#[test]
fn syntax_errors_exit_with_two() -> TestResult {
    let e = Env::new()?;
    let o = e.run(&["-c", "echo |"])?;
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("syntax error"));
    Ok(())
}

#[test]
fn script_mode_joins_continuation_lines() -> TestResult {
    let e = Env::new()?;
    let mut child = e
        .command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(b"echo hello \\\n  world\necho 'multi\nline'\nexit 4\n")?;
    let o = child.wait_with_output()?;
    assert_eq!(o.status.code(), Some(4));
    assert_eq!(out(&o), "hello world\nmulti\nline\n");
    Ok(())
}

#[test]
fn broken_config_reports_file_line_and_column() -> TestResult {
    let e = Env::new()?;
    std::fs::write(e.path("config/config.toml"), "[ui]\nthme = \"x\"\n")?;
    let o = e.run(&["-c", "true"])?;
    assert!(o.status.success(), "the shell still starts with defaults");
    assert!(err(&o).contains("config.toml:2:1"), "{}", err(&o));
    Ok(())
}

#[test]
fn help_lists_the_extra_builtins() -> TestResult {
    let e = Env::new()?;
    let text = out(&e.run(&["-c", "help"])?);
    for name in [
        "bundle", "ai", "theme", "cd", "jobs", "fg", "bg", "kill", "unalias",
    ] {
        assert!(text.contains(name), "{name} missing from help:\n{text}");
    }
    Ok(())
}

#[test]
fn theme_lists_switches_and_persists() -> TestResult {
    let e = Env::new()?;
    let list = out(&e.run(&["theme"])?);
    assert!(
        list.contains("* tokyo-night") && list.contains("  mono"),
        "{list}"
    );
    assert!(e.run(&["theme", "mono"])?.status.success());
    assert!(std::fs::read_to_string(e.path("config/config.toml"))?.contains("theme = \"mono\""));
    assert!(out(&e.run(&["theme"])?).contains("* mono"));
    let bad = e.run(&["theme", "nope"])?;
    assert_eq!(bad.status.code(), Some(1));
    Ok(())
}

// -------------------------------------------------------------- bundles

fn fixture_registry(dir: &Path) -> Result<String, Box<dyn Error>> {
    let mut bundles = Vec::new();
    for name in BUNDLES {
        let (src, dest) = (plugins().join(name), dir.join(name));
        std::fs::create_dir_all(&dest)?;
        let manifest = std::fs::read(src.join("bundle.toml"))?;
        std::fs::write(dest.join("bundle.toml"), &manifest)?;
        let mut files = Vec::new();
        for extra in ["init.rushy", "README.md"] {
            if let Ok(bytes) = std::fs::read(src.join(extra)) {
                std::fs::write(dest.join(extra), &bytes)?;
                files.push(serde_json::json!({"path": extra, "sha256": hex::encode(Sha256::digest(&bytes))}));
            }
        }
        let desc = std::fs::read_to_string(src.join("bundle.toml"))?
            .lines()
            .find_map(|l| {
                l.strip_prefix("description = ")
                    .map(|d| d.trim_matches('"').to_owned())
            })
            .unwrap_or_default();
        bundles.push(serde_json::json!({
            "name": name, "version": "0.1.0", "description": desc,
            "repo": "https://github.com/its-ash/xsh",
            "sha256": hex::encode(Sha256::digest(&manifest)),
            "url": format!("{name}/bundle.toml"), "files": files,
        }));
    }
    let index = dir.join("registry.json");
    std::fs::write(
        &index,
        serde_json::to_vec_pretty(&serde_json::json!({"version": 1, "bundles": bundles}))?,
    )?;
    Ok(format!("file://{}", index.display()))
}

#[test]
fn bundle_lifecycle_and_alias_activation() -> TestResult {
    let e = Env::new()?;
    let url = fixture_registry(&e.path("registry"))?;
    let run = |args: &[&str]| -> std::io::Result<Output> {
        e.command()
            .env("XSH_BUNDLES__REGISTRY_URL", &url)
            .args(args)
            .output()
    };

    let found = run(&["bundle", "search", "docker"])?;
    assert!(
        found.status.success() && out(&found).contains("docker"),
        "{}",
        err(&found)
    );
    assert_eq!(
        run(&["bundle", "search", "no-such-thing"])?.status.code(),
        Some(1)
    );

    let none = run(&["bundle", "list"])?;
    assert!(out(&none).contains("no bundles installed"));
    let inst = run(&["bundle", "install", "git"])?;
    assert!(inst.status.success(), "{}", err(&inst));
    assert!(
        out(&inst).contains("installed git 0.1.0") && out(&inst).contains("hook"),
        "{}",
        out(&inst)
    );
    assert!(e.path("config/bundles/git/bundle.toml").is_file());
    assert!(e.path("config/bundles/git/init.rushy").is_file());

    let list = out(&run(&["bundle", "list"])?);
    assert!(list.contains("git") && list.contains("idle"), "{list}");
    let info = out(&run(&["bundle", "info", "git"])?);
    assert!(
        info.contains("active when: **/.git") && info.contains("hook on Activate"),
        "{info}"
    );
    let notyet = out(&run(&["bundle", "info", "docker"])?);
    assert!(notyet.contains("(not installed)"), "{notyet}");
    assert!(out(&run(&["bundle", "update", "--all"])?).contains("git: up to date"));
    assert_eq!(
        run(&["bundle", "update"])?.status.code(),
        Some(2),
        "needs a name or --all"
    );

    // Inside a Git work tree with `git` on PATH, the bundle's aliases exist.
    let repo = e.path("repo");
    std::fs::create_dir_all(repo.join(".git"))?;
    std::fs::create_dir_all(repo.join("sub"))?;
    let bin = e.path("bin");
    std::fs::create_dir_all(&bin)?;
    std::fs::write(bin.join("git"), "#!/bin/sh\necho fake-git \"$@\"\n")?;
    std::fs::set_permissions(bin.join("git"), std::fs::Permissions::from_mode(0o755))?;
    let path = format!("{}:{}", bin.display(), std::env::var("PATH")?);
    let aliases = |dir: &Path| -> std::io::Result<String> {
        let o = e
            .command()
            .env("PATH", &path)
            .current_dir(dir)
            .args(["-c", "alias"])
            .output()?;
        Ok(out(&o))
    };
    let inside = aliases(&repo.join("sub"))?;
    assert!(
        inside.contains("alias gs='git status --short --branch'"),
        "{inside}"
    );
    assert!(
        inside.contains("alias gco='git checkout'"),
        "init.rushy aliases load too: {inside}"
    );
    assert!(
        !aliases(e.dir.path())?.contains("alias gs="),
        "not active outside a repository"
    );
    let ran = e
        .command()
        .env("PATH", &path)
        .current_dir(&repo)
        .args(["-c", "gs"])
        .output()?;
    assert_eq!(out(&ran).trim(), "fake-git status --short --branch");

    // Built-ins of the binary mix with the shell on one line.
    let mixed = run(&["-c", "bundle list; echo after"])?;
    assert!(
        out(&mixed).contains("git") && out(&mixed).ends_with("after\n"),
        "{}",
        out(&mixed)
    );

    assert!(run(&["bundle", "remove", "git"])?.status.success());
    assert!(!e.path("config/bundles/git").exists());
    assert_eq!(run(&["bundle", "remove", "git"])?.status.code(), Some(1));
    Ok(())
}

#[test]
fn tampered_bundles_and_foreign_urls_are_refused() -> TestResult {
    let e = Env::new()?;
    let url = fixture_registry(&e.path("registry"))?;
    std::fs::write(
        e.path("registry/git/bundle.toml"),
        "[package]\nname = \"git\"\nversion = \"9.9.9\"\n",
    )?;
    let o = e
        .command()
        .env("XSH_BUNDLES__REGISTRY_URL", &url)
        .args(["bundle", "install", "git"])
        .output()?;
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("checksum mismatch"), "{}", err(&o));
    assert!(!e.path("config/bundles/git").exists());

    let o = e
        .command()
        .env(
            "XSH_BUNDLES__REGISTRY_URL",
            "http://raw.githubusercontent.com/x/registry.json",
        )
        .args(["bundle", "search", "git"])
        .output()?;
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("only https://"), "{}", err(&o));

    // A project file may not redirect the registry.
    std::fs::write(
        e.path(".xsh.toml"),
        "[bundles]\nregistry_url = \"https://evil.example\"\n",
    )?;
    let o = e.run(&["-c", "true"])?;
    assert!(err(&o).contains(".xsh.toml:1"), "{}", err(&o));
    Ok(())
}

// -------------------------------------------------------------- history

fn history_json() -> String {
    let entry = |id: u32, cmd: &str| {
        serde_json::json!({"id": id, "ts": 1_700_000_000_000_i64 + i64::from(id), "command": cmd,
            "cwd": "/srv/project-zeta", "exit_code": 0, "duration_ms": 5, "host": "h", "session_id": "s"})
    };
    serde_json::to_string(&[
        entry(1, "cargo test --secret-flag"),
        entry(2, "git status"),
        entry(3, "ls -la"),
    ])
    .unwrap_or_default()
}

#[test]
fn history_commands_and_encryption_at_rest() -> TestResult {
    let e = Env::new()?;
    std::fs::write(e.path("in.json"), history_json())?;
    let imp = e.run(&[
        "history",
        "import",
        e.path("in.json").to_str().ok_or("path")?,
    ])?;
    assert!(out(&imp).contains("imported 3 entries"), "{}", err(&imp));

    let all = out(&e.run(&["history"])?);
    assert!(
        all.contains("cargo test --secret-flag") && all.contains("ls -la"),
        "{all}"
    );
    let last = out(&e.run(&["history", "1"])?);
    assert!(
        last.contains("ls -la") && !last.contains("git status"),
        "{last}"
    );
    let found = out(&e.run(&["history", "search", "carg", "tst"])?);
    assert!(
        found.contains("cargo test") && !found.contains("ls -la"),
        "{found}"
    );
    let json = out(&e.run(&["history", "export", "--format", "json"])?);
    assert!(json.contains("\"command\": \"git status\""), "{json}");
    assert_eq!(
        e.run(&["history", "export", "--format", "csv"])?
            .status
            .code(),
        Some(1)
    );

    assert!(out(&e.run(&["history", "pause"])?).contains("paused"));
    assert!(out(&e.run(&["history", "status"])?).contains("paused"));
    assert!(out(&e.run(&["history", "resume"])?).contains("recording"));

    let db = std::fs::read(e.path("data/history.db"))?;
    let text = String::from_utf8_lossy(&db);
    assert!(
        !text.contains("secret-flag") && !text.contains("project-zeta"),
        "plaintext found on disk"
    );

    let wrong = e
        .command()
        .env("XSH_HISTORY_PASSPHRASE", "wrong")
        .arg("history")
        .output()?;
    assert_eq!(wrong.status.code(), Some(1));
    assert!(
        err(&wrong).to_lowercase().contains("wrong"),
        "{}",
        err(&wrong)
    );

    assert!(e.run(&["history", "-c"])?.status.success());
    assert!(out(&e.run(&["history"])?).trim().is_empty());
    Ok(())
}

// ------------------------------------------------------------------- ai

/// A tiny HTTP server that answers Ollama chat requests by prompt.
fn mock_ollama() -> Option<String> {
    let listener = TcpListener::bind("127.0.0.1:0").ok()?;
    let url = format!("http://{}/", listener.local_addr().ok()?);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut len = 0usize;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            if reader.read_exact(&mut body).is_err() {
                continue;
            }
            let body = String::from_utf8_lossy(&body).into_owned();
            let content = if body.contains("security reviewer") {
                r#"{"safe": true, "reason": "read only"}"#.to_owned()
            } else if body.contains("turn a request into ONE command") {
                if body.contains("wipe the disk") {
                    r#"{"command": "rm -rf /", "explanation": "erases everything"}"#.to_owned()
                } else {
                    r#"{"command": "echo suggested-ok", "explanation": "prints a marker"}"#
                        .to_owned()
                }
            } else if body.contains("Explain the given shell command") {
                "MOCK EXPLANATION of the command.".to_owned()
            } else {
                String::new()
            };
            let reply = serde_json::json!({"message": {"role": "assistant", "content": content}})
                .to_string();
            let _ = write!(
                &stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    Some(url)
}

#[test]
fn ai_commands_with_a_local_provider() -> TestResult {
    let Some(url) = mock_ollama() else {
        eprintln!("skipped: this environment does not allow local sockets");
        return Ok(());
    };
    let e = Env::new()?;
    std::fs::write(
        e.path("config/ai.toml"),
        format!(
            "[default]\nprovider = \"ollama\"\n\n[providers.ollama]\nbase_url = \"{url}\"\nmodel = \"test\"\n"
        ),
    )?;
    let status = out(&e.run(&["ai", "status"])?);
    assert!(
        status.contains("provider: ollama") && status.contains("model: test"),
        "{status}"
    );

    let ex = e.run(&["ai", "explain", "tar", "-xzf", "a.tgz"])?;
    assert!(
        out(&ex).contains("MOCK EXPLANATION"),
        "{} {}",
        out(&ex),
        err(&ex)
    );
    let danger = e.run(&["ai", "explain", "rm", "-rf", "/"])?;
    assert!(
        out(&danger).contains("blocked (rm-recursive-root)"),
        "{}",
        out(&danger)
    );

    // Transcripts exist and are encrypted.
    let files: Vec<PathBuf> = std::fs::read_dir(e.path("data/ai"))?
        .flatten()
        .map(|d| d.path())
        .filter(|p| p.extension().is_some_and(|x| x == "bin"))
        .collect();
    assert_eq!(files.len(), 2, "one transcript per explain");
    for f in files {
        assert!(!String::from_utf8_lossy(&std::fs::read(f)?).contains("MOCK EXPLANATION"));
    }

    // suggest: show, check, ask, run.
    let mut child = e
        .command()
        .args(["ai", "suggest", "print", "a", "marker"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().ok_or("stdin")?.write_all(b"r\n")?;
    let ran = child.wait_with_output()?;
    assert!(
        out(&ran).matches("suggested-ok").count() >= 2,
        "shown, then run: {}",
        out(&ran)
    );

    // The rule guard refuses a destructive suggestion before asking anything.
    let mut child = e
        .command()
        .args(["ai", "suggest", "wipe", "the", "disk"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child.stdin.take().ok_or("stdin")?.write_all(b"r\n")?;
    let refused = child.wait_with_output()?;
    assert!(
        out(&refused).contains("refused: blocked (rm-recursive-root)"),
        "{}",
        out(&refused)
    );

    Ok(())
}

#[test]
fn ai_without_configuration_fails_cleanly() -> TestResult {
    let e = Env::new()?;
    let fix = e.run(&["ai", "fix"])?;
    assert_eq!(fix.status.code(), Some(1));
    assert!(err(&fix).contains("no failed command"), "{}", err(&fix));

    let missing = e
        .command()
        .env_remove("OPENAI_API_KEY")
        .args(["ai", "explain", "ls"])
        .output()?;
    assert_eq!(missing.status.code(), Some(1));
    assert!(err(&missing).contains("no API key"), "{}", err(&missing));

    let key = "sk-test-secret-value";
    let o = e
        .command()
        .env("OPENAI_API_KEY", key)
        .args(["ai", "status"])
        .output()?;
    assert!(
        !out(&o).contains(key) && !err(&o).contains(key),
        "keys never appear in output"
    );
    Ok(())
}
