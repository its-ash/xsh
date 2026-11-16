//! Implementations of `bundle`, `history`, `theme`, and `ai`.

use std::io::{IsTerminal as _, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use rushy_ai::agent::{self, AgentHost, Approval, CommandOutput, StepEvent, Stop};
use rushy_ai::{Assistant, ChatMessage, Failure, Settings, Verdict};
use rushy_bundles::{Effects, Installed, Manifest};
use rushy_core::Shell;

use crate::agent_pane::{self, Link};
use crate::app::{App, user_themes};
use crate::cli::{AiCmd, BundleCmd, ConfigCmd, HistoryArgs, HistoryCmd};
use crate::pretty::{Pal, StepLog};

// ---------------------------------------------------------------- config

/// `config list|get|set|unset|edit|path|check`.
pub fn config(app: &mut App, cmd: ConfigCmd) -> Result<i32> {
    match cmd {
        ConfigCmd::List => {
            for key in rushy_config::keys()? {
                println!("{key} = {}", rushy_config::get_value(&app.cfg, &key)?);
            }
        }
        ConfigCmd::Get { key } => println!("{}", rushy_config::get_value(&app.cfg, &key)?),
        ConfigCmd::Set { key, value } => {
            rushy_config::set_value(&app.paths, &key, &value)?;
            app.reload();
            println!("{key} = {}", rushy_config::get_value(&app.cfg, &key)?);
        }
        ConfigCmd::Unset { key } => {
            rushy_config::unset_value(&app.paths, &key)?;
            app.reload();
            println!("{key} = {}", rushy_config::get_value(&app.cfg, &key)?);
        }
        ConfigCmd::Edit => {
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".to_owned());
            let file = app.paths.config_file();
            std::fs::create_dir_all(app.paths.config_dir())?;
            // The editor value may hold flags, as in `code -w`.
            let mut parts = editor.split_whitespace();
            let prog = parts.next().context("$EDITOR is empty")?;
            let status = std::process::Command::new(prog)
                .args(parts)
                .arg(&file)
                .status()
                .with_context(|| format!("cannot start `{prog}`"))?;
            app.reload();
            return Ok(status.code().unwrap_or(1));
        }
        ConfigCmd::Path => {
            println!("config:  {}", app.paths.config_file().display());
            println!("startup: {}", app.rc_path().display());
            println!("data:    {}", app.paths.data_dir().display());
        }
        ConfigCmd::Check => {
            let cwd = app.shell.state().cwd().to_path_buf();
            match rushy_config::Loader::new(app.paths.clone())
                .project(cwd)
                .load()
            {
                Ok(_) => println!("config is valid"),
                Err(e) => {
                    eprintln!("xsh: {e}");
                    return Ok(1);
                }
            }
        }
    }
    Ok(0)
}

// ---------------------------------------------------------------- bundles

fn describe(m: &Manifest) {
    let p = &m.package;
    println!("{} {}", p.name, p.version);
    if !p.description.is_empty() {
        println!("  {}", p.description);
    }
    for (label, value) in [("author", &p.author), ("license", &p.license)] {
        if !value.is_empty() {
            println!("  {label}: {value}");
        }
    }
    if let Some(v) = &p.min_rushy_version {
        println!("  needs xsh: {v} or later");
    }
    let r = &m.bundle;
    if !r.detect.is_empty() {
        println!("  active when: {}", r.detect.join(", "));
    }
    if !r.requires_commands.is_empty() {
        println!("  requires commands: {}", r.requires_commands.join(", "));
    }
    println!(
        "  provides: {} aliases, {} variables{}{}",
        m.aliases.len(),
        m.env.len(),
        m.prompt
            .as_ref()
            .map_or_else(String::new, |s| format!(", prompt segment `{}`", s.segment)),
        if m.completion.commands.is_empty() {
            String::new()
        } else {
            format!(", completions for {}", m.completion.commands.join(", "))
        }
    );
    for h in &m.hooks {
        println!("  hook on {:?}: {}", h.on, h.run);
    }
}

fn is_path(name: &str) -> bool {
    name.starts_with('.') || name.contains(std::path::MAIN_SEPARATOR) || name.contains('/')
}

fn list_row(b: &Installed, active: bool) {
    println!(
        "{:<12} {:<8} {:<7} {}",
        b.name(),
        b.manifest.package.version,
        if active { "active" } else { "idle" },
        b.manifest.package.description
    );
}

/// `bundle ...`
pub async fn bundle(app: &mut App, cmd: BundleCmd) -> Result<i32> {
    match cmd {
        BundleCmd::Install { name } => {
            let inst = if is_path(&name) {
                app.store.install_dir(Path::new(&name))?
            } else {
                let dl = app.registry()?.fetch(&name).await?;
                app.store.install(&dl)?
            };
            println!(
                "installed {} {}",
                inst.name(),
                inst.manifest.package.version
            );
            if !inst.manifest.hooks.is_empty() {
                println!(
                    "note: it defines {} hook command(s); each one is checked by the safety guard before it runs",
                    inst.manifest.hooks.len()
                );
            }
            app.reload_bundles();
            Ok(0)
        }
        BundleCmd::Remove { name } => {
            app.store.remove(&name)?;
            println!("removed {name}");
            app.reload_bundles();
            Ok(0)
        }
        BundleCmd::List => {
            let (list, broken) = app.store.list();
            for e in &broken {
                eprintln!("xsh: broken bundle: {e}");
            }
            if list.is_empty() {
                println!("no bundles installed; try `bundle search` or `bundle install <name>`");
                return Ok(0);
            }
            let path = app.shell.state().var("PATH").unwrap_or_default().to_owned();
            let fx = Effects::compute(
                &list,
                app.shell.state().cwd(),
                std::ffi::OsStr::new(&path),
                &app.cfg.bundles.disabled,
            );
            for b in &list {
                list_row(b, fx.active.contains(b.name()));
            }
            Ok(i32::from(!broken.is_empty()))
        }
        BundleCmd::Search { query } => {
            let (hits, src) = app.registry()?.search(&query).await?;
            if matches!(src, rushy_bundles::IndexSource::Cache) {
                eprintln!("xsh: offline; showing the cached registry");
            }
            if hits.is_empty() {
                println!("no bundles match `{query}`");
                return Ok(1);
            }
            for h in hits {
                let mark = if app.store.get(&h.name).is_ok() {
                    "*"
                } else {
                    " "
                };
                println!("{mark} {:<12} {:<8} {}", h.name, h.version, h.description);
            }
            Ok(0)
        }
        BundleCmd::Update(args) => update(app, args.name, args.all).await,
        BundleCmd::Info { name } => {
            if let Ok(b) = app.store.get(&name) {
                describe(&b.manifest);
                println!("  installed at {}", b.dir.display());
                return Ok(0);
            }
            let (index, _) = app.registry()?.index().await?;
            let Some(e) = index.bundles.iter().find(|b| b.name == name) else {
                bail!("no bundle named `{name}` is installed or in the registry");
            };
            println!("{} {} (not installed)", e.name, e.version);
            println!("  {}", e.description);
            println!("  source: {}", e.repo);
            println!("  install with: bundle install {name}");
            Ok(0)
        }
    }
}

async fn update(app: &mut App, name: Option<String>, all: bool) -> Result<i32> {
    let client = app.registry()?;
    let (index, src) = client.index().await?;
    if matches!(src, rushy_bundles::IndexSource::Cache) {
        eprintln!("xsh: offline; comparing against the cached registry");
    }
    let targets: Vec<String> = match name {
        Some(n) if !all => vec![n],
        _ => app
            .store
            .list()
            .0
            .iter()
            .map(|b| b.name().to_owned())
            .collect(),
    };
    let mut failed = false;
    for t in targets {
        let step = async {
            let cur = app.store.get(&t)?;
            let old = semver::Version::parse(&cur.manifest.package.version)?;
            let Some(entry) = index.bundles.iter().find(|b| b.name == t) else {
                println!("{t}: not in the registry (local bundle)");
                return Ok::<(), anyhow::Error>(());
            };
            let new = semver::Version::parse(&entry.version)?;
            if new > old {
                app.store.install(&client.download(entry).await?)?;
                println!("{t}: {old} -> {new}");
            } else {
                println!("{t}: up to date ({old})");
            }
            Ok(())
        };
        if let Err(e) = step.await {
            eprintln!("xsh: {t}: {e:#}");
            failed = true;
        }
    }
    app.reload_bundles();
    Ok(i32::from(failed))
}

// ---------------------------------------------------------------- history

fn when(ts: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts).map_or_else(String::new, |d| {
        d.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    })
}

