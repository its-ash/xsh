//! Mutable shell state: working directory, variables, aliases, and history.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::Result;
use crate::ast::Command;

const HISTORY_LIMIT: usize = 10_000;
const STDERR_LIMIT: usize = 16 * 1024;
const LOG_LIMIT: usize = 64 * 1024;

/// Shared list of the process ids of the running foreground command.
///
/// Clone it before calling [`crate::Shell::run_line`], then use it from a
/// signal task to forward `SIGTERM` or `SIGHUP` to the running command.
///
/// It also carries `Ctrl-Z`: a signal task calls [`Self::request_stop`] on
/// `SIGTSTP`, and the shell then stops waiting and keeps the command as a
/// stopped job.
#[derive(Debug, Clone, Default)]
pub struct ForegroundHandle {
    pids: Arc<Mutex<Vec<u32>>>,
    stop: Arc<tokio::sync::Notify>,
    stopped: Arc<std::sync::atomic::AtomicBool>,
}

impl ForegroundHandle {
    pub(crate) fn set(&self, pids: Vec<u32>) {
        if let Ok(mut g) = self.pids.lock() {
            *g = pids;
        }
    }

    /// Process ids of the running foreground command.
    #[must_use]
    pub fn pids(&self) -> Vec<u32> {
        self.pids.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Reports `Ctrl-Z` (`SIGTSTP`). The terminal already stopped the
    /// foreground processes; the shell stops waiting for them. Does nothing
    /// when no foreground command runs.
    pub fn request_stop(&self) {
        if !self.pids().is_empty() {
            self.stopped
                .store(true, std::sync::atomic::Ordering::SeqCst);
            self.stop.notify_waiters();
        }
    }

    /// Whether the running command was stopped by `Ctrl-Z`.
    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Clears the stop mark before a new foreground command.
    pub(crate) fn clear_stop(&self) {
        self.stopped
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Resolves when the running command is stopped by `Ctrl-Z`.
    pub(crate) async fn stopped(&self) {
        loop {
            let n = self.stop.notified();
            if self.is_stopped() {
                return;
            }
            n.await;
        }
    }

    /// Sends `signal` (such as `TERM`, `INT`, or `HUP`) to every foreground
    /// process and returns how many were signaled.
    #[must_use]
    pub fn signal(&self, signal: &str) -> usize {
        self.pids()
            .into_iter()
            .filter(|pid| {
                std::process::Command::new("kill")
                    .arg(format!("-{signal}"))
                    .arg(pid.to_string())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            })
            .count()
    }
}

/// Shell options set with `set` and `shopt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Options {
    /// `set -e`: exit when a command fails.
    pub errexit: bool,
    /// `set -x`: print each command before it runs.
    pub xtrace: bool,
    /// `set -u`: expanding an unset variable is an error.
    pub nounset: bool,
    /// `set -o pipefail`: a pipeline fails if any stage fails.
    pub pipefail: bool,
    /// `set -f`: no file name expansion.
    pub noglob: bool,
    /// `set -C`: `>` does not overwrite files (`>|` does).
    pub noclobber: bool,
    /// `set -n`: read commands but do not run them.
    pub noexec: bool,
    /// `shopt autocd`: a directory name alone changes to it.
    pub autocd: bool,
    /// `shopt nullglob`: a pattern with no match expands to nothing.
    pub nullglob: bool,
    /// `shopt failglob`: a pattern with no match is an error.
    pub failglob: bool,
    /// `shopt dotglob`: patterns match names that start with `.`.
    pub dotglob: bool,
    /// `shopt globstar`: `**` matches any depth of directories.
    pub globstar: bool,
    /// `shopt nocaseglob`: patterns ignore case.
    pub nocaseglob: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            errexit: false,
            xtrace: false,
            nounset: false,
            pipefail: false,
            noglob: false,
            noclobber: false,
            noexec: false,
            autocd: false,
            nullglob: false,
            failglob: false,
            dotglob: false,
            globstar: true,
            nocaseglob: false,
        }
    }
}

