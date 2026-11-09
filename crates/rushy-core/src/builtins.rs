//! Built-in commands executed inside the shell process.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::jobs::{JobInfo, JobState, JobTable};
use crate::parser::is_var_name;
use crate::state::{SET_OPTIONS, SHOPT_OPTIONS, ShellState};
use crate::test::{self, Tok};

/// Built-in command names with one-line usage descriptions.
pub const BUILTINS: &[(&str, &str)] = &[
    (".", "run a file in this shell: . file [args]"),
    (":", "do nothing and succeed"),
    ("[", "test an expression: [ expr ]"),
    ("alias", "define or list aliases: alias [name[=value] ...]"),
    ("bg", "resume a job in the background: bg [%job]"),
    ("break", "leave a loop: break [n]"),
    (
        "builtin",
        "run a built-in, not a function: builtin name [args]",
    ),
    ("cd", "change directory: cd [dir|-]"),
    (
        "command",
        "run a command, not a function: command [-v|-V] name [args]",
    ),
    ("continue", "start the next loop pass: continue [n]"),
    ("dirs", "show the directory stack: dirs [-c|-p|-v]"),
    ("disown", "forget a job: disown [-a] [%job ...]"),
    ("echo", "print arguments: echo [-neE] [arg ...]"),
    ("eval", "run arguments as a command: eval [arg ...]"),
    (
        "exec",
        "replace the shell with a command: exec command [args]",
    ),
    ("exit", "exit the shell: exit [status]"),
    ("export", "set or list variables: export [NAME[=value] ...]"),
    ("false", "do nothing and fail"),
    ("fg", "resume a job and wait for it: fg [%job]"),
    ("help", "list built-in commands: help [name ...]"),
    ("history", "list or clear history: history [-c] [count]"),
    ("jobs", "list background jobs"),
    (
        "kill",
        "signal jobs or processes: kill [-SIGNAL] %job|pid ...",
    ),
    ("local", "make function variables: local NAME[=value] ..."),
    ("popd", "leave a pushed directory: popd [+N]"),
    ("printf", "print formatted text: printf format [args]"),
    ("pushd", "push a directory and change to it: pushd [dir|+N]"),
    ("pwd", "print the working directory"),
    (
        "read",
        "read a line into variables: read [-r] [-p prompt] [-d delim] [name ...]",
    ),
    ("return", "return from a function: return [status]"),
    (
        "set",
        "set options or positional parameters: set [-euxfCn] [-o name] [--] [args]",
    ),
    ("shift", "shift positional parameters: shift [n]"),
    ("shopt", "set shell options: shopt [-s|-u|-q|-p] [name ...]"),
    ("source", "run a file in this shell: source file [args]"),
    ("test", "test an expression: test expr"),
    (
        "trap",
        "run a command on a signal or exit: trap [-lp] [command] [signal ...]",
    ),
    ("true", "do nothing and succeed"),
    ("type", "describe commands: type [-afptP] name ..."),
    ("ulimit", "show resource limits: ulimit [-a|-n|...]"),
    ("umask", "show or set the file mode mask: umask [-S] [mode]"),
    ("unalias", "remove aliases: unalias [-a] name ..."),
    (
        "unset",
        "remove variables or functions: unset [-fv] NAME ...",
    ),
    ("wait", "wait for jobs: wait [%job|pid ...]"),
    ("which", "locate commands: which [-a] name ..."),
];

/// Reserved words, for `type`.
pub const KEYWORDS: &[&str] = &[
    "!", "[[", "]]", "{", "}", "case", "do", "done", "elif", "else", "esac", "fi", "for",
    "function", "if", "in", "then", "until", "while",
];

const SIGNALS: &[(u32, &str)] = &[
    (1, "HUP"),
    (2, "INT"),
    (3, "QUIT"),
    (4, "ILL"),
    (5, "TRAP"),
    (6, "ABRT"),
    (8, "FPE"),
    (9, "KILL"),
    (10, "BUS"),
    (11, "SEGV"),
    (12, "SYS"),
    (13, "PIPE"),
    (14, "ALRM"),
    (15, "TERM"),
    (16, "URG"),
    (17, "STOP"),
    (18, "TSTP"),
    (19, "CONT"),
    (20, "CHLD"),
    (21, "TTIN"),
    (22, "TTOU"),
    (28, "WINCH"),
    (30, "USR1"),
    (31, "USR2"),
];

/// Whether `name` is a built-in command.
#[must_use]
pub fn is_builtin(name: &str) -> bool {
    BUILTINS.iter().any(|(n, _)| *n == name)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) status: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) exit: Option<i32>,
}

impl Output {
    pub(crate) fn out(stdout: String) -> Self {
        Self {
            stdout,
            ..Self::default()
        }
    }

    pub(crate) fn status(status: i32) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }

    pub(crate) fn fail(status: i32, msg: impl Into<String>) -> Self {
        let mut res = Self::default();
        res.error(status, msg);
        res
    }

    pub(crate) fn error(&mut self, status: i32, msg: impl Into<String>) {
        self.status = status;
        self.stderr.push_str(&msg.into());
        self.stderr.push('\n');
    }
}

pub(crate) async fn run(
    name: &str,
    args: &[String],
    state: &mut ShellState,
    jobs: &mut JobTable,
) -> Output {
    match name {
        ":" | "true" => Output::default(),
        "false" => Output::status(1),
        "alias" => alias(args, state),
        "bg" => bg(args, jobs).await,
        "cd" => cd(args, state).await,
        "dirs" => dirs(args, state),
        "disown" => disown(args, jobs),
        "echo" => echo(args),
        "exit" => exit(args, state),
        "export" => export(args, state),
        "fg" => fg(args, &state.foreground(), jobs).await,
        "help" => help(args),
        "history" => history(args, state),
        "jobs" => list_jobs(jobs),
        "kill" => kill(args, state, jobs).await,
        "popd" => popd(args, state).await,
        "printf" => printf(args),
        "pushd" => pushd(args, state).await,
        "pwd" => Output::out(format!("{}\n", state.cwd().display())),
        "set" => set(args, state),
        "shift" => shift(args, state),
        "shopt" => shopt(args, state),
        "test" => test_cmd(args, state),
        "[" => match args.split_last() {
            Some((last, rest)) if last == "]" => test_cmd(rest, state),
            _ => Output::fail(2, "[: missing `]'"),
        },
        "trap" => trap(args, state),
        "type" => type_cmd(args, state),
        "ulimit" => ulimit(args, state).await,
        "umask" => umask(args, state).await,
        "unalias" => unalias(args, state),
        "unset" => unset(args, state),
        "wait" => wait(args, jobs).await,
        "which" => which(args, state),
        _ => Output::fail(127, format!("xsh: {name}: not a built-in")),
    }
}

pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Quotes `value` only when the shell would read it differently.
pub(crate) fn quote_if_needed(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%^".contains(c))
    {
        value.to_owned()
    } else {
        quote(value)
    }
}

/// Changes to `target`; returns the new directory.
async fn change_dir(target: &str, state: &mut ShellState) -> Result<PathBuf, String> {
    let dir = match tokio::fs::canonicalize(state.resolve(target)).await {
        Ok(d) => d,
        Err(e) => return Err(format!("cd: {target}: {e}")),
    };
    match tokio::fs::metadata(&dir).await {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("cd: {target}: not a directory")),
        Err(e) => return Err(format!("cd: {target}: {e}")),
    }
    let old = state.cwd().display().to_string();
    state.set_env("OLDPWD", old);
    state.set_env("PWD", dir.display().to_string());
    state.set_cwd(dir.clone());
    Ok(dir)
}

fn via_cdpath(target: &str, state: &ShellState) -> Option<String> {
    if Path::new(target).is_absolute()
        || target.starts_with("./")
        || target.starts_with("../")
        || target == "."
        || target == ".."
    {
        return None;
    }
    state.cdpath().into_iter().find_map(|base| {
        let base = if base.is_empty() {
            ".".to_owned()
        } else {
            base
        };
        let cand = state.resolve(&base).join(target);
        cand.is_dir().then(|| cand.display().to_string())
    })
}

async fn cd(args: &[String], state: &mut ShellState) -> Output {
    let args: Vec<&String> = args.iter().filter(|a| *a != "-L" && *a != "-P").collect();
    if args.len() > 1 {
        return Output::fail(1, "cd: too many arguments");
    }
    let dash = args.first().is_some_and(|a| *a == "-");
    let target = match args.first() {
        Some(_) if dash => state.var("OLDPWD").map(str::to_owned),
        Some(a) => Some((*a).clone()),
        None => state.home(),
    };
    let Some(target) = target else {
        return Output::fail(
            1,
            if dash {
                "cd: OLDPWD not set"
            } else {
                "cd: HOME not set"
            },
        );
    };
    let found = if state.resolve(&target).is_dir() {
        None
    } else {
        via_cdpath(&target, state)
    };
    let print = dash || found.is_some();
    match change_dir(found.as_deref().unwrap_or(&target), state).await {
        Ok(dir) => Output::out(if print {
            format!("{}\n", dir.display())
        } else {
            String::new()
        }),
        Err(e) => Output::fail(1, e),
    }
}

fn tilde_path(path: &Path, state: &ShellState) -> String {
    match state.home().filter(|h| !h.is_empty()) {
        Some(h) => match path.strip_prefix(&h) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}

fn stack(state: &ShellState) -> Vec<PathBuf> {
    std::iter::once(state.cwd().to_path_buf())
        .chain(state.dirs().iter().cloned())
        .collect()
}

fn show_dirs(state: &ShellState) -> String {
    let parts: Vec<String> = stack(state).iter().map(|p| tilde_path(p, state)).collect();
    format!("{}\n", parts.join(" "))
}

fn dirs(args: &[String], state: &mut ShellState) -> Output {
    match args.first().map(String::as_str) {
        None => Output::out(show_dirs(state)),
        Some("-c") => {
            state.dirs_mut().clear();
            Output::default()
        }
        Some("-p") => Output::out(stack(state).iter().fold(String::new(), |mut s, p| {
            let _ = writeln!(s, "{}", tilde_path(p, state));
            s
        })),
        Some("-v") => Output::out(stack(state).iter().enumerate().fold(
            String::new(),
            |mut s, (i, p)| {
                let _ = writeln!(s, "{i:>2}  {}", tilde_path(p, state));
                s
            },
        )),
        Some(a) => Output::fail(2, format!("dirs: {a}: invalid option")),
    }
}

fn stack_index(arg: &str, len: usize) -> Option<usize> {
    let n: usize = arg.strip_prefix('+')?.parse().ok()?;
    (n < len).then_some(n)
}

async fn pushd(args: &[String], state: &mut ShellState) -> Output {
    let mut all = stack(state);
    let target = match args.first() {
        None => {
            if all.len() < 2 {
                return Output::fail(1, "pushd: no other directory");
            }
            all.swap(0, 1);
            None
        }
        Some(a) if a.starts_with('+') => {
            let Some(n) = stack_index(a, all.len()) else {
                return Output::fail(1, format!("pushd: {a}: directory stack index out of range"));
            };
            all.rotate_left(n);
            None
        }
        Some(a) => Some(a.clone()),
    };
    let old = state.cwd().to_path_buf();
    let dest = match &target {
        Some(t) => t.clone(),
        None => all
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
    };
    if let Err(e) = change_dir(&dest, state).await {
        return Output::fail(1, e.replacen("cd:", "pushd:", 1));
    }
    let rest: Vec<PathBuf> = if target.is_some() {
        std::iter::once(old)
            .chain(state.dirs().iter().cloned())
            .collect()
    } else {
        all.into_iter().skip(1).collect()
    };
    *state.dirs_mut() = rest;
    Output::out(show_dirs(state))
}

async fn popd(args: &[String], state: &mut ShellState) -> Output {
    if state.dirs().is_empty() {
        return Output::fail(1, "popd: directory stack empty");
    }
    match args.first() {
        Some(a) if a.starts_with('+') => {
            let len = state.dirs().len() + 1;
            match stack_index(a, len) {
                Some(0) | None => {
                    Output::fail(1, format!("popd: {a}: directory stack index out of range"))
                }
                Some(n) => {
                    state.dirs_mut().remove(n - 1);
                    Output::out(show_dirs(state))
                }
            }
        }
        Some(a) => Output::fail(2, format!("popd: {a}: invalid argument")),
        None => {
            let next = state.dirs_mut().remove(0);
            match change_dir(&next.display().to_string(), state).await {
                Ok(_) => Output::out(show_dirs(state)),
                Err(e) => Output::fail(1, e.replacen("cd:", "popd:", 1)),
            }
        }
    }
}

fn exit(args: &[String], state: &ShellState) -> Output {
    let code = match args.first().map(|a| (a, a.parse::<i32>())) {
        None => state.last_status(),
        Some((_, Ok(c))) => c & 0xff,
        Some((a, Err(_))) => {
            let mut res = Output::fail(2, format!("exit: {a}: numeric argument required"));
            res.exit = Some(2);
            return res;
        }
    };
    Output {
        status: code,
        exit: Some(code),
        ..Output::default()
    }
}

/// `export [-n] [-p] [NAME[=value] ...]`. `-n` removes the export mark.
fn export(args: &[String], state: &mut ShellState) -> Output {
    let unexport = args.iter().any(|a| a == "-n");
    let names: Vec<&String> = args.iter().filter(|a| *a != "-n" && *a != "-p").collect();
    if names.is_empty() {
        let mut out = String::new();
        for (k, v) in state.env_vars() {
            let _ = writeln!(out, "export {k}={}", quote(v));
        }
        return Output::out(out);
    }
    let mut res = Output::default();
    for a in names {
        let (name, value) = a
            .split_once('=')
            .map_or((a.as_str(), None), |(n, v)| (n, Some(v)));
        if !is_var_name(name) {
            res.error(1, format!("export: `{a}`: not a valid identifier"));
            continue;
        }
        if let Some(v) = value {
            state.set_var(name, v);
        }
        state.set_exported(name, !unexport);
    }
    res
}

fn unset(args: &[String], state: &mut ShellState) -> Output {
    let mut res = Output::default();
    let mut funcs = false;
    for a in args {
        match a.as_str() {
            "-f" => funcs = true,
            "-v" => funcs = false,
            _ if funcs => {
                state.remove_function(a);
            }
            _ if is_var_name(a) => {
                if state.var(a).is_none() {
                    state.remove_function(a);
                }
                state.unset_var(a);
            }
            _ => res.error(1, format!("unset: `{a}`: not a valid identifier")),
        }
    }
    res
}

fn valid_alias_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_whitespace() || "=/|&;<>'\"\\$`".contains(c))
}

