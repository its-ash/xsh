//! `ai claude <goal>`: Claude Code as the agent, with xsh in charge of every
//! action.
//!
//! Claude Code starts with all of its built-in tools turned off (`--tools ""`,
//! `--restricted`), no MCP servers but ours (`--strict-mcp-config`), and the
//! `dontAsk` permission mode, which refuses any tool that is not allowed in
//! advance. The only allowed tools are those of `xsh mcp` (see [`crate::mcp`]).
//! That server passes each call over a private socket back to this session,
//! where it runs here: commands pass the safety guard (and the LLM review when
//! an xsh provider is set up), then the same approval prompt as `ai agent`,
//! then run in the user's shell or the app's agent pane. Files can only be
//! read or changed inside the working directory, and each change needs
//! approval too.

use std::fmt::Write as _;
use std::io::IsTerminal as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use rushy_ai::agent::{AgentHost as _, Approval, CommandOutput, StepEvent};
use rushy_ai::{Assistant, Settings, Verdict};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

use crate::agent_pane::Link;
use crate::app::App;
use crate::cmds::CliHost;
use crate::mcp;
use crate::pretty::{Pal, StepLog};

/// Built-in Claude Code tools, refused by name as well, in case a version
/// does not know `--tools ""`.
const BUILTIN_TOOLS: &str = "Bash,BashOutput,KillShell,Read,Write,Edit,MultiEdit,NotebookEdit,\
Glob,Grep,LS,WebFetch,WebSearch,Task,Agent,TodoWrite,SlashCommand,Skill";

/// Added to Claude Code's system prompt.
const PROMPT: &str = "You run inside xsh, the user's shell. Your built-in tools are \
turned off; the tools of the `xsh` MCP server are the only way to act. run_command runs a \
command in the user's shell after the xsh safety guard and the user approve it. Keep \
commands short and non-interactive. If a step is declined or refused, do not retry it in \
another form: explain what you needed and stop, or choose a different safe approach. \
End with a short summary of what you did and what is left.";

/// Bytes of command output sent back to Claude Code.
const OUTPUT_LIMIT: usize = 30_000;

type Call = (Value, oneshot::Sender<Value>);

/// Deletes the private socket directory at the end.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn private_dir() -> Result<TempDir> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("xsh-claude-{}-{stamp}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(TempDir(dir))
}

/// The `claude` program: `$XSH_CLAUDE`, then `PATH`, then the places the
/// installers use.
fn find_claude(app: &App) -> Result<PathBuf> {
    let state = app.shell.state();
    if let Some(p) = state.var("XSH_CLAUDE").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let path = state.var("PATH").unwrap_or_default();
    let home = state.home().unwrap_or_default();
    std::env::split_paths(path)
        .map(|d| d.join("claude"))
        .chain(
            [".local/bin/claude", ".claude/local/claude"]
                .iter()
                .map(|p| Path::new(&home).join(p)),
        )
        .chain(["/opt/homebrew/bin/claude", "/usr/local/bin/claude"].map(PathBuf::from))
        .find(|p| p.is_file())
        .context(
            "Claude Code is not installed: get it from https://claude.com/claude-code, \
             or set XSH_CLAUDE to the `claude` program",
        )
}

/// The `--mcp-config` value: one stdio server, this binary in `mcp` mode.
fn mcp_config(exe: &Path, socket: &Path) -> Value {
    json!({
        "mcpServers": {
            mcp::SERVER: {
                "type": "stdio",
                "command": exe,
                "args": ["mcp", "--socket", socket],
            }
        }
    })
}

/// Arguments for `claude`. The goal goes on standard input: the list flags
/// take several values and would swallow a goal after them.
fn claude_args(config: &Value, model: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "--print",
        "--output-format",
        "stream-json",
        "--verbose",
        "--restricted",
        "--tools",
        "",
        "--disallowedTools",
        BUILTIN_TOOLS,
        "--strict-mcp-config",
        "--mcp-config",
        &config.to_string(),
        "--allowedTools",
        &format!("mcp__{}", mcp::SERVER),
        "--permission-mode",
        "dontAsk",
        "--disable-slash-commands",
        "--append-system-prompt",
        PROMPT,
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    if let Some(m) = model {
        args.extend(["--model".to_owned(), m.to_owned()]);
    }
    args
}

/// Accepts the connection of `xsh mcp` and passes its calls to `tx`.
async fn accept(listener: UnixListener, tx: mpsc::Sender<Call>) {
    while let Ok((stream, _)) = listener.accept().await {
        let tx = tx.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(req) = serde_json::from_str::<Value>(&line) else {
                    break;
                };
                let (reply_tx, reply_rx) = oneshot::channel();
                if tx.send((req, reply_tx)).await.is_err() {
                    break;
                }
                let Ok(reply) = reply_rx.await else {
                    break;
                };
                if write
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
    }
}

fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn render(out: &CommandOutput) -> String {
    format!(
        "exit status: {}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        tail(&out.stdout, OUTPUT_LIMIT),
        tail(&out.stderr, OUTPUT_LIMIT / 3)
    )
}

/// What `shell_info` reports besides the live shell state.
struct Session {
    history: Vec<String>,
    bundles: Vec<String>,
}

/// One `ai claude` run: the host that prints and runs, and the rules.
struct Run<'a, 'h> {
    host: CliHost<'h>,
    root: PathBuf,
    step: usize,
    session: Session,
    review: Option<(&'a Assistant<'a>, &'a rushy_ai::Context)>,
    /// The user stopped the run at an approval.
    aborted: bool,
}

impl Run<'_, '_> {
    /// Shows Claude Code's events and answers its tool calls until it ends or
    /// the user stops it. Returns the final answer and whether the user stopped.
    async fn drive(
        &mut self,
        events: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        rx: &mut mpsc::Receiver<Call>,
        fg: &rushy_core::ForegroundHandle,
    ) -> (String, bool) {
        let tty = std::io::stdin().is_terminal();
        let mut answer = String::new();
        self.host.event(StepEvent::Thinking);
        loop {
            tokio::select! {
                line = events.next_line() => match line {
                    Ok(Some(l)) => {
                        if let Ok(ev) = serde_json::from_str::<Value>(&l) {
                            show_event(&mut self.host, &ev, &mut answer);
                        }
                    }
                    _ => return (answer, false),
                },
                Some((req, reply)) = rx.recv() => {
                    let tool = req["tool"].as_str().unwrap_or_default().to_owned();
                    let res = self.call(&tool, &req["args"]).await;
                    let _ = reply.send(res);
                    if self.aborted {
                        return (answer, true);
                    }
                    self.host.event(StepEvent::Thinking);
                }
                Ok(()) = tokio::signal::ctrl_c() => {
                    // A command in this pane ends first; the run stops after it.
                    if self.host.busy.load(Ordering::SeqCst) {
                        self.host.interrupted.store(true, Ordering::SeqCst);
                        if !tty {
                            let _ = fg.signal("INT");
                        }
                    } else {
                        return (answer, true);
                    }
                }
            }
        }
    }

    async fn verdict(&self, command: &str) -> Verdict {
        match self.review {
            Some((a, ctx)) => a.check(command, ctx).await,
            None => rushy_ai::check_command(command),
        }
    }

    /// Runs one tool call. Returns the MCP result.
    async fn call(&mut self, tool: &str, args: &Value) -> Value {
        if self.aborted {
            return mcp::tool_text("the user stopped the run", true);
        }
        let short = tool.strip_prefix("mcp__xsh__").unwrap_or(tool);
        let res = match short {
            "run_command" => self.run_command(args).await,
            "write_file" | "edit_file" => self.change_file(short, args).await,
            "read_file" | "list_dir" | "find_files" | "search_text" => {
                let what = args["path"]
                    .as_str()
                    .or_else(|| args["pattern"].as_str())
                    .or_else(|| args["query"].as_str())
                    .unwrap_or(".");
                self.host.clear_thinking();
                eprintln!("{}", self.host.pal.dim(&format!("  · {short} {what}")));
                match short {
                    "read_file" => mcp::read_file(&self.root, args),
                    "list_dir" => mcp::list_dir(&self.root, args),
                    "find_files" => mcp::find_files(&self.root, args),
                    _ => mcp::search_text(&self.root, args),
                }
            }
            "shell_info" => Ok(self.shell_info()),
            "check_command" => {
                let command = args["command"].as_str().unwrap_or_default();
                Ok(match rushy_ai::check_command(command) {
                    v if v.is_allowed() => "allowed by the rule guard".to_owned(),
                    v => format!("refused: {v}"),
                })
            }
            other => Err(anyhow::anyhow!("unknown tool `{other}`")),
        };
        match res {
            Ok(text) => mcp::tool_text(&text, false),
            Err(e) => mcp::tool_text(&format!("error: {e:#}"), true),
        }
    }

    async fn run_command(&mut self, args: &Value) -> Result<String> {
        let command = args["command"].as_str().unwrap_or_default().trim();
        let reason = args["reason"].as_str().unwrap_or_default();
        if command.is_empty() {
            bail!("`command` is empty");
        }
        self.step += 1;
        let step = self.step;
        let verdict = self.verdict(command).await;
        if !verdict.is_allowed() {
            let text = verdict.to_string();
            self.host.event(StepEvent::Blocked {
                step,
                command,
                reason: text.clone(),
            });
            return Ok(format!("not run: {text}"));
        }
        match self.host.approve(step, command, reason).await {
            Approval::Run => {
                let out = self.host.run(command).await;
                self.host.event(StepEvent::Ran {
                    step,
                    command,
                    output: &out,
                });
                if self.host.stopped() {
                    self.aborted = true;
                }
                Ok(render(&out))
            }
            Approval::Skip => Ok("not run: the user declined this command".to_owned()),
            Approval::Abort => {
                self.aborted = true;
                Ok("not run: the user stopped the run".to_owned())
            }
        }
    }

