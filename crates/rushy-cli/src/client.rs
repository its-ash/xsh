//! The multiplexer client: `xsh new`, `attach`, `ls`, and `kill-session`.

use std::io::{self, Stdout};
use std::os::unix::process::CommandExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode, size,
};
use rushy_pty::proto::{ClientMsg, ServerMsg, read_frame, write_frame};
use rushy_pty::server::{self, Connection};
use rushy_pty::{PaneCommand, View};
use rushy_tui::mux::{self, Options, Screens};
use rushy_tui::{AiState, StatusInfo, clock_now};
use tokio::sync::mpsc;

use crate::app::App;
use crate::keys::{Action, Keymap, parse_prefix};

/// The program each pane runs: `[mux].shell`, or this very binary.
fn pane_command(app: &App) -> Result<PaneCommand> {
    let program = if app.cfg.mux.shell.is_empty() {
        std::env::current_exe().context("cannot find the xsh binary")?
    } else {
        app.cfg.mux.shell.clone().into()
    };
    let mut cmd = PaneCommand::new(program);
    cmd.cwd = Some(app.shell.state().cwd().to_path_buf());
    Ok(cmd)
}

/// Runs the multiplexer server in this process until the last session ends.
pub async fn serve(app: &App) -> Result<i32> {
    server::serve(&app.paths.mux_dir(), pane_command(app)?).await?;
    Ok(0)
}

fn spawn_server() -> Result<()> {
    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("server")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own process group, so closing this terminal does not end the sessions.
    cmd.process_group(0);
    cmd.spawn().context("cannot start the multiplexer server")?;
    Ok(())
}

async fn connect(app: &App, start: bool) -> Result<Connection> {
    let dir = app.paths.mux_dir();
    if let Ok(c) = server::connect(&dir).await {
        return Ok(c);
    }
    if !start {
        bail!("no multiplexer server is running; start one with `xsh new`");
    }
    spawn_server()?;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        if let Ok(c) = server::connect(&dir).await {
            return Ok(c);
        }
    }
    bail!("the multiplexer server did not start")
}

async fn request(app: &App, msg: &ClientMsg) -> Result<ServerMsg> {
    let mut conn = connect(app, false).await?;
    write_frame(&mut conn, msg).await?;
    read_frame(&mut conn)
        .await?
        .context("the server closed the connection")
}

/// `ls`
pub async fn list(app: &App) -> Result<i32> {
    match request(app, &ClientMsg::List).await {
        Ok(ServerMsg::Sessions(s)) if s.is_empty() => {
            println!("no sessions");
            Ok(0)
        }
        Ok(ServerMsg::Sessions(s)) => {
            for i in s {
                println!(
                    "{}: {} windows, {} panes{}",
                    i.name,
                    i.windows,
                    i.panes,
                    if i.attached { " (attached)" } else { "" }
                );
            }
            Ok(0)
        }
        Ok(ServerMsg::Error(e)) => bail!("{e}"),
        Ok(_) => bail!("unexpected reply from the server"),
        Err(_) => {
            println!("no sessions");
            Ok(0)
        }
    }
}

/// `kill-session`
pub async fn kill(app: &App, name: &str) -> Result<i32> {
    match request(app, &ClientMsg::Kill(name.to_owned())).await? {
        ServerMsg::Ok => {
            println!("killed session {name}");
            Ok(0)
        }
        ServerMsg::Error(e) => bail!("{e}"),
        _ => bail!("unexpected reply from the server"),
    }
}

struct TermGuard;

impl TermGuard {
    fn enter() -> io::Result<(Self, Terminal<CrosstermBackend<Stdout>>)> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        Ok((guard, term))
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
        let _ = disable_raw_mode();
    }
}

enum Input {
    Key(ratatui::crossterm::event::KeyEvent),
    Resize(u16, u16),
}

fn spawn_input(tx: mpsc::UnboundedSender<Input>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            match event::poll(Duration::from_millis(100)) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => break,
            }
            let msg = match event::read() {
                Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => Input::Key(k),
                Ok(Event::Resize(c, r)) => Input::Resize(c, r),
                Ok(_) => continue,
                Err(_) => break,
            };
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
}

fn status_info(app: &App, view: &View, ai: &AiState) -> StatusInfo {
    let focused = view
        .panes
        .iter()
        .find(|p| p.focused)
        .map_or_else(String::new, |p| p.id.to_string());
    StatusInfo {
        session: view.session.name.clone(),
        window: view
            .windows
            .iter()
            .position(|w| w.active)
            .map_or(1, |i| i + 1),
        windows: view.windows.len(),
        pane: focused,
        bundles: app.effects.active.len(),
        ai: ai.clone(),
        clock: clock_now(),
    }
}