fn alias(args: &[String], state: &mut ShellState) -> Output {
    if args.is_empty() || args.iter().all(|a| a == "-p") {
        let mut out = String::new();
        for (k, v) in state.aliases() {
            let _ = writeln!(out, "alias {k}={}", quote(v));
        }
        return Output::out(out);
    }
    let mut res = Output::default();
    for a in args {
        match a.split_once('=') {
            Some((name, value)) if valid_alias_name(name) => state.set_alias(name, value),
            Some((name, _)) => res.error(1, format!("alias: `{name}`: invalid alias name")),
            None => match state.aliases().get(a.as_str()) {
                Some(v) => {
                    let _ = writeln!(res.stdout, "alias {a}={}", quote(v));
                }
                None => res.error(1, format!("alias: {a}: not found")),
            },
        }
    }
    res
}

fn unalias(args: &[String], state: &mut ShellState) -> Output {
    if args.is_empty() {
        return Output::fail(2, "unalias: usage: unalias [-a] name ...");
    }
    let mut res = Output::default();
    for a in args {
        if a == "-a" {
            state.clear_aliases();
        } else if !state.remove_alias(a) {
            res.error(1, format!("unalias: {a}: not found"));
        }
    }
    res
}

fn history(args: &[String], state: &mut ShellState) -> Output {
    let count = match args.first().map(String::as_str) {
        Some("-c") => {
            state.clear_history();
            return Output::default();
        }
        Some(n) => match n.parse::<usize>() {
            Ok(n) => n,
            Err(_) => return Output::fail(2, format!("history: {n}: numeric argument required")),
        },
        None => usize::MAX,
    };
    let entries = state.history();
    let mut out = String::new();
    for (i, cmd) in entries
        .iter()
        .enumerate()
        .skip(entries.len().saturating_sub(count))
    {
        let _ = writeln!(out, "{:>5}  {cmd}", i + 1);
    }
    Output::out(out)
}

fn render_job(out: &mut String, job: &JobInfo) {
    let state = match job.state {
        JobState::Running => "Running".to_owned(),
        JobState::Stopped => "Stopped".to_owned(),
        JobState::Done(0) => "Done".to_owned(),
        JobState::Done(c) => format!("Exit {c}"),
    };
    let _ = writeln!(out, "[{}]  {state:<10}{}", job.id, job.command);
}

fn list_jobs(jobs: &mut JobTable) -> Output {
    let mut all = jobs.reap();
    all.extend(jobs.list());
    all.sort_by_key(|j| j.id);
    let mut out = String::new();
    for job in &all {
        render_job(&mut out, job);
    }
    Output::out(out)
}