    async fn change_file(&mut self, tool: &str, args: &Value) -> Result<String> {
        let (full, what) = mcp::describe_write(&self.root, tool, args)?;
        let reason = args["reason"].as_str().unwrap_or_default();
        self.step += 1;
        match self.host.approve(self.step, &what, reason).await {
            Approval::Run => {
                let res = if tool == "write_file" {
                    mcp::write_file(&self.root, &full, args)
                } else {
                    mcp::edit_file(&self.root, &full, args)
                };
                let pal = &self.host.pal;
                let (status, result) = match &res {
                    Ok(_) => (0, "done".to_owned()),
                    Err(e) => (1, format!("failed: {e}")),
                };
                eprint!("{}", pal.card_note(status != 0, &result));
                self.host.steps.push(StepLog {
                    command: what,
                    result,
                    output: String::new(),
                });
                res
            }
            Approval::Skip => Ok("not changed: the user declined this change".to_owned()),
            Approval::Abort => {
                self.aborted = true;
                Ok("not changed: the user stopped the run".to_owned())
            }
        }
    }

    fn shell_info(&self) -> String {
        let shell = &*self.host.shell;
        let mut out = format!(
            "working directory: {}\nfile tools are limited to: {}\nlast exit status: {}\n",
            shell.state().cwd().display(),
            self.root.display(),
            shell.last_status()
        );
        if let Some(f) = shell.last_failure() {
            let _ = writeln!(
                out,
                "last failed command: {} (exit {}, in {})\nits error output:\n{}",
                f.command,
                f.exit_code,
                f.cwd.display(),
                tail(&f.stderr, 4_000)
            );
        }
        if !self.session.bundles.is_empty() {
            let _ = writeln!(out, "active bundles: {}", self.session.bundles.join(", "));
        }
        if !self.session.history.is_empty() {
            out.push_str("recent commands, oldest first:\n");
            for c in &self.session.history {
                let _ = writeln!(out, "  {c}");
            }
        }
        out
    }
}

/// Prints the text blocks of one `assistant` event; keeps the last one as
/// the answer so far.
fn show_event(host: &mut CliHost<'_>, ev: &Value, answer: &mut String) {
    match ev["type"].as_str() {
        Some("system") if ev["subtype"] == "init" => {
            let ok = ev["mcp_servers"].as_array().is_some_and(|list| {
                list.iter()
                    .any(|s| s["name"] == mcp::SERVER && s["status"] == "connected")
            });
            if !ok {
                host.clear_thinking();
                eprintln!("xsh: Claude Code could not reach the xsh tools; it cannot act");
            }
        }
        Some("assistant") => {
            let blocks = ev["message"]["content"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let uses_tools = blocks.iter().any(|b| b["type"] == "tool_use");
            for b in &blocks {
                if let Some(text) = b["text"].as_str().filter(|t| !t.trim().is_empty()) {
                    if uses_tools {
                        host.event(StepEvent::Said { text });
                    } else {
                        text.clone_into(answer);
                    }
                }
            }
            if uses_tools {
                host.event(StepEvent::Thinking);
            }
        }
        Some("result") => {
            if let Some(text) = ev["result"].as_str().filter(|t| !t.trim().is_empty()) {
                text.clone_into(answer);
            }
            if ev["is_error"].as_bool() == Some(true) && answer.is_empty() {
                *answer = format!(
                    "*Claude Code stopped: {}*",
                    ev["subtype"].as_str().unwrap_or("error")
                );
            }
        }
        _ => {}
    }
}

/// A running Claude Code and the socket its MCP server talks to.
struct Started {
    /// Removed when the run ends.
    _dir: TempDir,
    child: tokio::process::Child,
    events: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    rx: mpsc::Receiver<Call>,
    acceptor: tokio::task::JoinHandle<()>,
}

/// Opens the tool socket and starts Claude Code in `root` with `goal`.
async fn start(
    claude: &Path,
    root: &Path,
    path: Option<&str>,
    goal: &str,
    model: Option<&str>,
) -> Result<Started> {
    let dir = private_dir()?;
    let socket = dir.0.join("s");
    let listener = UnixListener::bind(&socket).context("cannot open the tool socket")?;
    let exe = std::env::current_exe().context("cannot find the xsh program")?;
    let config = mcp_config(&exe, &socket);
    let mut cmd = tokio::process::Command::new(claude);
    cmd.args(claude_args(&config, model))
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if let Some(p) = path {
        cmd.env("PATH", p);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("cannot start {}", claude.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(goal.as_bytes()).await?;
        drop(stdin);
    }
    let stdout = child.stdout.take().context("no output from Claude Code")?;
    let (tx, rx) = mpsc::channel::<Call>(1);
    Ok(Started {
        _dir: dir,
        child,
        events: BufReader::new(stdout).lines(),
        rx,
        acceptor: tokio::spawn(accept(listener, tx)),
    })
}

/// `ai claude [--auto] [--model M] <goal>`.
pub async fn run(app: &mut App, goal: &str, auto: bool, model: Option<&str>) -> Result<i32> {
    // `--model` names a Claude Code model here, not one for the xsh provider.
    app.set_model_override(None);
    let claude = find_claude(app)?;
    let root = app
        .shell
        .state()
        .cwd()
        .canonicalize()
        .context("the working directory is not available")?;
    let rt = app.ai().ok();
    let ctx = match &rt {
        Some(rt) => Some(app.ai_context(&rt.cfg).await),
        None => None,
    };
    let assistant = rt
        .as_ref()
        .map(|rt| Assistant::new(&*rt.provider, Settings::from(&rt.cfg)));
    let auto = auto || rt.as_ref().is_some_and(|rt| rt.cfg.default.autopilot);
    let ask_each = !auto
        && rt
            .as_ref()
            .is_none_or(|rt| rt.cfg.default.approve_each_step);
    let session = Session {
        history: app.recent_commands(15),
        bundles: app.effects.active.iter().cloned().collect(),
    };

    let path = app.shell.state().var("PATH").map(str::to_owned);
    let Started {
        _dir,
        mut child,
        mut events,
        mut rx,
        acceptor,
    } = start(&claude, &root, path.as_deref(), goal, model).await?;

    let pal = Pal::for_app(app);
    eprint!("{}", pal.question(goal));
    eprintln!(
        "{}",
        pal.dim(&format!(
            "Claude Code{} · only xsh tools · {} · file changes only inside {}",
            model.map_or_else(String::new, |m| format!(" ({m})")),
            if ask_each {
                "you approve each step"
            } else {
                "autopilot: no questions, the safety checks stay on"
            },
            root.display()
        ))
    );
    let pane = Link::open(&root).await;
    let busy = Arc::new(AtomicBool::new(false));
    let interrupted = Arc::new(AtomicBool::new(false));
    let fg = app.shell.state().foreground();
    let host = CliHost {
        shell: &mut app.shell,
        ask_each,
        link: pane,
        pane_closed: false,
        busy: Arc::clone(&busy),
        interrupted: Arc::clone(&interrupted),
        pal: pal.clone(),
        thinking: false,
        steps: Vec::new(),
    };
    let mut run = Run {
        host,
        root,
        step: 0,
        session,
        review: assistant.as_ref().zip(ctx.as_ref()),
        aborted: false,
    };
    let (answer, stopped) = run.drive(&mut events, &mut rx, &fg).await;
    run.host.clear_thinking();
    if stopped {
        let _ = child.start_kill();
    }
    let status = child.wait().await.ok().and_then(|s| s.code());
    acceptor.abort();
    let steps = std::mem::take(&mut run.host.steps);
    drop(run);
    let answer = if stopped {
        format!("*Stopped by you after {} step(s).*", steps.len())
    } else if answer.trim().is_empty() {
        match status {
            Some(0) => "*Claude Code gave no summary.*".to_owned(),
            Some(c) => format!("*Claude Code ended with status {c}; see the messages above.*"),
            None => "*Claude Code was stopped.*".to_owned(),
        }
    } else {
        answer
    };
    print!("\n{}", pal.agent_report(goal, &answer, &steps));
    Ok(if stopped { 130 } else { status.unwrap_or(1) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_gets_no_tools_of_its_own() {
        let config = mcp_config(Path::new("/x/xsh"), Path::new("/tmp/s"));
        assert_eq!(config["mcpServers"]["xsh"]["args"][0], "mcp");
        let args = claude_args(&config, Some("sonnet"));
        let pair = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .and_then(|i| args.get(i + 1))
                .cloned()
        };
        assert_eq!(pair("--tools").as_deref(), Some(""));
        assert_eq!(pair("--permission-mode").as_deref(), Some("dontAsk"));
        assert_eq!(pair("--allowedTools").as_deref(), Some("mcp__xsh"));
        assert_eq!(pair("--model").as_deref(), Some("sonnet"));
        assert!(pair("--disallowedTools").is_some_and(|t| t.contains("Bash")));
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
        assert!(args.iter().any(|a| a == "--restricted"));
    }

    #[test]
    fn output_tail_keeps_the_end() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 3), "ab");
        assert_eq!(tail("aé", 1), "");
    }
}