/// `history ...`
pub fn history(app: &mut App, args: HistoryArgs) -> Result<i32> {
    let cmd = match (args.cmd, args.clear) {
        (Some(c), _) => c,
        (None, true) => HistoryCmd::Clear,
        (None, false) => {
            let h = app.history_store()?;
            let entries = match args.count {
                Some(n) => h.recent(n)?,
                None => h.all()?,
            };
            for e in entries {
                println!("{:>6}  {}  {}", e.id, when(e.ts), e.command);
            }
            return Ok(0);
        }
    };
    let h = app.history_store()?;
    match cmd {
        HistoryCmd::Search { query, limit } => {
            let found = h.search(&query.join(" "), limit)?;
            for e in &found {
                println!("{:>6}  {}  {}", e.id, when(e.ts), e.command);
            }
            Ok(i32::from(found.is_empty()))
        }
        HistoryCmd::Export { format, output } => {
            if format != "json" {
                bail!("unsupported format `{format}`; only json is available");
            }
            if let Some(path) = output {
                let file = std::fs::File::create(&path)
                    .with_context(|| format!("cannot write {}", path.display()))?;
                h.export_json(std::io::BufWriter::new(file))?;
            } else {
                h.export_json(std::io::stdout().lock())?;
                println!();
            }
            Ok(0)
        }
        HistoryCmd::Import { file } => {
            let n = if file == Path::new("-") {
                h.import_json(std::io::stdin().lock())?
            } else {
                let f = std::fs::File::open(&file)
                    .with_context(|| format!("cannot read {}", file.display()))?;
                h.import_json(std::io::BufReader::new(f))?
            };
            println!("imported {n} entries");
            Ok(0)
        }
        HistoryCmd::Pause => {
            h.pause()?;
            println!("history is paused; nothing is recorded until `history resume`");
            Ok(0)
        }
        HistoryCmd::Resume => {
            h.resume()?;
            println!("history is recording");
            Ok(0)
        }
        HistoryCmd::Clear => {
            h.clear()?;
            app.shell.state_mut().clear_history();
            Ok(0)
        }
        HistoryCmd::Status => {
            println!(
                "history is {}",
                if h.is_paused()? {
                    "paused"
                } else {
                    "recording"
                }
            );
            Ok(0)
        }
    }
}