/// Names accepted by `set -o`.
pub const SET_OPTIONS: &[&str] = &[
    "errexit",
    "noclobber",
    "noexec",
    "noglob",
    "nounset",
    "pipefail",
    "xtrace",
];

/// Names accepted by `shopt`.
pub const SHOPT_OPTIONS: &[&str] = &[
    "autocd",
    "dotglob",
    "failglob",
    "globstar",
    "nocaseglob",
    "nullglob",
];

impl Options {
    fn slot(&mut self, name: &str) -> Option<&mut bool> {
        Some(match name {
            "errexit" => &mut self.errexit,
            "xtrace" => &mut self.xtrace,
            "nounset" => &mut self.nounset,
            "pipefail" => &mut self.pipefail,
            "noglob" => &mut self.noglob,
            "noclobber" => &mut self.noclobber,
            "noexec" => &mut self.noexec,
            "autocd" => &mut self.autocd,
            "nullglob" => &mut self.nullglob,
            "failglob" => &mut self.failglob,
            "dotglob" => &mut self.dotglob,
            "globstar" => &mut self.globstar,
            "nocaseglob" => &mut self.nocaseglob,
            _ => return None,
        })
    }

    /// Value of the option `name` (a `set -o` or `shopt` name).
    #[must_use]
    pub fn get(&self, name: &str) -> Option<bool> {
        let mut copy = *self;
        copy.slot(name).map(|v| *v)
    }

    /// Sets the option `name`; returns `false` if the name is unknown.
    pub fn set(&mut self, name: &str, on: bool) -> bool {
        match self.slot(name) {
            Some(v) => {
                *v = on;
                true
            }
            None => false,
        }
    }

    /// The `$-` letters for the options that are on.
    #[must_use]
    pub fn flags(&self) -> String {
        [
            (self.errexit, 'e'),
            (self.noglob, 'f'),
            (self.noexec, 'n'),
            (self.nounset, 'u'),
            (self.xtrace, 'x'),
            (self.noclobber, 'C'),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, c)| *c)
        .collect()
    }
}

/// Working directory, environment, aliases, last status, and in-memory history.
///
/// Only exported variables reach child processes: those inherited from the
/// environment and those named by `export`.
#[derive(Debug, Clone)]
pub struct ShellState {
    cwd: PathBuf,
    vars: BTreeMap<String, String>,
    exported: std::collections::BTreeSet<String>,
    aliases: BTreeMap<String, String>,
    last_status: i32,
    history: Vec<String>,
    capture_stderr: bool,
    last_command: String,
    last_stderr: Vec<u8>,
    log: bool,
    out_log: Vec<u8>,
    err_log: Vec<u8>,
    foreground: ForegroundHandle,
    functions: BTreeMap<String, Arc<Command>>,
    options: Options,
    arg0: String,
    positional: Vec<String>,
    locals: Vec<Vec<(String, Option<String>)>>,
    dirs: Vec<PathBuf>,
    traps: BTreeMap<String, String>,
    cdpath: Option<Vec<String>>,
    umask: Option<u32>,
    last_bg: Option<u32>,
}

impl ShellState {
    /// Creates state with the given working directory and variables. The
    /// variables are exported, as an inherited environment is.
    #[must_use]
    pub fn new(cwd: PathBuf, vars: impl IntoIterator<Item = (String, String)>) -> Self {
        let vars: BTreeMap<String, String> = vars.into_iter().collect();
        Self {
            cwd,
            exported: vars.keys().cloned().collect(),
            vars,
            aliases: BTreeMap::new(),
            last_status: 0,
            history: Vec::new(),
            capture_stderr: false,
            last_command: String::new(),
            last_stderr: Vec::new(),
            log: false,
            out_log: Vec::new(),
            err_log: Vec::new(),
            foreground: ForegroundHandle::default(),
            functions: BTreeMap::new(),
            options: Options::default(),
            arg0: "xsh".to_owned(),
            positional: Vec::new(),
            locals: Vec::new(),
            dirs: Vec::new(),
            traps: BTreeMap::new(),
            cdpath: None,
            umask: None,
            last_bg: None,
        }
    }