/// Sends `SIGCONT` to the processes of a job.
async fn resume(jobs: &JobTable, id: usize) {
    let pids: Vec<String> = jobs.pids(id).iter().map(u32::to_string).collect();
    if !pids.is_empty() {
        let _ = tokio::process::Command::new("kill")
            .arg("-CONT")
            .args(&pids)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
}

async fn bg(args: &[String], jobs: &mut JobTable) -> Output {
    let Some(id) = jobs.resolve(args.first().map(String::as_str)) else {
        return Output::fail(1, "bg: no such job");
    };
    resume(jobs, id).await;
    jobs.set_running(id);
    Output::out(format!(
        "[{id}] {} &\n",
        jobs.command(id).unwrap_or_default()
    ))
}

/// Resumes a job and waits for it. The command is printed first, as in
/// bash, because the job may run for a long time.
async fn fg(args: &[String], fgh: &crate::ForegroundHandle, jobs: &mut JobTable) -> Output {
    let Some(id) = jobs.resolve(args.first().map(String::as_str)) else {
        return Output::fail(1, "fg: no such job");
    };
    let cmd = jobs.command(id).unwrap_or_default();
    {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{cmd}");
        let _ = out.flush();
    }
    resume(jobs, id).await;
    match jobs.wait(id, fgh).await {
        Ok(Some((_, 148))) => Output::fail(148, format!("\n[{id}]+  Stopped    {cmd}")),
        Ok(Some((_, status))) => Output::status(status),
        Ok(None) => Output::fail(1, "fg: no such job"),
        Err(e) => Output::fail(1, format!("fg: {e}")),
    }
}

fn disown(args: &[String], jobs: &mut JobTable) -> Output {
    if args.iter().any(|a| a == "-a") {
        for id in jobs.ids() {
            jobs.remove(id);
        }
        return Output::default();
    }
    let specs: Vec<Option<&str>> = if args.is_empty() {
        vec![None]
    } else {
        args.iter().map(|a| Some(a.as_str())).collect()
    };
    let mut res = Output::default();
    for spec in specs {
        match jobs.resolve(spec) {
            Some(id) => {
                jobs.remove(id);
            }
            None => res.error(
                1,
                format!("disown: {}: no such job", spec.unwrap_or("current")),
            ),
        }
    }
    res
}

/// `wait` uses its own handle: background jobs run in their own process
/// groups, so `Ctrl-Z` does not stop them.
async fn wait(args: &[String], jobs: &mut JobTable) -> Output {
    let none = crate::ForegroundHandle::default();
    if args.is_empty() {
        for id in jobs.ids() {
            let _ = jobs.wait(id, &none).await;
        }
        return Output::default();
    }
    let mut res = Output::default();
    for a in args {
        let id = if a.starts_with('%') {
            jobs.resolve(Some(a))
        } else {
            a.parse::<u32>().ok().and_then(|pid| jobs.by_pid(pid))
        };
        let Some(id) = id else {
            res.error(127, format!("wait: {a}: no such job"));
            continue;
        };
        match jobs.wait(id, &none).await {
            Ok(Some((_, status))) => res.status = status,
            Ok(None) => res.error(127, format!("wait: {a}: no such job")),
            Err(e) => res.error(1, format!("wait: {e}")),
        }
    }
    res
}

async fn system(program: &str, args: &[String], state: &ShellState) -> Output {
    let res = tokio::process::Command::new(program)
        .args(args)
        .env_clear()
        .envs(state.env_vars())
        .current_dir(state.cwd())
        .output()
        .await;
    match res {
        Ok(o) => Output {
            status: o.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            exit: None,
        },
        Err(e) => Output::fail(1, format!("{program}: {e}")),
    }
}

async fn kill(args: &[String], state: &ShellState, jobs: &mut JobTable) -> Output {
    if args.is_empty() {
        return Output::fail(2, "kill: usage: kill [-SIGNAL] %job|pid ...");
    }
    let mut targets = Vec::with_capacity(args.len());
    let mut aborted = false;
    for a in args {
        if a.starts_with('%') {
            let Some(id) = jobs.resolve(Some(a)) else {
                return Output::fail(1, format!("kill: {a}: no such job"));
            };
            let pids = jobs.pids(id);
            if pids.is_empty() {
                if jobs.abort_tasks(id) {
                    aborted = true;
                    continue;
                }
                return Output::fail(1, format!("kill: {a}: no such job"));
            }
            targets.extend(pids.iter().map(u32::to_string));
        } else {
            targets.push(a.clone());
        }
    }
    if aborted && targets.iter().all(|t| t.starts_with('-')) {
        return Output::default();
    }
    system("kill", &targets, state).await
}

fn help(args: &[String]) -> Output {
    if !args.is_empty() {
        let mut res = Output::default();
        for a in args {
            match BUILTINS.iter().find(|(n, _)| n == a) {
                Some((name, desc)) => {
                    let _ = writeln!(res.stdout, "{name}: {desc}");
                }
                None => res.error(1, format!("help: no help topics match `{a}`")),
            }
        }
        return res;
    }
    let mut out = String::from("xsh built-in commands:\n");
    for (name, desc) in BUILTINS {
        let _ = writeln!(out, "  {name:<10}{desc}");
    }
    Output::out(out)
}

/// Decodes backslash escapes. In `echo` mode octal needs a leading `0`.
/// Returns the text and whether `\c` asked to stop output.
pub(crate) fn unescape(s: &str, echo: bool) -> (String, bool) {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let num = |i: &mut usize, radix: u32, max: usize| -> Option<u32> {
        let start = *i;
        while *i - start < max && chars.get(*i).is_some_and(|c| c.is_digit(radix)) {
            *i += 1;
        }
        let text: String = chars.get(start..*i)?.iter().collect();
        u32::from_str_radix(&text, radix).ok()
    };
    while let Some(&c) = chars.get(i) {
        i += 1;
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(&e) = chars.get(i) else {
            out.push('\\');
            break;
        };
        i += 1;
        let ch = match e {
            'a' => '\x07',
            'b' => '\x08',
            'e' | 'E' => '\x1b',
            'f' => '\x0c',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'v' => '\x0b',
            '\\' => '\\',
            '\'' if !echo => '\'',
            '"' if !echo => '"',
            '?' if !echo => '?',
            'c' => return (out, true),
            '0' if echo => num(&mut i, 8, 3).and_then(char::from_u32).unwrap_or('\0'),
            '0'..='7' if !echo => {
                i -= 1;
                num(&mut i, 8, 3).and_then(char::from_u32).unwrap_or('\0')
            }
            'x' => {
                if let Some(c) = num(&mut i, 16, 2).and_then(char::from_u32) {
                    c
                } else {
                    out.push_str("\\x");
                    continue;
                }
            }
            'u' | 'U' => {
                let max = if e == 'u' { 4 } else { 8 };
                if let Some(c) = num(&mut i, 16, max).and_then(char::from_u32) {
                    c
                } else {
                    out.push('\\');
                    out.push(e);
                    continue;
                }
            }
            _ => {
                out.push('\\');
                e
            }
        };
        out.push(ch);
    }
    (out, false)
}

fn echo(args: &[String]) -> Output {
    let (mut newline, mut escapes) = (true, false);
    let mut i = 0;
    while let Some(a) = args.get(i) {
        let Some(flags) = a.strip_prefix('-') else {
            break;
        };
        if flags.is_empty() || !flags.chars().all(|c| "neE".contains(c)) {
            break;
        }
        for c in flags.chars() {
            match c {
                'n' => newline = false,
                'e' => escapes = true,
                _ => escapes = false,
            }
        }
        i += 1;
    }
    let text = args.get(i..).unwrap_or_default().join(" ");
    if escapes {
        let (mut s, stop) = unescape(&text, true);
        if !stop && newline {
            s.push('\n');
        }
        return Output::out(s);
    }
    Output::out(if newline { format!("{text}\n") } else { text })
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per printf modifier"
)]
struct Spec {
    left: bool,
    zero: bool,
    plus: bool,
    space: bool,
    alt: bool,
    width: Option<usize>,
    prec: Option<usize>,
}

fn pad(s: String, spec: &Spec, numeric: bool) -> String {
    let w = spec.width.unwrap_or(0);
    let len = s.chars().count();
    if len >= w {
        return s;
    }
    let fill = w - len;
    if spec.left {
        format!("{s}{}", " ".repeat(fill))
    } else if spec.zero && numeric {
        let (sign, rest) = match s.chars().next() {
            Some(c @ ('-' | '+' | ' ')) => (c.to_string(), s[1..].to_owned()),
            _ => (String::new(), s),
        };
        format!("{sign}{}{rest}", "0".repeat(fill))
    } else {
        format!("{}{s}", " ".repeat(fill))
    }
}