// ------------------------------------------------------------------ theme

/// `theme [name]`
pub fn theme(app: &mut App, name: Option<String>) -> Result<i32> {
    match name {
        None => {
            let mut names: Vec<String> =
                rushy_tui::BUILTIN.iter().map(|s| (*s).to_owned()).collect();
            names.extend(user_themes(&app.paths.themes_dir()));
            names.sort();
            names.dedup();
            for n in names {
                let mark = if n == app.cfg.ui.theme { "*" } else { " " };
                println!("{mark} {n}");
            }
            Ok(0)
        }
        Some(n) => {
            let t = rushy_tui::Theme::load(&app.paths.themes_dir(), &n)?;
            rushy_config::set_theme(&app.paths, &n)?;
            app.theme = t;
            app.cfg.ui.theme.clone_from(&n);
            println!("theme: {n}");
            Ok(0)
        }
    }
}

// --------------------------------------------------------------------- ai

/// What the user chose for a suggested command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Put it in the prompt for editing.
    Insert(String),
    /// Run it now.
    Run(String),
    /// Do nothing.
    Cancel,
}

/// The answer of [`ask`] when the user presses Ctrl-C.
const INTERRUPTED: &str = "\x03";

fn read_cooked() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s.trim().to_lowercase()
}

/// Reads one line in raw mode, so Ctrl-C can end a read that a plain blocking
/// read would keep. That read would also take the keys of the next prompt.
fn read_raw(stop: &AtomicBool) -> String {
    if enable_raw_mode().is_err() {
        return read_cooked();
    }
    let mut s = String::new();
    while !stop.load(Ordering::SeqCst) {
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(_) => break,
        }
        let Ok(Event::Key(k)) = event::read() else {
            continue;
        };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        match k.code {
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                INTERRUPTED.clone_into(&mut s);
                break;
            }
            KeyCode::Enter => break,
            KeyCode::Backspace => {
                if s.pop().is_some() {
                    eprint!("\x08 \x08");
                }
            }
            KeyCode::Char(c) => {
                s.push(c);
                eprint!("{c}");
            }
            _ => {}
        }
        let _ = std::io::stderr().flush();
    }
    let _ = disable_raw_mode();
    eprintln!();
    s.trim().to_lowercase()
}

