//! Shell session: ties the parser, state, executor, and job table together.

use std::path::Path;

use crate::exec::{Ctl, Ctx, Io};
use crate::jobs::{JobInfo, JobTable};
use crate::state::ShellState;
use crate::{Error, Result, parser};

/// What the caller should do after running a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep reading input.
    Continue,
    /// Exit the shell with this status.
    Exit(i32),
}

/// A command that ended with a non-zero status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRun {
    /// The command line.
    pub command: String,
    /// Its standard error (empty unless `shell.capture_stderr` is on).
    pub stderr: String,
    /// Its exit status.
    pub exit_code: i32,
    /// The working directory.
    pub cwd: std::path::PathBuf,
}

/// A shell session: state plus background jobs.
#[derive(Debug)]
pub struct Shell {
    state: ShellState,
    jobs: JobTable,
    failure: Option<LastRun>,
}

impl Shell {
    /// Creates a shell from explicit state.
    #[must_use]
    pub fn new(state: ShellState) -> Self {
        Self {
            state,
            jobs: JobTable::default(),
            failure: None,
        }
    }

    /// Creates a shell from the process environment and working directory.
    ///
    /// # Errors
    ///
    /// Fails if the current directory cannot be read.
    pub fn from_env() -> Result<Self> {
        Ok(Self::new(ShellState::from_env()?))
    }

    /// Parses and runs one command line, which may span several lines.
    ///
    /// Lines that start with a space are not added to history.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Incomplete`] when more input is needed, [`Error::Parse`]
    /// on syntax errors (and sets `$?` to 2), and [`Error::Io`] if waiting on a
    /// process or writing output fails.
    pub async fn run_line(&mut self, line: &str) -> Result<Flow> {
        let list = match parser::parse_program(line, self.state.aliases()) {
            Ok(p) => p,
            Err(e) => {
                if !matches!(e, Error::Incomplete) {
                    self.record_external(line.trim(), 2, &format!("xsh: {e}\n"));
                }
                return Err(e);
            }
        };
        let text = line.trim();
        if !line.starts_with(' ') {
            self.state.push_history(text);
        }
        if self.state.options().noexec {
            return Ok(Flow::Continue);
        }
        let ctl = {
            let mut ctx = Ctx::new(&mut self.state, &mut self.jobs, text.to_owned());
            ctx.run_list(&list, &Io::default()).await?
        };
        if let Ctl::Exit(code) = ctl {
            self.state.set_last_status(code);
            self.run_exit_trap().await?;
            return Ok(Flow::Exit(code));
        }
        self.failure = (self.state.last_status() != 0).then(|| LastRun {
            command: text.to_owned(),
            stderr: self.state.last_stderr(),
            exit_code: self.state.last_status(),
            cwd: self.state.cwd().to_path_buf(),
        });
        Ok(Flow::Continue)
    }

    /// Runs a whole script, one complete command at a time, so aliases
    /// that one line defines apply to the lines after it. Nothing is added
    /// to history.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if waiting on a process or writing output fails.
    pub async fn run_script(&mut self, text: &str) -> Result<Flow> {
        if self.state.options().noexec {
            parser::parse_program(text, self.state.aliases())?;
            return Ok(Flow::Continue);
        }
        let ctl = {
            let mut ctx = Ctx::new(&mut self.state, &mut self.jobs, String::new());
            ctx.top = false;
            ctx.run_script(text, &Io::default()).await?
        };
        if let Ctl::Exit(code) = ctl {
            self.state.set_last_status(code);
            self.run_exit_trap().await?;
            return Ok(Flow::Exit(code));
        }
        Ok(Flow::Continue)
    }

    /// Runs the `EXIT` trap once, if one is set. Call it before the shell
    /// exits for a reason other than the `exit` built-in (such as end of
    /// input).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if writing output fails.
    pub async fn run_exit_trap(&mut self) -> Result<()> {
        let Some(cmd) = self.state.traps_mut().remove("EXIT") else {
            return Ok(());
        };
        let status = self.state.last_status();
        self.run_trap_text(&cmd).await?;
        self.state.set_last_status(status);
        Ok(())
    }

    /// Runs the trap set for `condition` (such as `INT`, `TERM`, or `HUP`).
    /// Returns `false` when no trap is set for it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if writing output fails.
    pub async fn run_trap(&mut self, condition: &str) -> Result<bool> {
        let Some(cmd) = self.state.trap(condition).map(str::to_owned) else {
            return Ok(false);
        };
        let status = self.state.last_status();
        self.run_trap_text(&cmd).await?;
        self.state.set_last_status(status);
        Ok(true)
    }

    async fn run_trap_text(&mut self, cmd: &str) -> Result<()> {
        if cmd.is_empty() {
            return Ok(());
        }
        let mut ctx = Ctx::new(&mut self.state, &mut self.jobs, cmd.to_owned());
        ctx.top = false;
        ctx.run_text(cmd, &Io::default()).await?;
        Ok(())
    }

    /// Number of background jobs that were running when last checked.
    #[must_use]
    pub fn job_count(&self) -> usize {
        self.jobs.active()
    }

    /// Names of the defined shell functions, sorted.
    #[must_use]
    pub fn function_names(&self) -> Vec<String> {
        self.state.function_names()
    }

    /// Removes finished background jobs from the job table and returns them.
    #[must_use]
    pub fn reap_jobs(&mut self) -> Vec<JobInfo> {
        self.jobs.reap()
    }

    /// Shell state.
    #[must_use]
    pub fn state(&self) -> &ShellState {
        &self.state
    }

    /// Mutable shell state.
    #[must_use]
    pub fn state_mut(&mut self) -> &mut ShellState {
        &mut self.state
    }

    /// Exit status of the last command.
    #[must_use]
    pub fn last_status(&self) -> i32 {
        self.state.last_status()
    }

    /// The most recent command that failed, until the next command succeeds.
    #[must_use]
    pub fn last_failure(&self) -> Option<LastRun> {
        self.failure.clone()
    }

    /// Records a command that ran outside the executor (such as `bundle` or
    /// `ai`) so `$?` and `ai fix` see it. A failure replaces the remembered
    /// one; a success sets `$?` but keeps it, so `ai explain` does not hide a
    /// failure that `ai fix` should look at.
    pub fn record_external(&mut self, command: &str, status: i32, stderr: &str) {
        self.state.set_last_status(status);
        if status != 0 {
            self.failure = Some(LastRun {
                command: command.to_owned(),
                stderr: stderr.to_owned(),
                exit_code: status,
                cwd: self.state.cwd().to_path_buf(),
            });
        }
    }

    /// Default prompt: `[status] user@host path ❯ `.
    #[must_use]
    pub fn prompt(&self) -> String {
        let s = &self.state;
        let user = s.var("USER").unwrap_or("user");
        let host = s.var("HOSTNAME").unwrap_or("localhost");
        let path = display_path(s.cwd(), s.home().as_deref());
        let status = match s.last_status() {
            0 => String::new(),
            code => format!("[{code}] "),
        };
        format!("{status}{user}@{host} {path} ❯ ")
    }
}

fn display_path(cwd: &Path, home: Option<&str>) -> String {
    if let Some(rest) = home
        .filter(|h| !h.is_empty())
        .and_then(|h| cwd.strip_prefix(h).ok())
    {
        return if rest.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", rest.display())
        };
    }
    cwd.display().to_string()
}