fn int_arg(a: &str) -> Result<i64, String> {
    let t = a.trim();
    if let Some(c) = t.strip_prefix('\'').or_else(|| t.strip_prefix('"')) {
        return Ok(c.chars().next().map_or(0, |c| i64::from(u32::from(c))));
    }
    if t.is_empty() {
        return Ok(0);
    }
    let (neg, body) = t.strip_prefix('-').map_or((false, t), |b| (true, b));
    let body = body.strip_prefix('+').unwrap_or(body);
    let v = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i64::from_str_radix(h, 16)
    } else if body.len() > 1 && body.starts_with('0') {
        i64::from_str_radix(&body[1..], 8)
    } else {
        body.parse()
    }
    .map_err(|_| format!("printf: {a}: invalid number"))?;
    Ok(if neg { -v } else { v })
}

fn exp_format(v: f64, prec: usize, upper: bool) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let s = format!("{v:.prec$e}");
    let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let sign = if exp < 0 { '-' } else { '+' };
    let out = format!("{mant}e{sign}{:02}", exp.abs());
    if upper { out.to_uppercase() } else { out }
}

#[allow(
    clippy::many_single_char_names,
    reason = "names follow the printf %g spec"
)]
fn general_format(v: f64, prec: usize, alt: bool, upper: bool) -> String {
    let p = if prec == 0 { 1 } else { prec };
    if v == 0.0 {
        return "0".into();
    }
    let s = format!("{:.*e}", p - 1, v);
    let exp: i32 = s
        .split_once('e')
        .and_then(|(_, e)| e.parse().ok())
        .unwrap_or(0);
    let p_i = i32::try_from(p).unwrap_or(i32::MAX);
    let mut out = if exp < -4 || exp >= p_i {
        exp_format(v, p - 1, upper)
    } else {
        let decimals = usize::try_from(p_i - 1 - exp).unwrap_or(0);
        format!("{v:.decimals$}")
    };
    if !alt {
        if let Some(epos) = out.find(['e', 'E']) {
            let (m, e) = out.split_at(epos);
            let m = if m.contains('.') {
                m.trim_end_matches('0').trim_end_matches('.')
            } else {
                m
            };
            out = format!("{m}{e}");
        } else if out.contains('.') {
            out = out.trim_end_matches('0').trim_end_matches('.').to_owned();
        }
    }
    out
}

#[allow(clippy::too_many_lines)]
fn printf(args: &[String]) -> Output {
    let Some((fmt, rest)) = args.split_first() else {
        return Output::fail(2, "printf: usage: printf format [arguments]");
    };
    let fmt: Vec<char> = fmt.chars().collect();
    let mut res = Output::default();
    let mut ai = 0;
    loop {
        let start_ai = ai;
        let mut used_conv = false;
        let mut i = 0;
        while let Some(&c) = fmt.get(i) {
            i += 1;
            if c == '\\' {
                let mut esc = String::from("\\");
                if let Some(&n) = fmt.get(i) {
                    esc.push(n);
                    i += 1;
                    let more = match n {
                        '0'..='7' | 'x' => 2,
                        'u' => 4,
                        'U' => 8,
                        _ => 0,
                    };
                    for _ in 0..more {
                        match fmt.get(i) {
                            Some(&d) if d.is_ascii_hexdigit() => {
                                esc.push(d);
                                i += 1;
                            }
                            _ => break,
                        }
                    }
                }
                let (s, stop) = unescape(&esc, false);
                res.stdout.push_str(&s);
                if stop {
                    return res;
                }
                continue;
            }
            if c != '%' {
                res.stdout.push(c);
                continue;
            }
            if fmt.get(i) == Some(&'%') {
                res.stdout.push('%');
                i += 1;
                continue;
            }
            let mut spec = Spec {
                left: false,
                zero: false,
                plus: false,
                space: false,
                alt: false,
                width: None,
                prec: None,
            };
            while let Some(&f) = fmt.get(i) {
                match f {
                    '-' => spec.left = true,
                    '0' => spec.zero = true,
                    '+' => spec.plus = true,
                    ' ' => spec.space = true,
                    '#' => spec.alt = true,
                    _ => break,
                }
                i += 1;
            }
            let next_arg = |ai: &mut usize| -> String {
                let a = rest.get(*ai).cloned().unwrap_or_default();
                *ai += 1;
                a
            };
            if fmt.get(i) == Some(&'*') {
                i += 1;
                let w = int_arg(&next_arg(&mut ai)).unwrap_or(0);
                if w < 0 {
                    spec.left = true;
                }
                spec.width = usize::try_from(w.unsigned_abs()).ok();
            } else {
                let mut w = String::new();
                while let Some(&d) = fmt.get(i).filter(|d| d.is_ascii_digit()) {
                    w.push(d);
                    i += 1;
                }
                spec.width = w.parse().ok();
            }
            if fmt.get(i) == Some(&'.') {
                i += 1;
                if fmt.get(i) == Some(&'*') {
                    i += 1;
                    spec.prec = usize::try_from(int_arg(&next_arg(&mut ai)).unwrap_or(0)).ok();
                } else {
                    let mut p = String::new();
                    while let Some(&d) = fmt.get(i).filter(|d| d.is_ascii_digit()) {
                        p.push(d);
                        i += 1;
                    }
                    spec.prec = Some(p.parse().unwrap_or(0));
                }
            }
            while matches!(fmt.get(i), Some('l' | 'h' | 'q' | 'j' | 'z' | 't' | 'L'))
                && fmt.get(i) != Some(&'q')
            {
                i += 1;
            }
            let Some(&conv) = fmt.get(i) else {
                res.error(1, "printf: missing format character");
                return res;
            };
            i += 1;
            used_conv = true;
            let arg = next_arg(&mut ai);
            let sign = |v: String, neg: bool| {
                if neg {
                    v
                } else if spec.plus {
                    format!("+{v}")
                } else if spec.space {
                    format!(" {v}")
                } else {
                    v
                }
            };
            let text = match conv {
                's' => {
                    let s = match spec.prec {
                        Some(p) => arg.chars().take(p).collect(),
                        None => arg,
                    };
                    pad(s, &spec, false)
                }
                'b' => {
                    let (s, stop) = unescape(&arg, true);
                    res.stdout.push_str(&pad(s, &spec, false));
                    if stop {
                        return res;
                    }
                    continue;
                }
                'q' => pad(quote_if_needed(&arg), &spec, false),
                'c' => pad(
                    arg.chars().next().map(String::from).unwrap_or_default(),
                    &spec,
                    false,
                ),
                'd' | 'i' | 'u' | 'o' | 'x' | 'X' => {
                    let v = match int_arg(&arg) {
                        Ok(v) => v,
                        Err(e) => {
                            res.error(1, e);
                            0
                        }
                    };
                    let mut digits = match conv {
                        'o' => format!("{v:o}"),
                        'x' => format!("{v:x}"),
                        'X' => format!("{v:X}"),
                        'u' if v < 0 => u64::from_ne_bytes(v.to_ne_bytes()).to_string(),
                        _ => v.unsigned_abs().to_string(),
                    };
                    if let Some(p) = spec.prec {
                        if digits.len() < p {
                            digits = format!("{}{digits}", "0".repeat(p - digits.len()));
                        }
                    }
                    if spec.alt && conv == 'o' && !digits.starts_with('0') {
                        digits.insert(0, '0');
                    }
                    if spec.alt && v != 0 && (conv == 'x' || conv == 'X') {
                        digits.insert_str(0, if conv == 'x' { "0x" } else { "0X" });
                    }
                    let neg = v < 0 && matches!(conv, 'd' | 'i');
                    let s = if neg {
                        format!("-{digits}")
                    } else {
                        sign(digits, false)
                    };
                    pad(s, &spec, spec.prec.is_none())
                }
                'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                    let v: f64 = if arg.trim().is_empty() {
                        0.0
                    } else if let Ok(v) = arg.trim().parse() {
                        v
                    } else {
                        res.error(1, format!("printf: {arg}: invalid number"));
                        0.0
                    };
                    let p = spec.prec.unwrap_or(6);
                    let s = match conv {
                        'f' | 'F' => format!("{v:.p$}"),
                        'e' | 'E' => exp_format(v, p, conv == 'E'),
                        _ => general_format(v, p, spec.alt, conv == 'G'),
                    };
                    let neg = s.starts_with('-');
                    pad(sign(s, neg), &spec, true)
                }
                other => {
                    res.error(1, format!("printf: `{other}': invalid format character"));
                    return res;
                }
            };
            res.stdout.push_str(&text);
        }
        if !used_conv || ai >= rest.len() || ai == start_ai {
            break;
        }
    }
    res
}