    /// Shell options.
    #[must_use]
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// Mutable shell options.
    pub fn options_mut(&mut self) -> &mut Options {
        &mut self.options
    }

    /// Sets a `set -o` or `shopt` option by name, such as `xtrace`,
    /// `errexit`, `nounset`, `pipefail`, `noexec`, or `autocd`. Returns
    /// `false` if the name is unknown.
    pub fn set_option(&mut self, name: &str, on: bool) -> bool {
        self.options.set(name, on)
    }

    /// Turns `autocd` on or off: a directory name alone changes to it.
    pub fn set_auto_cd(&mut self, on: bool) {
        self.options.autocd = on;
    }

    /// Directories that `cd` searches for relative names. When set, this
    /// list replaces the `CDPATH` variable.
    pub fn set_cdpath(&mut self, dirs: Vec<String>) {
        self.cdpath = Some(dirs);
    }

    /// The `cd` search path: the configured list, else `CDPATH`.
    #[must_use]
    pub fn cdpath(&self) -> Vec<String> {
        match &self.cdpath {
            Some(d) => d.clone(),
            None => self
                .var("CDPATH")
                .map(|p| p.split(':').map(str::to_owned).collect())
                .unwrap_or_default(),
        }
    }

    /// Defined functions, sorted by name.
    #[must_use]
    pub fn functions(&self) -> &BTreeMap<String, Arc<Command>> {
        &self.functions
    }

    /// Names of the defined functions, sorted.
    #[must_use]
    pub fn function_names(&self) -> Vec<String> {
        self.functions.keys().cloned().collect()
    }

    pub(crate) fn set_function(&mut self, name: String, body: Arc<Command>) {
        self.functions.insert(name, body);
    }

    pub(crate) fn remove_function(&mut self, name: &str) -> bool {
        self.functions.remove(name).is_some()
    }

    /// Sets `$0` and the positional parameters `$1`..`$N`.
    pub fn set_positional(&mut self, zero: String, args: Vec<String>) {
        self.arg0 = zero;
        self.positional = args;
    }

    /// The positional parameters `$1`..`$N`.
    #[must_use]
    pub fn positional(&self) -> &[String] {
        &self.positional
    }

    pub(crate) fn replace_positional(&mut self, args: Vec<String>) -> Vec<String> {
        std::mem::replace(&mut self.positional, args)
    }

    pub(crate) fn push_frame(&mut self) {
        self.locals.push(Vec::new());
    }

    pub(crate) fn pop_frame(&mut self) {
        if let Some(frame) = self.locals.pop() {
            for (name, old) in frame.into_iter().rev() {
                match old {
                    Some(v) => self.set_var(name, v),
                    None => self.unset_var(&name),
                }
            }
        }
    }

    /// Makes `name` local to the running function; returns `false` outside
    /// a function.
    pub(crate) fn make_local(&mut self, name: &str) -> bool {
        let old = self.vars.get(name).cloned();
        let Some(frame) = self.locals.last_mut() else {
            return false;
        };
        if !frame.iter().any(|(n, _)| n == name) {
            frame.push((name.to_owned(), old));
        }
        true
    }

    pub(crate) fn dirs_mut(&mut self) -> &mut Vec<PathBuf> {
        &mut self.dirs
    }

    /// The `pushd` directory stack, most recent first (without the
    /// working directory).
    #[must_use]
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    /// Trap commands by condition (`EXIT`, `ERR`, `INT`, `TERM`, ...).
    #[must_use]
    pub fn traps(&self) -> &BTreeMap<String, String> {
        &self.traps
    }

    /// The trap command for `condition` (such as `EXIT` or `INT`).
    #[must_use]
    pub fn trap(&self, condition: &str) -> Option<&str> {
        self.traps.get(condition).map(String::as_str)
    }

    pub(crate) fn traps_mut(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.traps
    }

    pub(crate) fn umask(&self) -> Option<u32> {
        self.umask
    }

