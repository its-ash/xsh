//! The right pane of an agent run in `xsh.app`.
//!
//! `ai agent` makes a private socket and writes an escape sequence with the
//! token of the app and the socket path. The app opens a pane to the right that
//! runs `xsh app agent-runner <socket>`. The left pane keeps the plan, the text
//! of the model, and the approvals. The right pane runs each approved command,
//! then becomes a normal shell when the agent stops.

use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use rushy_ai::agent::CommandOutput;
use rushy_core::Shell;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

use crate::app::App;

const TOKEN_VAR: &str = "XSH_APP_TOKEN";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

async fn send(writer: &mut OwnedWriteHalf, v: &Value) -> std::io::Result<()> {
    writer.write_all(format!("{v}\n").as_bytes()).await?;
    writer.flush().await
}

/// Runs `command` in `shell`. The output shows on the terminal and also comes back.
pub async fn run_logged(shell: &mut Shell, command: &str) -> CommandOutput {
    shell.state_mut().set_log(true);
    let ran = shell.run_line(command).await;
    let (stdout, mut stderr) = shell.state_mut().take_log();
    shell.state_mut().set_log(false);
    if let Err(e) = ran {
        let _ = writeln!(stderr, "xsh: {e}");
    }
    CommandOutput {
        status: shell.last_status(),
        stdout,
        stderr,
    }
}

/// The connection from the agent to its right pane.
pub struct Link {
    lines: Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
    interrupted: bool,
}

impl Link {
    /// Asks the app for a right pane. Returns `None` outside `xsh.app`, or when
    /// no pane connects in time. Then the commands run in this pane.
    pub async fn open(cwd: &Path) -> Option<Self> {
        let token = std::env::var(TOKEN_VAR).ok().filter(|t| !t.is_empty())?;
        if !std::io::stdout().is_terminal() {
            return None;
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let name = format!("xsh-agent-{}-{stamp}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .inspect_err(|e| eprintln!("xsh: no agent pane: {e}"))
            .ok()?;
        let link = Self::connect(&dir, &token, cwd).await;
        let _ = std::fs::remove_dir_all(&dir);
        if link.is_none() {
            eprintln!("xsh: the agent pane did not open; commands run in this pane");
        }
        link
    }

    async fn connect(dir: &Path, token: &str, cwd: &Path) -> Option<Self> {
        let path = dir.join("s");
        let listener = UnixListener::bind(&path).ok()?;
        crate::term::agent_split(token, &path);
        let (stream, _) = tokio::time::timeout(CONNECT_TIMEOUT, listener.accept())
            .await
            .ok()?
            .ok()?;
        let (read, mut writer) = stream.into_split();
        send(&mut writer, &json!({ "cwd": cwd.to_string_lossy() }))
            .await
            .ok()?;
        Some(Self {
            lines: BufReader::new(read).lines(),
            writer,
            interrupted: false,
        })
    }

    /// Whether the user pressed Ctrl-C in the right pane during a command.
    pub fn interrupted(&self) -> bool {
        self.interrupted
    }

    /// Runs `command` in the right pane. Returns `None` when the pane is gone.
    pub async fn run(&mut self, command: &str) -> Option<CommandOutput> {
        send(&mut self.writer, &json!({ "command": command }))
            .await
            .ok()?;
        let line = self.lines.next_line().await.ok()??;
        let v: Value = serde_json::from_str(&line).ok()?;
        self.interrupted |= v["interrupted"].as_bool().unwrap_or(false);
        let text = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
        Some(CommandOutput {
            status: v["status"]
                .as_i64()
                .and_then(|s| i32::try_from(s).ok())
                .unwrap_or(1),
            stdout: text("stdout"),
            stderr: text("stderr"),
        })
    }
}

async fn serve_link(shell: &mut Shell, stream: UnixStream) -> Result<()> {
    let (read, mut writer) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let fg = shell.state().foreground();
    loop {
        // Ctrl-C here, between commands, also stops the agent: it sees the
        // connection close at its next command.
        let next = tokio::select! {
            line = lines.next_line() => line.context("agent connection")?,
            Ok(()) = tokio::signal::ctrl_c() => None,
        };
        let Some(line) = next else {
            return Ok(());
        };
        let v: Value = serde_json::from_str(&line).context("agent message")?;
        if let Some(cwd) = v["cwd"].as_str().map(PathBuf::from) {
            if cwd.is_dir() {
                shell.state_mut().set_cwd(cwd);
            }
        } else if let Some(command) = v["command"].as_str() {
            println!("\x1b[2m$ {}\x1b[0m", command.replace('\x1b', ""));
            let work = run_logged(shell, command);
            tokio::pin!(work);
            let (mut interrupted, mut gone) = (false, false);
            let out = loop {
                tokio::select! {
                    out = &mut work => break out,
                    Ok(()) = tokio::signal::ctrl_c(), if !interrupted => interrupted = true,
                    line = lines.next_line(), if !gone => {
                        // The agent stopped, so its command stops too.
                        if matches!(line, Ok(None) | Err(_)) {
                            gone = true;
                            let _ = fg.signal("INT");
                        }
                    }
                }
            };
            if gone {
                return Ok(());
            }
            let reply = json!({
                "status": out.status,
                "stdout": out.stdout,
                "stderr": out.stderr,
                "interrupted": interrupted,
            });
            send(&mut writer, &reply)
                .await
                .context("agent connection")?;
        }
    }
}

/// `xsh app agent-runner`: runs the commands of the agent, then a normal shell.
pub async fn serve(app: &mut App, socket: &Path) -> Result<i32> {
    crate::repl::forward_signals(&app.shell.state().foreground());
    crate::term::set_title("xsh agent");
    match UnixStream::connect(socket).await {
        Ok(stream) => {
            if let Err(e) = serve_link(&mut app.shell, stream).await {
                eprintln!("xsh: {e:#}");
            }
            eprintln!("\nxsh: the agent stopped; this pane is a normal shell now");
        }
        Err(e) => eprintln!("xsh: no agent at {}: {e}", socket.display()),
    }
    Box::pin(crate::repl::run(app)).await
}