/// Ends the raw read when the caller stops waiting for it.
struct StopRead(Arc<AtomicBool>);

impl Drop for StopRead {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn ask(prompt: &str) -> String {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let stop = Arc::new(AtomicBool::new(false));
    let _guard = StopRead(Arc::clone(&stop));
    let tty = std::io::stdin().is_terminal();
    tokio::task::spawn_blocking(move || if tty { read_raw(&stop) } else { read_cooked() })
        .await
        .unwrap_or_default()
}

/// Runs `work`, and gives up on it when the user presses Ctrl-C. Use it only
/// for work that is safe to drop, such as a request to the model.
async fn until_interrupt<T>(work: impl Future<Output = T>) -> Result<T> {
    tokio::select! {
        v = work => Ok(v),
        Ok(()) = tokio::signal::ctrl_c() => bail!("stopped"),
    }
}

async fn git_status(cwd: &Path) -> Option<String> {
    let out = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::process::Command::new("git")
            .args(["status", "--short"])
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn confirm(
    assistant: &Assistant<'_>,
    ctx: &rushy_ai::Context,
    command: &str,
    allow_insert: bool,
) -> Choice {
    let Ok(verdict) = until_interrupt(assistant.check(command, ctx)).await else {
        eprintln!("stopped");
        return Choice::Cancel;
    };
    if let Verdict::Block { .. } = &verdict {
        println!("refused: {verdict}");
        return Choice::Cancel;
    }
    let prompt = if allow_insert {
        "[i]nsert in prompt (default), [r]un now, [c]ancel? "
    } else {
        "[r]un now, [c]ancel (default)? "
    };
    match ask(prompt).await.as_str() {
        "r" | "run" | "y" | "yes" => Choice::Run(command.to_owned()),
        "" | "i" | "insert" if allow_insert => Choice::Insert(command.to_owned()),
        _ => Choice::Cancel,
    }
}

/// Turns `text` into a command, shows it, runs the safety checks, and asks
/// what to do. Used by `Ctrl-Space` and `ai suggest`.
pub async fn suggest(app: &mut App, text: &str, allow_insert: bool) -> Result<Choice> {
    let rt = app.ai()?;
    let ctx = app.ai_context(&rt.cfg).await;
    let assistant = Assistant::new(&*rt.provider, Settings::from(&rt.cfg));
    let s = until_interrupt(assistant.suggest(text, &ctx)).await??;
    app.save_transcript(
        "suggest",
        rt.provider.name(),
        vec![
            ChatMessage::user(text),
            ChatMessage::assistant(format!("{} -- {}", s.command, s.explanation), Vec::new()),
        ],
    );
    let pal = Pal::for_app(app);
    if s.command.trim().is_empty() {
        print!("{}", pal.answer(&format!("no command: {}", s.explanation)));
        return Ok(Choice::Cancel);
    }
    print!("{}", pal.suggestion(&s.command, &s.explanation));
    Ok(confirm(&assistant, &ctx, &s.command, allow_insert).await)
}

/// The terminal side of an agent run: step cards, approvals, and the shell
/// (or the app's right pane) that runs each command. `ai agent` and
/// `ai claude` share it.
pub(crate) struct CliHost<'a> {
    pub(crate) shell: &'a mut Shell,
    pub(crate) ask_each: bool,
    /// The right pane in `xsh.app`. Without it, commands run in this pane.
    pub(crate) link: Option<Link>,
    /// The right pane closed during the run.
    pub(crate) pane_closed: bool,
    /// A command runs in this pane, so Ctrl-C reaches it first.
    pub(crate) busy: Arc<AtomicBool>,
    /// The user pressed Ctrl-C during a command in this pane.
    pub(crate) interrupted: Arc<AtomicBool>,
    /// Colors, glyphs, and width for the step cards.
    pub(crate) pal: Pal,
    /// A "thinking" line is on the screen without a newline.
    pub(crate) thinking: bool,
    /// Every step so far, for the report at the end.
    pub(crate) steps: Vec<StepLog>,
}

impl CliHost<'_> {
    /// Erases the "thinking" line before anything else is printed.
    pub(crate) fn clear_thinking(&mut self) {
        if std::mem::take(&mut self.thinking) {
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        }
    }
}