fn set(args: &[String], state: &mut ShellState) -> Output {
    if args.is_empty() {
        let mut out = String::new();
        for (k, v) in state.vars() {
            let _ = writeln!(out, "{k}={}", quote_if_needed(v));
        }
        return Output::out(out);
    }
    let mut res = Output::default();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        if a == "--" {
            state.replace_positional(args.get(i + 1..).unwrap_or_default().to_vec());
            return res;
        }
        if a == "-" {
            state.options_mut().xtrace = false;
            state.replace_positional(args.get(i + 1..).unwrap_or_default().to_vec());
            return res;
        }
        let on = a.starts_with('-');
        if !(on || a.starts_with('+')) || a.len() < 2 {
            state.replace_positional(args.get(i..).unwrap_or_default().to_vec());
            return res;
        }
        if a == "-o" || a == "+o" {
            if let Some(name) = args.get(i + 1) {
                if !SET_OPTIONS.contains(&name.as_str()) || !state.set_option(name, on) {
                    res.error(2, format!("set: {name}: invalid option name"));
                }
                i += 2;
            } else {
                for name in SET_OPTIONS {
                    let v = state.options().get(name).unwrap_or(false);
                    let _ = if on {
                        writeln!(res.stdout, "{name:<15}{}", if v { "on" } else { "off" })
                    } else {
                        writeln!(res.stdout, "set {}o {name}", if v { '-' } else { '+' })
                    };
                }
                i += 1;
            }
            continue;
        }
        for c in a.chars().skip(1) {
            let name = match c {
                'e' => "errexit",
                'x' => "xtrace",
                'u' => "nounset",
                'f' => "noglob",
                'C' => "noclobber",
                'n' => "noexec",
                _ => {
                    res.error(2, format!("set: -{c}: invalid option"));
                    continue;
                }
            };
            state.set_option(name, on);
        }
        i += 1;
    }
    res
}

fn shift(args: &[String], state: &mut ShellState) -> Output {
    let n = match args.first().map(|a| a.parse::<usize>()) {
        None => 1,
        Some(Ok(n)) => n,
        Some(Err(_)) => return Output::fail(1, "shift: numeric argument required"),
    };
    let mut pos = state.positional().to_vec();
    if n > pos.len() {
        return Output::status(1);
    }
    pos.drain(..n);
    state.replace_positional(pos);
    Output::default()
}

fn shopt(args: &[String], state: &mut ShellState) -> Output {
    let mut mode = None;
    let mut names = Vec::new();
    for a in args {
        match a.as_str() {
            "-s" | "-u" | "-q" | "-p" => mode = Some(a.as_str()),
            _ => names.push(a.as_str()),
        }
    }
    let all: Vec<&str> = if names.is_empty() {
        SHOPT_OPTIONS.to_vec()
    } else {
        names.clone()
    };
    let mut res = Output::default();
    for name in &all {
        if !SHOPT_OPTIONS.contains(name) {
            res.error(1, format!("shopt: {name}: invalid shell option name"));
            continue;
        }
        let v = state.options().get(name).unwrap_or(false);
        match mode {
            Some("-s") if !names.is_empty() => {
                state.set_option(name, true);
            }
            Some("-u") if !names.is_empty() => {
                state.set_option(name, false);
            }
            Some("-q") => {
                if !v {
                    res.status = 1;
                }
            }
            Some("-s") if !v => {}
            Some("-u") if v => {}
            Some("-p" | "-s" | "-u") => {
                let _ = writeln!(res.stdout, "shopt {} {name}", if v { "-s" } else { "-u" });
            }
            _ => {
                let _ = writeln!(res.stdout, "{name:<15}{}", if v { "on" } else { "off" });
            }
        }
    }
    res
}

fn test_cmd(args: &[String], state: &ShellState) -> Output {
    let toks: Vec<Tok> = args.iter().map(|a| Tok::plain(a)).collect();
    match test::eval(&toks, false, state.cwd(), &|n| state.var(n).is_some()) {
        Ok(true) => Output::default(),
        Ok(false) => Output::status(1),
        Err(e) => Output::fail(2, format!("test: {e}")),
    }
}

/// The canonical trap condition for `name` (`INT` for `SIGINT`, `2`, ...).
pub(crate) fn signal_name(name: &str) -> Option<String> {
    let up = name.to_ascii_uppercase();
    let up = up.strip_prefix("SIG").unwrap_or(&up);
    if let Ok(n) = up.parse::<u32>() {
        if n == 0 {
            return Some("EXIT".into());
        }
        return SIGNALS
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(_, s)| (*s).to_owned());
    }
    (matches!(up, "EXIT" | "ERR" | "DEBUG" | "RETURN") || SIGNALS.iter().any(|(_, s)| *s == up))
        .then(|| up.to_owned())
}