/// `attach` and `new`: draws the session and forwards keys until detached.
#[allow(
    clippy::too_many_lines,
    reason = "one event loop; splitting it would only move shared state into a struct"
)]
pub async fn attach(app: &mut App, session: &str, create: bool) -> Result<i32> {
    let prefix = parse_prefix(&app.cfg.mux.prefix).with_context(|| {
        format!(
            "invalid mux.prefix `{}`; use a form like C-b",
            app.cfg.mux.prefix
        )
    })?;
    app.refresh_effects();
    let (mut rx, mut tx) = connect(app, create).await?.into_split();
    let ai = match rushy_ai::AiConfig::load(&app.paths.ai_file()) {
        Ok(c) if app.paths.ai_file().exists() => AiState::Ready(c.default.provider),
        _ => AiState::Off,
    };
    let opts = Options {
        ascii: app.cfg.ui.screen_reader,
        status_bar: app.cfg.ui.status_bar,
    };
    let (cols, rows) = size().context("a terminal is required to attach")?;
    let (pc, pr) = mux::pane_area((cols, rows), opts.status_bar);

    let (srv_tx, mut srv_rx) = mpsc::unbounded_channel::<ServerMsg>();
    tokio::spawn(async move {
        while let Ok(Some(msg)) = read_frame::<_, ServerMsg>(&mut rx).await {
            if srv_tx.send(msg).is_err() {
                return;
            }
        }
    });
    write_frame(
        &mut tx,
        &ClientMsg::Attach {
            session: session.to_owned(),
            create,
            cols: pc,
            rows: pr,
            cwd: Some(app.shell.state().cwd().display().to_string()),
        },
    )
    .await?;

    let (guard, mut term) = TermGuard::enter()?;
    let stop = Arc::new(AtomicBool::new(false));
    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
    spawn_input(in_tx, Arc::clone(&stop));

    let mut keymap = Keymap::new(prefix);
    let mut screens = Screens::new();
    let mut view: Option<View> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(33));
    let mut clock = clock_now();
    let (mut dirty, mut failure) = (true, None::<String>);
    let outcome = loop {
        tokio::select! {
            Some(msg) = srv_rx.recv() => match msg {
                ServerMsg::View(v) => { screens.sync(&v); view = Some(v); dirty = true; }
                ServerMsg::Snapshot(id, b) => { screens.snapshot(id, &b); dirty = true; }
                ServerMsg::Output(id, b) => { screens.output(id, &b); dirty = true; }
                ServerMsg::Detached => break "detached",
                ServerMsg::Exited => break "exited",
                ServerMsg::Error(e) => {
                    if view.is_none() { failure = Some(e); break "failed"; }
                }
                ServerMsg::Ok | ServerMsg::Sessions(_) => {}
            },
            Some(input) = in_rx.recv() => {
                let msg = match input {
                    Input::Key(k) => match keymap.feed(k) {
                        Action::Send(bytes) if !bytes.is_empty() => Some(ClientMsg::Input(bytes)),
                        Action::Command(c) => Some(ClientMsg::Command(c)),
                        Action::Send(_) | Action::Ignore => None,
                    },
                    Input::Resize(c, r) => {
                        let (pc, pr) = mux::pane_area((c, r), opts.status_bar);
                        Some(ClientMsg::Resize { cols: pc, rows: pr })
                    }
                };
                if let Some(m) = msg {
                    // A detach is answered with `Detached`, which ends the loop.
                    if write_frame(&mut tx, &m).await.is_err() { break "lost"; }
                }
                dirty = true;
            }
            _ = tick.tick() => {
                let now = clock_now();
                if now != clock { clock = now; dirty = true; }
                if dirty {
                    if let Some(v) = &view {
                        let info = status_info(app, v, &ai);
                        term.draw(|f| mux::draw(f, v, &screens, &app.theme, Some(&info), opts))?;
                    }
                    dirty = false;
                }
            }
            else => break "lost",
        }
    };
    stop.store(true, Ordering::Relaxed);
    drop(guard);
    drop(term);
    match outcome {
        "detached" => println!("[detached from session {session}]"),
        "exited" => println!("[session {session} ended]"),
        "lost" => println!("[lost connection to the multiplexer server]"),
        _ => {
            if let Some(e) = failure {
                bail!("{e}");
            }
        }
    }
    Ok(0)
}