/// Lines of output shown in a step card when the command ran in the agent pane.
const PREVIEW_LINES: usize = 6;

#[async_trait]
impl AgentHost for CliHost<'_> {
    async fn approve(&mut self, step: usize, command: &str, reason: &str) -> Approval {
        self.clear_thinking();
        eprint!("{}", self.pal.card_head(step, command, reason));
        if !self.ask_each {
            return Approval::Run;
        }
        let answer = match ask("[y]es run, [n]o skip, [a]bort (default: no)? ")
            .await
            .as_str()
        {
            "y" | "yes" => Approval::Run,
            "a" | "abort" | "q" | INTERRUPTED => Approval::Abort,
            _ => Approval::Skip,
        };
        let note = match answer {
            Approval::Run => return answer,
            Approval::Skip => "skipped",
            Approval::Abort => "stopped",
        };
        eprint!("{}", self.pal.card_note(answer == Approval::Abort, note));
        self.steps.push(StepLog {
            command: command.to_owned(),
            result: note.to_owned(),
            output: String::new(),
        });
        answer
    }

    async fn run(&mut self, command: &str) -> CommandOutput {
        // The output follows this line: live here, or as a preview from the
        // agent pane when the command ends.
        eprint!("{}", self.pal.card_response());
        let Some(link) = &mut self.link else {
            self.busy.store(true, Ordering::SeqCst);
            let out = agent_pane::run_logged(self.shell, command).await;
            self.busy.store(false, Ordering::SeqCst);
            return out;
        };
        if let Some(out) = link.run(command).await {
            return out;
        }
        self.link = None;
        self.pane_closed = true;
        eprintln!("xsh: the agent pane stopped; the agent stops");
        CommandOutput {
            status: 1,
            stdout: String::new(),
            stderr: "the agent pane was closed; the result is not known".to_owned(),
        }
    }

    fn event(&mut self, event: StepEvent<'_>) {
        self.clear_thinking();
        match event {
            StepEvent::Thinking => {
                let dots = self.pal.dim("⋯ thinking");
                // On a terminal the line is erased when the next output comes.
                if std::io::stderr().is_terminal() {
                    eprint!("\n{dots}");
                    let _ = std::io::stderr().flush();
                    self.thinking = true;
                } else {
                    eprintln!("\n{dots}");
                }
            }
            StepEvent::Said { text } => eprint!("\n{}", self.pal.said(text)),
            StepEvent::Blocked {
                step,
                command,
                reason,
            } => {
                eprint!("{}", self.pal.card_head(step, command, ""));
                eprint!(
                    "{}",
                    self.pal.card_note(true, &format!("refused: {reason}"))
                );
                self.steps.push(StepLog {
                    command: command.to_owned(),
                    result: "refused".to_owned(),
                    output: reason,
                });
            }
            StepEvent::Ran {
                command, output, ..
            } => {
                self.steps.push(StepLog {
                    command: command.to_owned(),
                    result: if output.status == 0 {
                        "ok (exit 0)".to_owned()
                    } else {
                        format!("failed (exit {})", output.status)
                    },
                    output: StepLog::last_line(&output.stdout, &output.stderr),
                });
                // In the app the command ran in the right pane, so show the
                // end of its output here too. Otherwise it is already above.
                if output.stdout.trim().is_empty() && output.stderr.trim().is_empty() {
                    eprint!("{}", self.pal.card_no_output());
                } else if self.link.is_some() {
                    let text = format!("{}\n{}", output.stdout, output.stderr);
                    eprint!("{}", self.pal.card_preview(&text, PREVIEW_LINES));
                }
                eprint!("{}", self.pal.card_status(output.status));
            }
        }
    }

    fn stopped(&self) -> bool {
        self.pane_closed
            || self.interrupted.load(Ordering::SeqCst)
            || self.link.as_ref().is_some_and(Link::interrupted)
    }
}