fn trap(args: &[String], state: &mut ShellState) -> Output {
    match args.first().map(String::as_str) {
        None | Some("-p") => {
            let mut out = String::new();
            for (sig, cmd) in state.traps() {
                let _ = writeln!(out, "trap -- {} {sig}", quote(cmd));
            }
            return Output::out(out);
        }
        Some("-l") => {
            let mut out = String::new();
            for (n, s) in SIGNALS {
                let _ = writeln!(out, "{n:>2}) SIG{s}");
            }
            return Output::out(out);
        }
        _ => {}
    }
    let (action, sigs) = match args.split_first() {
        Some((a, [])) => {
            if signal_name(a).is_some() {
                (None, std::slice::from_ref(a))
            } else {
                return Output::fail(2, "trap: usage: trap [-lp] [command] [signal ...]");
            }
        }
        Some((a, rest)) if a == "-" => (None, rest),
        Some((a, rest)) => (Some(a.clone()), rest),
        None => return Output::default(),
    };
    let mut res = Output::default();
    for s in sigs {
        let Some(name) = signal_name(s) else {
            res.error(1, format!("trap: {s}: invalid signal specification"));
            continue;
        };
        match &action {
            Some(cmd) => {
                state.traps_mut().insert(name, cmd.clone());
            }
            None => {
                state.traps_mut().remove(&name);
            }
        }
    }
    res
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0
}

/// Every executable named `name` on `PATH` (or `name` itself if it has a
/// slash), in search order.
pub(crate) fn find_programs(name: &str, state: &ShellState) -> Vec<PathBuf> {
    if name.is_empty() {
        return Vec::new();
    }
    if name.contains('/') {
        let p = state.resolve(name);
        return if is_executable(&p) {
            vec![p]
        } else {
            Vec::new()
        };
    }
    let path = state.var("PATH").unwrap_or_default();
    std::env::split_paths(path)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(name))
        .filter(|p| is_executable(p))
        .collect()
}

fn type_cmd(args: &[String], state: &ShellState) -> Output {
    let mut flags = String::new();
    let mut names = Vec::new();
    for a in args {
        match a.strip_prefix('-') {
            Some(f) if !f.is_empty() && names.is_empty() => flags.push_str(f),
            _ => names.push(a.as_str()),
        }
    }
    let (all, tonly, ponly, force_path, no_func) = (
        flags.contains('a'),
        flags.contains('t'),
        flags.contains('p'),
        flags.contains('P'),
        flags.contains('f'),
    );
    let mut res = Output::default();
    for name in names {
        let mut found: Vec<(&str, String)> = Vec::new();
        if !force_path {
            if let Some(v) = state.aliases().get(name) {
                found.push(("alias", format!("{name} is aliased to `{v}'")));
            }
            if KEYWORDS.contains(&name) {
                found.push(("keyword", format!("{name} is a shell keyword")));
            }
            if !no_func && state.functions().contains_key(name) {
                found.push(("function", format!("{name} is a function")));
            }
            if is_builtin(name) {
                found.push(("builtin", format!("{name} is a shell builtin")));
            }
        }
        let skip_path = !all && !found.is_empty() && !force_path;
        if !skip_path {
            for p in find_programs(name, state) {
                found.push(("file", p.display().to_string()));
                if !all {
                    break;
                }
            }
        }
        if found.is_empty() {
            if !tonly && !ponly {
                res.error(1, format!("xsh: type: {name}: not found"));
            } else {
                res.status = 1;
            }
            continue;
        }
        for (kind, text) in found.iter().take(if all { usize::MAX } else { 1 }) {
            let line = if tonly {
                (*kind).to_owned()
            } else if ponly || force_path {
                if *kind != "file" {
                    continue;
                }
                text.clone()
            } else if *kind == "file" {
                format!("{name} is {text}")
            } else {
                text.clone()
            };
            let _ = writeln!(res.stdout, "{line}");
        }
    }
    res
}

fn which(args: &[String], state: &ShellState) -> Output {
    let all = args.iter().any(|a| a == "-a");
    let mut res = Output::default();
    let names: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if names.is_empty() {
        return Output::fail(1, "which: usage: which [-a] name ...");
    }
    for name in names {
        let mut lines = Vec::new();
        if let Some(v) = state.aliases().get(name.as_str()) {
            lines.push(format!("{name}: aliased to {v}"));
        }
        if state.functions().contains_key(name.as_str()) {
            lines.push(format!("{name}: shell function"));
        }
        if is_builtin(name) {
            lines.push(format!("{name}: shell built-in command"));
        }
        if all || lines.is_empty() {
            for p in find_programs(name, state) {
                lines.push(p.display().to_string());
                if !all {
                    break;
                }
            }
        }
        if lines.is_empty() {
            res.error(1, format!("{name} not found"));
            continue;
        }
        for l in lines.iter().take(if all { usize::MAX } else { 1 }) {
            let _ = writeln!(res.stdout, "{l}");
        }
    }
    res
}

fn symbolic_umask(mask: u32) -> String {
    let part = |shift: u32| {
        let bits = !(mask >> shift) & 7;
        let mut s = String::new();
        for (b, c) in [(4, 'r'), (2, 'w'), (1, 'x')] {
            if bits & b != 0 {
                s.push(c);
            }
        }
        s
    };
    format!("u={},g={},o={}", part(6), part(3), part(0))
}

fn parse_symbolic(spec: &str, mut mask: u32) -> Option<u32> {
    for clause in spec.split(',') {
        let pos = clause.find(['=', '+', '-'])?;
        let (who, rest) = clause.split_at(pos);
        let op = rest.chars().next()?;
        let mut perm = 0;
        for c in rest.chars().skip(1) {
            perm |= match c {
                'r' => 4,
                'w' => 2,
                'x' => 1,
                _ => return None,
            };
        }
        let shifts: Vec<u32> = if who.is_empty() || who.contains('a') {
            vec![6, 3, 0]
        } else {
            who.chars()
                .map(|c| match c {
                    'u' => Some(6),
                    'g' => Some(3),
                    'o' => Some(0),
                    _ => None,
                })
                .collect::<Option<_>>()?
        };
        for s in shifts {
            let allowed = !(mask >> s) & 7;
            let next = match op {
                '=' => perm,
                '+' => allowed | perm,
                _ => allowed & !perm,
            };
            mask = (mask & !(7 << s)) | ((!next & 7) << s);
        }
    }
    Some(mask)
}