    pub(crate) fn set_umask(&mut self, mask: u32) {
        self.umask = Some(mask);
    }

    pub(crate) fn set_last_bg(&mut self, pid: Option<u32>) {
        self.last_bg = pid;
    }

    /// Takes the output logs and recorded stderr of a subshell copy, which
    /// started as a clone of this state.
    pub(crate) fn adopt_logs(&mut self, child: &mut Self) {
        self.out_log = std::mem::take(&mut child.out_log);
        self.err_log = std::mem::take(&mut child.err_log);
        self.last_stderr = std::mem::take(&mut child.last_stderr);
    }

    /// Whether output logging is on.
    #[must_use]
    pub fn log_enabled(&self) -> bool {
        self.log
    }

    /// Turns output logging on or off. While on, everything foreground
    /// commands print is also kept (last 64 KiB per stream) for
    /// [`ShellState::take_log`], and stdout and stderr of the last pipeline
    /// stage are pipes (not terminals).
    pub fn set_log(&mut self, on: bool) {
        self.log = on;
        if !on {
            self.out_log.clear();
            self.err_log.clear();
        }
    }

    /// Appends to the stdout log (only while logging is on).
    pub fn push_stdout(&mut self, bytes: &[u8]) {
        if self.log {
            Self::append_capped(&mut self.out_log, bytes);
        }
    }

    /// Returns and clears the logged `(stdout, stderr)`.
    pub fn take_log(&mut self) -> (String, String) {
        let out = String::from_utf8_lossy(&std::mem::take(&mut self.out_log)).into_owned();
        let err = String::from_utf8_lossy(&std::mem::take(&mut self.err_log)).into_owned();
        (out, err)
    }

    fn append_capped(buf: &mut Vec<u8>, bytes: &[u8]) {
        buf.extend_from_slice(bytes);
        if buf.len() > LOG_LIMIT {
            let cut = buf.len() - LOG_LIMIT;
            buf.drain(..cut);
        }
    }

    /// Whether foreground standard error is copied for `ai fix`.
    #[must_use]
    pub fn capture_stderr(&self) -> bool {
        self.capture_stderr
    }

    /// Turns stderr capture on or off. When on, a foreground command's
    /// standard error is a pipe (and not a terminal) while it still reaches
    /// the screen unchanged.
    pub fn set_capture_stderr(&mut self, on: bool) {
        self.capture_stderr = on;
    }

    /// Starts recording a new command: forgets the previous stderr.
    pub fn begin_capture(&mut self, command: &str) {
        command.clone_into(&mut self.last_command);
        self.last_stderr.clear();
    }

    /// Appends to the recorded stderr, keeping the last 16 KiB.
    pub fn push_stderr(&mut self, bytes: &[u8]) {
        if self.log {
            Self::append_capped(&mut self.err_log, bytes);
        }
        self.last_stderr.extend_from_slice(bytes);
        if self.last_stderr.len() > STDERR_LIMIT {
            let cut = self.last_stderr.len() - STDERR_LIMIT;
            self.last_stderr.drain(..cut);
        }
    }

    /// The last command line that ran.
    #[must_use]
    pub fn last_command(&self) -> &str {
        &self.last_command
    }

    /// Standard error of the last command (empty unless capture is on).
    #[must_use]
    pub fn last_stderr(&self) -> String {
        String::from_utf8_lossy(&self.last_stderr).into_owned()
    }

    /// Handle to the running foreground processes.
    #[must_use]
    pub fn foreground(&self) -> ForegroundHandle {
        self.foreground.clone()
    }

    /// Creates state from the process working directory and environment.
    ///
    /// Variables whose name or value is not valid UTF-8 are skipped.
    ///
    /// # Errors
    ///
    /// Fails if the current directory cannot be read.
    pub fn from_env() -> Result<Self> {
        let vars = std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
        Ok(Self::new(std::env::current_dir()?, vars))
    }