fn ai_status(app: &App) -> Result<i32> {
    let cfg = rushy_ai::AiConfig::load(&app.paths.ai_file())?;
    let name = cfg.default.provider.clone();
    println!("provider: {name}");
    println!("model: {}", cfg.model(&name));
    let p = cfg.provider(&name);
    let base = p.chat_base();
    match (&name[..], p.api_key) {
        ("custom", key) => {
            println!("endpoint: {}", base.as_deref().unwrap_or("not set"));
            match key {
                Some(k) => println!("api key: {k:?}"),
                None => println!("api key: none"),
            }
        }
        ("ollama", _) => println!(
            "endpoint: {}",
            p.base_url.as_deref().unwrap_or("http://localhost:11434")
        ),
        (_, Some(k)) => println!("api key: {k:?}"),
        (_, None) => println!("api key: from the {}_API_KEY variable", name.to_uppercase()),
    }
    println!("config: {}", app.paths.ai_file().display());
    Ok(0)
}

/// `ai ...`. `model` is `ai --model`; the other modes read it through
/// [`App::ai`], and `ai claude` passes it to Claude Code.
pub async fn ai(app: &mut App, cmd: AiCmd, model: Option<String>) -> Result<i32> {
    match cmd {
        AiCmd::Claude { auto, goal } => {
            crate::claude::run(app, &goal.join(" "), auto, model.as_deref()).await
        }
        AiCmd::Status => ai_status(app),
        // `ai <text>` with no subcommand explains too.
        AiCmd::Explain { command } | AiCmd::Ask(command) => {
            let command = command.join(" ");
            let rt = app.ai()?;
            let ctx = app.ai_context(&rt.cfg).await;
            let assistant = Assistant::new(&*rt.provider, Settings::from(&rt.cfg));
            let text = until_interrupt(assistant.explain(&command, &ctx)).await??;
            let report = format!("### Question\n{}\n\n### Answer\n{text}", command.trim());
            print!("\n{}", Pal::for_app(app).bubble(&report, "xsh"));
            // A question is not a command: only warn about text that parses.
            let verdict = rushy_ai::check_command(&command);
            if !verdict.is_allowed() && verdict.rule() != Some("unparseable") {
                println!("\nwarning: {verdict}");
            }
            app.save_transcript(
                "explain",
                rt.provider.name(),
                vec![
                    ChatMessage::user(&command),
                    ChatMessage::assistant(text, Vec::new()),
                ],
            );
            Ok(0)
        }
        AiCmd::Fix => {
            let Some(last) = app.shell.last_failure() else {
                eprintln!(
                    "xsh: no failed command in this session; run `ai fix` at the xsh prompt after a command fails"
                );
                return Ok(1);
            };
            let rt = app.ai()?;
            let ctx = app.ai_context(&rt.cfg).await;
            let assistant = Assistant::new(&*rt.provider, Settings::from(&rt.cfg));
            let git = if rt.cfg.context.git_status {
                git_status(&last.cwd).await
            } else {
                None
            };
            if last.stderr.is_empty() && !app.cfg.shell.capture_stderr {
                eprintln!(
                    "xsh: note: shell.capture_stderr is off, so the error text is not available"
                );
            }
            let failure = Failure {
                command: last.command.clone(),
                stderr: last.stderr.clone(),
                exit_code: last.exit_code,
                cwd: last.cwd.display().to_string(),
                git_status: git,
            };
            let s = until_interrupt(assistant.fix(&failure, &ctx)).await??;
            app.save_transcript(
                "fix",
                rt.provider.name(),
                vec![
                    ChatMessage::user(format!(
                        "{} (exit {})\n{}",
                        failure.command, failure.exit_code, failure.stderr
                    )),
                    ChatMessage::assistant(
                        format!("{} -- {}", s.command, s.explanation),
                        Vec::new(),
                    ),
                ],
            );
            let pal = Pal::for_app(app);
            if s.command.trim().is_empty() {
                print!("{}", pal.answer(&s.explanation));
                return Ok(0);
            }
            print!("{}", pal.suggestion(&s.command, &s.explanation));
            run_choice(app, confirm(&assistant, &ctx, &s.command, false).await).await
        }
        AiCmd::Suggest { description } => {
            let choice = suggest(app, &description.join(" "), false).await?;
            run_choice(app, choice).await
        }
        AiCmd::Agent { auto, goal } => agent_goal(app, &goal.join(" "), auto).await,
    }
}