async fn current_umask(state: &mut ShellState) -> u32 {
    if let Some(m) = state.umask() {
        return m;
    }
    let out = system("sh", &["-c".to_owned(), "umask".to_owned()], state).await;
    let m = u32::from_str_radix(out.stdout.trim(), 8).unwrap_or(0o022);
    state.set_umask(m);
    m
}

async fn umask(args: &[String], state: &mut ShellState) -> Output {
    let symbolic = args.iter().any(|a| a == "-S");
    let mode = args.iter().find(|a| *a != "-S" && *a != "-p");
    let cur = current_umask(state).await;
    match mode {
        None => Output::out(if symbolic {
            format!("{}\n", symbolic_umask(cur))
        } else {
            format!("{cur:04o}\n")
        }),
        Some(m) => {
            let new = if m.chars().all(|c| c.is_digit(8)) {
                u32::from_str_radix(m, 8).ok().filter(|v| *v <= 0o777)
            } else {
                parse_symbolic(m, cur)
            };
            match new {
                Some(v) => {
                    state.set_umask(v);
                    Output::default()
                }
                None => Output::fail(1, format!("umask: {m}: invalid mode")),
            }
        }
    }
}

async fn ulimit(args: &[String], state: &ShellState) -> Output {
    if let Some(v) = args.iter().find(|a| !a.starts_with('-')) {
        return Output::fail(
            1,
            format!("ulimit: {v}: xsh cannot change resource limits of its own process"),
        );
    }
    let script = std::iter::once("ulimit".to_owned())
        .chain(args.iter().map(|a| quote(a)))
        .collect::<Vec<_>>()
        .join(" ");
    system("sh", &["-c".to_owned(), script], state).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st() -> ShellState {
        ShellState::new(PathBuf::from("/"), Vec::new())
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn echo_handles_flags_and_escapes() {
        assert_eq!(echo(&args(&["a", "b"])).stdout, "a b\n");
        assert_eq!(echo(&args(&["-n", "a"])).stdout, "a");
        assert_eq!(echo(&args(&["-e", "a\\tb\\n"])).stdout, "a\tb\n\n");
        assert_eq!(echo(&args(&["-e", "a\\cb"])).stdout, "a");
        assert_eq!(echo(&args(&["-x"])).stdout, "-x\n");
        assert_eq!(echo(&args(&["-e", "\\0101\\x42"])).stdout, "AB\n");
    }

    #[test]
    fn printf_formats() {
        for (a, want) in [
            (vec!["%s-%s\\n", "a", "b"], "a-b\n"),
            (vec!["%5s|%-5s|", "ab", "cd"], "   ab|cd   |"),
            (
                vec!["%d %i %x %X %o", "42", "-7", "255", "255", "8"],
                "42 -7 ff FF 10",
            ),
            (vec!["%05d|%+d|% d", "42", "5", "5"], "00042|+5| 5"),
            (vec!["%.2f %e", "3.14159", "1234.5"], "3.14 1.234500e+03"),
            (
                vec!["%g %g %g", "0.0001", "100000", "1e10"],
                "0.0001 100000 1e+10",
            ),
            (vec!["%s\\n", "a", "b", "c"], "a\nb\nc\n"),
            (vec!["%c%c", "hello", "world"], "hw"),
            (vec!["%b", "a\\tb"], "a\tb"),
            (vec!["%%|%.3s", "abcdef"], "%|abc"),
            (vec!["%d", "'A"], "65"),
            (vec!["%*d|%-*d|", "4", "7", "3", "1"], "   7|1  |"),
            (vec!["%q", "a b"], "'a b'"),
            (vec!["no args\\n"], "no args\n"),
        ] {
            assert_eq!(printf(&args(&a)).stdout, want, "{a:?}");
        }
        assert_eq!(printf(&args(&["%d", "x"])).status, 1);
        assert_eq!(printf(&[]).status, 2);
    }

    #[test]
    fn set_and_shopt_change_options() {
        let mut s = st();
        set(&args(&["-eu", "-o", "pipefail"]), &mut s);
        assert!(s.options().errexit && s.options().nounset && s.options().pipefail);
        set(&args(&["+e", "--", "a", "b"]), &mut s);
        assert!(!s.options().errexit);
        assert_eq!(s.positional(), ["a", "b"]);
        assert_eq!(set(&args(&["-o", "bogus"]), &mut s).status, 2);
        shopt(&args(&["-s", "autocd", "nullglob"]), &mut s);
        assert!(s.options().autocd && s.options().nullglob);
        assert_eq!(shopt(&args(&["-q", "dotglob"]), &mut s).status, 1);
        assert_eq!(shopt(&args(&["-s", "bogus"]), &mut s).status, 1);
        shift(&args(&["1"]), &mut s);
        assert_eq!(s.positional(), ["b"]);
        assert_eq!(shift(&args(&["5"]), &mut s).status, 1);
    }

    #[test]
    fn trap_stores_and_lists() {
        let mut s = st();
        trap(&args(&["echo bye", "EXIT", "SIGINT", "15"]), &mut s);
        assert_eq!(s.trap("EXIT"), Some("echo bye"));
        assert_eq!(s.trap("INT"), Some("echo bye"));
        assert_eq!(s.trap("TERM"), Some("echo bye"));
        assert!(trap(&[], &mut s).stdout.contains("trap -- 'echo bye' EXIT"));
        trap(&args(&["-", "INT"]), &mut s);
        assert_eq!(s.trap("INT"), None);
        assert_eq!(trap(&args(&["x", "NOPE"]), &mut s).status, 1);
    }

    #[test]
    fn type_and_which_describe_commands() {
        let mut s = st();
        s.set_alias("ll", "ls -l");
        assert!(type_cmd(&args(&["ll"]), &s).stdout.contains("aliased"));
        assert_eq!(
            type_cmd(&args(&["-t", "if", "cd"]), &s).stdout,
            "keyword\nbuiltin\n"
        );
        assert_eq!(type_cmd(&args(&["no-such-cmd-xyz"]), &s).status, 1);
        assert!(which(&args(&["cd"]), &s).stdout.contains("built-in"));
    }

    #[test]
    fn umask_parses_modes() {
        assert_eq!(symbolic_umask(0o022), "u=rwx,g=rx,o=rx");
        assert_eq!(parse_symbolic("g-x,o=", 0o022), Some(0o037));
        assert_eq!(parse_symbolic("a+rwx", 0o077), Some(0));
        assert_eq!(parse_symbolic("z=r", 0), None);
    }

    #[test]
    fn signal_names_are_canonical() {
        assert_eq!(signal_name("sigint").as_deref(), Some("INT"));
        assert_eq!(signal_name("0").as_deref(), Some("EXIT"));
        assert_eq!(signal_name("bogus"), None);
    }
}