    /// Current working directory.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Sets the working directory.
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
    }

    /// Resolves `path` against the working directory.
    #[must_use]
    pub fn resolve(&self, path: &str) -> PathBuf {
        self.cwd.join(path)
    }

    /// Value of variable `name`.
    #[must_use]
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// All variables, sorted by name.
    #[must_use]
    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }

    /// Sets variable `name`. An exported variable stays exported; a new one
    /// is not exported.
    pub fn set_var(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.vars.insert(name.into(), value.into());
    }

    /// Sets variable `name` and exports it to child processes.
    pub fn set_env(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        self.exported.insert(name.clone());
        self.vars.insert(name, value.into());
    }

    /// Marks `name` as exported (`on`) or not exported.
    pub fn set_exported(&mut self, name: &str, on: bool) {
        if on {
            self.exported.insert(name.to_owned());
        } else {
            self.exported.remove(name);
        }
    }

    /// Whether `name` is exported.
    #[must_use]
    pub fn is_exported(&self, name: &str) -> bool {
        self.exported.contains(name)
    }

    /// The exported variables that are set: the environment of a child process.
    pub fn env_vars(&self) -> impl Iterator<Item = (&String, &String)> {
        self.vars
            .iter()
            .filter(|(k, _)| self.exported.contains(k.as_str()))
    }

    /// Removes variable `name` and its export mark.
    pub fn unset_var(&mut self, name: &str) {
        self.vars.remove(name);
        self.exported.remove(name);
    }

    /// Resolves a parameter, including the specials `?`, `$`, `#`, `@`,
    /// `*`, `!`, `-`, `0`, the positional parameters, and `RANDOM`.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<String> {
        match name {
            "?" => Some(self.last_status.to_string()),
            "$" => Some(std::process::id().to_string()),
            "#" => Some(self.positional.len().to_string()),
            "@" | "*" => Some(self.positional.join(" ")),
            "!" => self.last_bg.map(|p| p.to_string()),
            "-" => Some(self.options.flags()),
            "0" => Some(self.arg0.clone()),
            "RANDOM" if !self.vars.contains_key("RANDOM") => Some(random().to_string()),
            _ if name.bytes().all(|b| b.is_ascii_digit()) && !name.is_empty() => name
                .parse::<usize>()
                .ok()
                .and_then(|i| self.positional.get(i.checked_sub(1)?))
                .cloned(),
            _ => self.vars.get(name).cloned(),
        }
    }

    /// Home directory from `HOME`.
    #[must_use]
    pub fn home(&self) -> Option<String> {
        self.var("HOME").map(str::to_owned)
    }

    /// All aliases, sorted by name.
    #[must_use]
    pub fn aliases(&self) -> &BTreeMap<String, String> {
        &self.aliases
    }

    /// Defines or replaces an alias.
    pub fn set_alias(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.aliases.insert(name.into(), value.into());
    }

    /// Removes an alias, returning whether it existed.
    pub fn remove_alias(&mut self, name: &str) -> bool {
        self.aliases.remove(name).is_some()
    }

    /// Removes all aliases.
    pub fn clear_aliases(&mut self) {
        self.aliases.clear();
    }

    /// Exit status of the last command (`$?`).
    #[must_use]
    pub fn last_status(&self) -> i32 {
        self.last_status
    }

    /// Sets the exit status of the last command.
    pub fn set_last_status(&mut self, status: i32) {
        self.last_status = status;
    }

    /// History entries, oldest first.
    #[must_use]
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Appends a history entry, skipping empty lines and consecutive duplicates.
    pub fn push_history(&mut self, line: &str) {
        if line.is_empty() || self.history.last().is_some_and(|l| l == line) {
            return;
        }
        if self.history.len() >= HISTORY_LIMIT {
            self.history.remove(0);
        }
        self.history.push(line.to_owned());
    }

    /// Removes all history entries.
    pub fn clear_history(&mut self) {
        self.history.clear();
    }
}

/// A number in `0..32768` from the clock, for `$RANDOM`.
fn random() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let mut x = nanos ^ std::process::id().rotate_left(16) ^ 0x9e37_79b9;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    x % 32768
}