async fn run_choice(app: &mut App, choice: Choice) -> Result<i32> {
    match choice {
        Choice::Run(cmd) | Choice::Insert(cmd) => {
            match Box::pin(crate::exec::execute(app, &cmd)).await {
                Ok(rushy_core::Flow::Exit(code)) => Ok(code),
                Ok(rushy_core::Flow::Continue) => Ok(app.shell.last_status()),
                Err(e) => bail!("{e}"),
            }
        }
        Choice::Cancel => Ok(0),
    }
}

/// Runs agent mode for `goal`. With `auto`, or the `autopilot` setting, no step
/// asks a question. The safety checks still apply to each command.
pub async fn agent_goal(app: &mut App, goal: &str, auto: bool) -> Result<i32> {
    let rt = app.ai()?;
    let ctx = app.ai_context(&rt.cfg).await;
    let assistant = Assistant::new(&*rt.provider, Settings::from(&rt.cfg));
    let auto = auto || rt.cfg.default.autopilot;
    let ask_each = !auto && rt.cfg.default.approve_each_step;
    let limit = if auto {
        agent::AUTOPILOT_MAX_STEPS
    } else {
        agent::MAX_STEPS
    };
    let tty = std::io::stdin().is_terminal();
    let pal = Pal::for_app(app);
    eprint!("{}", pal.question(goal));
    if auto {
        eprintln!(
            "autopilot: commands run without a question, at most {limit} steps; the safety checks stay on; Ctrl-C stops"
        );
    } else if !tty && ask_each {
        eprintln!("xsh: agent steps need approval; every step without a reply is skipped");
    }
    let link = Link::open(app.shell.state().cwd()).await;
    let fg = app.shell.state().foreground();
    let busy = Arc::new(AtomicBool::new(false));
    let interrupted = Arc::new(AtomicBool::new(false));
    let mut host = CliHost {
        shell: &mut app.shell,
        ask_each,
        link,
        pane_closed: false,
        busy: Arc::clone(&busy),
        interrupted: Arc::clone(&interrupted),
        pal: pal.clone(),
        thinking: false,
        steps: Vec::new(),
    };
    let finished = {
        let work = agent::run_limited(&assistant, goal, &ctx, &mut host, limit);
        tokio::pin!(work);
        loop {
            tokio::select! {
                res = &mut work => break Some(res),
                Ok(()) = tokio::signal::ctrl_c() => {
                    // A command in this pane must end first: the shell still
                    // waits for it. The agent then stops before the next step.
                    if !busy.load(Ordering::SeqCst) {
                        break None;
                    }
                    interrupted.store(true, Ordering::SeqCst);
                    if !tty {
                        let _ = fg.signal("INT");
                    }
                }
            }
        }
    };
    host.clear_thinking();
    let steps = std::mem::take(&mut host.steps);
    drop(host);
    let Some(out) = finished else {
        let answer = format!("*Stopped with Ctrl-C after {} step(s).*", steps.len());
        print!("\n{}", pal.agent_report(goal, &answer, &steps));
        return Ok(130);
    };
    let out = out?;
    app.save_transcript("agent", rt.provider.name(), out.messages.clone());
    let answer = match out.stop {
        Stop::Done if out.summary.trim().is_empty() => "*The agent gave no summary.*".to_owned(),
        Stop::Done => out.summary,
        Stop::StepLimit => format!("*Stopped after {limit} steps (the limit).*"),
        Stop::Aborted => format!("*Stopped by you after {} step(s).*", out.steps),
    };
    print!("\n{}", pal.agent_report(goal, &answer, &steps));
    Ok(0)
}
