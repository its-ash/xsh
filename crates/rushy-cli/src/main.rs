//! The `xsh` binary entry point.
#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]

#[cfg(not(target_os = "macos"))]
compile_error!("xsh supports macOS only");

mod adb;
mod agent_pane;
mod app;
mod bridge;
mod cli;
mod client;
mod cmds;
mod exec;
mod highlight;
mod hist;
mod keys;
mod man;
mod rcimport;
mod repl;
mod subcmds;
mod term;
mod update;

use std::io::IsTerminal as _;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser as _;
use rushy_config::Overrides;
use rushy_core::{Error, Flow};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::app::App;
use crate::cli::{AppCmd, Cli, Sub};

const DEFAULT_SESSION: &str = "main";

/// Runs one subcommand; shared by the process arguments and the prompt.
pub(crate) async fn dispatch(app: &mut App, sub: Sub) -> Result<i32> {
    match sub {
        Sub::Bundle(c) => cmds::bundle(app, c).await,
        Sub::Ai(c) => cmds::ai(app, c).await,
        Sub::History(a) => cmds::history(app, a),
        Sub::Theme { name } => cmds::theme(app, name),
        Sub::Config(c) => cmds::config(app, c),
        Sub::Attach { session } => {
            let name = session.unwrap_or_else(|| DEFAULT_SESSION.to_owned());
            client::attach(app, &name, false).await
        }
        Sub::New { name } => {
            let name = name.unwrap_or_else(|| DEFAULT_SESSION.to_owned());
            client::attach(app, &name, true).await
        }
        Sub::AdbFind => adb::find(app.paths.data_dir()).await,
        Sub::AdbConnect => adb::connect(app.paths.data_dir()).await,
        Sub::Ls => client::list(app).await,
        Sub::KillSession { name } => client::kill(app, &name).await,
        Sub::Server => client::serve(app).await,
        Sub::App(c) => bridge::run(app, c).await,
    }
}

async fn run_once(app: &mut App, line: &str) -> i32 {
    match exec::execute(app, line).await {
        Ok(Flow::Exit(code)) => code,
        Ok(Flow::Continue) => app.shell.last_status(),
        Err(e) => {
            eprintln!("xsh: {e}");
            2
        }
    }
}

/// Runs the lines of `text` as a script; a construct that spans lines
/// (`if` … `fi`, a quote) runs once it is complete.
async fn run_block(app: &mut App, text: &str, name: &str) -> i32 {
    let mut buf = String::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 && line.starts_with("#!") {
            continue;
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(line);
        match exec::execute(app, &buf).await {
            Ok(Flow::Exit(code)) => return code,
            Ok(Flow::Continue) => buf.clear(),
            Err(Error::Incomplete) => {}
            Err(e) => {
                eprintln!("xsh: {name}:{}: {e}", i + 1);
                buf.clear();
            }
        }
    }
    if buf.is_empty() {
        app.shell.last_status()
    } else {
        eprintln!("xsh: {name}: {}", Error::Incomplete);
        2
    }
}

/// Runs the start-up file, if there is one.
async fn run_rc(app: &mut App) {
    rcimport::import(app).await;
    let path = app.rc_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            // Start-up lines are not history.
            let record = std::mem::replace(&mut app.cfg.history.enabled, false);
            let status = app.shell.last_status();
            run_block(app, &text, &path.display().to_string()).await;
            app.shell.state_mut().set_last_status(status);
            app.cfg.history.enabled = record;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("xsh: {}: {e}", path.display()),
    }
}

/// Reads commands from a pipe or file on standard input.
async fn script(app: &mut App) -> Result<i32> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut buf = String::new();
    while let Some(line) = lines.next_line().await? {
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(&line);
        match exec::execute(app, &buf).await {
            Ok(Flow::Exit(code)) => return Ok(code),
            Ok(Flow::Continue) => buf.clear(),
            Err(Error::Incomplete) => {}
            Err(e) => {
                eprintln!("xsh: {e}");
                buf.clear();
            }
        }
    }
    if buf.is_empty() {
        Ok(app.shell.last_status())
    } else {
        eprintln!("xsh: {}", Error::Incomplete);
        Ok(2)
    }
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let cli = Cli::parse();
    let overrides = Overrides {
        theme: cli.theme.clone(),
        screen_reader: cli.screen_reader.then_some(true),
        no_color: cli.no_color.then_some(true),
    };
    // The agent pane ends as a normal shell, so it starts like one.
    let shell_sub = matches!(cli.sub, None | Some(Sub::App(AppCmd::AgentRunner { .. })));
    let interactive = cli.command.is_none()
        && shell_sub
        && cli.script.is_none()
        && std::io::stdin().is_terminal();
    let mut app = App::load(overrides, interactive)?;
    for (on, name) in [
        (cli.xtrace, "xtrace"),
        (cli.errexit, "errexit"),
        (cli.noexec, "noexec"),
    ] {
        if on && !app.shell.state_mut().set_option(name, true) {
            eprintln!("xsh: option `{name}` is not supported");
        }
    }
    if interactive && !cli.norc {
        let _ = app.refresh_effects();
        run_rc(&mut app).await;
    }
    let status = if let Some(line) = cli.command {
        let _ = app.refresh_effects();
        run_once(&mut app, &line).await
    } else if let Some(path) = cli.script {
        let _ = app.refresh_effects();
        let name = path.display().to_string();
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                app.shell.state_mut().set_positional(name.clone(), cli.args);
                run_block(&mut app, &text, &name).await
            }
            Err(e) => {
                eprintln!("xsh: {name}: {e}");
                127
            }
        }
    } else if let Some(sub) = cli.sub {
        match dispatch(&mut app, sub).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("xsh: {e:#}");
                1
            }
        }
    } else if interactive {
        repl::run(&mut app).await?
    } else {
        let _ = app.refresh_effects();
        script(&mut app).await?
    };
    Ok(ExitCode::from(
        u8::try_from(status.rem_euclid(256)).unwrap_or(u8::MAX),
    ))
}
