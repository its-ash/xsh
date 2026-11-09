//! Execution of syntax trees: lists, pipelines, compound commands,
//! functions, built-ins, external processes, and redirection.

use std::future::Future;
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, ChildStdout, Command as Proc};
use tokio::task::JoinHandle;

use crate::ast::{
    AndOr, CaseEnd, Command, Compound, Connector, List, Pipe, Redir, RedirOp, Simple,
};
use crate::builtins::{self, Output};
use crate::expand::{Exp, split_fields};
use crate::jobs::JobTable;
use crate::state::ShellState;
use crate::test::{self, Tok};
use crate::{Result, glob, parser};

const TEE_KEEP: usize = 32 * 1024;
const MAX_NESTING: u32 = 1000;

/// A boxed future, for recursion.
pub(crate) type BoxFut<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// How control leaves a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ctl {
    Normal,
    Break(u32),
    Continue(u32),
    Return,
    Exit(i32),
    Abort,
}

#[derive(Debug, Default)]
pub(crate) struct InBuf {
    data: Vec<u8>,
    pos: usize,
}

/// Where a command reads standard input from.
#[derive(Debug, Clone)]
pub(crate) enum Input {
    Inherit,
    Null,
    File(Arc<std::fs::File>),
    Buf(Arc<Mutex<InBuf>>),
    Pipe(Arc<Mutex<Option<ChildStdout>>>),
}

/// The output end of a pipeline stage.
#[derive(Debug, Default)]
pub(crate) struct Slot {
    child: Option<ChildStdout>,
    buf: Arc<Mutex<Vec<u8>>>,
}

/// Where a command writes an output stream.
#[derive(Debug, Clone)]
pub(crate) enum Sink {
    Stdout,
    Stderr,
    Null,
    File(Arc<std::fs::File>),
    Buf(Arc<Mutex<Vec<u8>>>),
    Pipe(Arc<Mutex<Slot>>),
}

/// Standard streams of a command.
#[derive(Debug, Clone)]
pub(crate) struct Io {
    pub(crate) input: Input,
    pub(crate) out: Sink,
    pub(crate) err: Sink,
}

impl Default for Io {
    fn default() -> Self {
        Self {
            input: Input::Inherit,
            out: Sink::Stdout,
            err: Sink::Stderr,
        }
    }
}

fn buf_input(data: Vec<u8>) -> Input {
    Input::Buf(Arc::new(Mutex::new(InBuf { data, pos: 0 })))
}

enum Reader {
    OutTee(JoinHandle<Vec<u8>>),
    ErrTee(JoinHandle<Vec<u8>>),
    Fill(JoinHandle<()>),
}

pub(crate) struct Spawned {
    child: Child,
    readers: Vec<Reader>,
}

pub(crate) enum SimpleRes {
    Done(Ctl),
    Spawned(Spawned),
}

enum Plan {
    Direct,
    OutTee,
    ErrTee,
    Fill(Arc<Mutex<Vec<u8>>>),
    SlotChild(Arc<Mutex<Slot>>),
}

pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    status.signal().map_or(1, |sig| 128 + sig)
}

/// Copies a child's output to our stdout or stderr while keeping the last
/// 32 KiB.
fn tee(mut src: impl AsyncRead + Unpin + Send + 'static, to_stdout: bool) -> JoinHandle<Vec<u8>> {
    tokio::spawn(async move {
        let mut out: Box<dyn AsyncWrite + Unpin + Send> = if to_stdout {
            Box::new(tokio::io::stdout())
        } else {
            Box::new(tokio::io::stderr())
        };
        let mut buf = vec![0u8; 4096];
        let mut keep: Vec<u8> = Vec::new();
        loop {
            match src.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = buf.get(..n).unwrap_or_default();
                    let _ = out.write_all(chunk).await;
                    let _ = out.flush().await;
                    keep.extend_from_slice(chunk);
                    if keep.len() > TEE_KEEP * 2 {
                        let cut = keep.len() - TEE_KEEP;
                        keep.drain(..cut);
                    }
                }
            }
        }
        keep
    })
}

fn fill(
    mut src: impl AsyncRead + Unpin + Send + 'static,
    buf: Arc<Mutex<Vec<u8>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut chunk = vec![0u8; 8192];
        loop {
            match src.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut g) = buf.lock() {
                        g.extend_from_slice(chunk.get(..n).unwrap_or_default());
                    }
                }
            }
        }
    })
}

fn write_stdout(bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let mut out = io::stdout().lock();
    out.write_all(bytes)?;
    out.flush()
}

fn write_stderr(bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let mut err = io::stderr().lock();
    err.write_all(bytes)?;
    err.flush()
}

async fn drain(input: &Input) -> Input {
    if let Input::Pipe(p) = input {
        let taken = p.lock().ok().and_then(|mut g| g.take());
        let mut data = Vec::new();
        if let Some(mut out) = taken {
            let _ = out.read_to_end(&mut data).await;
        }
        return buf_input(data);
    }
    input.clone()
}

fn take_slot(slot: &Arc<Mutex<Slot>>) -> Input {
    let Ok(mut g) = slot.lock() else {
        return Input::Null;
    };
    if let Some(child) = g.child.take() {
        return Input::Pipe(Arc::new(Mutex::new(Some(child))));
    }
    let data = g
        .buf
        .lock()
        .map(|mut b| std::mem::take(&mut *b))
        .unwrap_or_default();
    buf_input(data)
}

/// Runs commands against shell state.
pub(crate) struct Ctx<'a> {
    pub(crate) state: &'a mut ShellState,
    pub(crate) jobs: &'a mut JobTable,
    pub(crate) text: String,
    pub(crate) temps: Vec<PathBuf>,
    pub(crate) after: Vec<(List, PathBuf)>,
    pub(crate) top: bool,
    cond: u32,
    loops: u32,
    funcs: u32,
    sources: u32,
    depth: u32,
    background: bool,
    subst_status: Option<i32>,
}

impl Drop for Ctx<'_> {
    fn drop(&mut self) {
        for p in &self.temps {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(state: &'a mut ShellState, jobs: &'a mut JobTable, text: String) -> Self {
        Self {
            state,
            jobs,
            text,
            temps: Vec::new(),
            after: Vec::new(),
            top: true,
            cond: 0,
            loops: 0,
            funcs: 0,
            sources: 0,
            depth: 0,
            background: false,
            subst_status: None,
        }
    }

    fn sub<'b>(&'b mut self, state: &'b mut ShellState) -> Ctx<'b> {
        Ctx {
            state,
            jobs: &mut *self.jobs,
            text: self.text.clone(),
            temps: Vec::new(),
            after: Vec::new(),
            top: false,
            cond: 0,
            loops: 0,
            funcs: self.funcs,
            sources: self.sources,
            depth: self.depth + 1,
            background: false,
            subst_status: None,
        }
    }

    /// Writes to a sink, recording terminal output for the logs.
    pub(crate) fn write_sink(&mut self, sink: &Sink, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        match sink {
            Sink::Stdout => {
                write_stdout(bytes)?;
                self.state.push_stdout(bytes);
            }
            Sink::Stderr => {
                self.state.push_stderr(bytes);
                write_stderr(bytes)?;
            }
            Sink::Null => {}
            Sink::File(f) => {
                let mut w: &std::fs::File = f;
                match w.write_all(bytes) {
                    Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
                    other => other?,
                }
            }
            Sink::Buf(b) => {
                if let Ok(mut g) = b.lock() {
                    g.extend_from_slice(bytes);
                }
            }
            Sink::Pipe(slot) => {
                let buf = slot.lock().map(|g| Arc::clone(&g.buf));
                if let Ok(buf) = buf {
                    if let Ok(mut g) = buf.lock() {
                        g.extend_from_slice(bytes);
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn write_err(&mut self, io: &Io, msg: &str) -> Result<()> {
        let mut text = msg.to_owned();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        self.write_sink(&io.err, text.as_bytes())
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "each builtin result is used once"
    )]
    fn emit(&mut self, out: Output, io: &Io) -> Result<Ctl> {
        self.write_sink(&io.out, out.stdout.as_bytes())?;
        self.write_sink(&io.err, out.stderr.as_bytes())?;
        self.state.set_last_status(out.status);
        Ok(out.exit.map_or(Ctl::Normal, Ctl::Exit))
    }

    fn abort(&mut self, io: &Io, msg: &str) -> Result<SimpleRes> {
        self.write_err(io, msg)?;
        self.state.set_last_status(1);
        Ok(SimpleRes::Done(Ctl::Abort))
    }

    pub(crate) fn run_list<'s>(&'s mut self, list: &'s List, io: &'s Io) -> BoxFut<'s, Ctl> {
        Box::pin(async move {
            for item in &list.0 {
                if self.top {
                    self.state.begin_capture(&self.text);
                }
                let ctl = if item.background {
                    self.background_item(&item.and_or, io).await?
                } else {
                    self.run_and_or(&item.and_or, io).await?
                };
                if ctl != Ctl::Normal {
                    return Ok(ctl);
                }
            }
            Ok(Ctl::Normal)
        })
    }

    fn run_and_or<'s>(&'s mut self, ao: &'s AndOr, io: &'s Io) -> BoxFut<'s, Ctl> {
        Box::pin(async move {
            let n = ao.rest.len();
            let mut ctl = self.run_pipe_in(&ao.first, io, n > 0).await?;
            let mut eligible = n == 0 && !ao.first.negated;
            for (i, (conn, pipe)) in ao.rest.iter().enumerate() {
                if ctl != Ctl::Normal {
                    return Ok(ctl);
                }
                let ok = self.state.last_status() == 0;
                if (*conn == Connector::And) != ok {
                    eligible = false;
                    continue;
                }
                let last = i + 1 == n;
                ctl = self.run_pipe_in(pipe, io, !last).await?;
                eligible = last && !pipe.negated;
            }
            if ctl == Ctl::Normal && eligible && self.cond == 0 && self.state.last_status() != 0 {
                let status = self.state.last_status();
                if let Some(cmd) = self.state.trap("ERR").map(str::to_owned) {
                    if !cmd.is_empty() {
                        self.run_text(&cmd, io).await?;
                        self.state.set_last_status(status);
                    }
                }
                if self.state.options().errexit {
                    return Ok(Ctl::Exit(status));
                }
            }
            Ok(ctl)
        })
    }

    async fn run_pipe_in(&mut self, pipe: &Pipe, io: &Io, in_cond: bool) -> Result<Ctl> {
        let inc = in_cond || pipe.negated;
        if inc {
            self.cond += 1;
        }
        let res = self.run_pipe(pipe, io, false).await;
        if inc {
            self.cond -= 1;
        }
        let ctl = res?;
        if pipe.negated && ctl == Ctl::Normal {
            let s = self.state.last_status();
            self.state.set_last_status(i32::from(s == 0));
        }
        Ok(ctl)
    }

    #[allow(clippy::too_many_lines)]
    fn run_pipe<'s>(&'s mut self, pipe: &'s Pipe, io: &'s Io, background: bool) -> BoxFut<'s, Ctl> {
        Box::pin(async move {
            if let (false, [cmd]) = (background, pipe.commands.as_slice()) {
                return self.run_command(cmd, io).await;
            }
            let n = pipe.commands.len();
            let mut input = if background && matches!(io.input, Input::Inherit) {
                Input::Null
            } else {
                io.input.clone()
            };
            let mut procs: Vec<(usize, Spawned)> = Vec::new();
            let mut statuses = vec![0; n];
            let saved_bg = self.background;
            self.background = background;
            for (i, cmd) in pipe.commands.iter().enumerate() {
                let last = i + 1 == n;
                let slot = (!last).then(|| Arc::new(Mutex::new(Slot::default())));
                let out = slot.clone().map_or_else(|| io.out.clone(), Sink::Pipe);
                let stage = Io {
                    input: input.clone(),
                    out,
                    err: io.err.clone(),
                };
                let res = match cmd {
                    Command::Simple(s) => self.exec_simple(s, &stage).await,
                    other => {
                        let stage = self.for_compound(&stage).await;
                        self.run_command(other, &stage).await.map(SimpleRes::Done)
                    }
                };
                match res {
                    Ok(SimpleRes::Spawned(sp)) => {
                        let piped = slot
                            .as_ref()
                            .and_then(|s| s.lock().ok().map(|g| g.child.is_some()))
                            .unwrap_or(true);
                        if piped || background {
                            procs.push((i, sp));
                        } else {
                            let code = self.finish(sp).await?;
                            if let Some(s) = statuses.get_mut(i) {
                                *s = code;
                            }
                        }
                    }
                    Ok(SimpleRes::Done(_)) => {
                        if let Some(s) = statuses.get_mut(i) {
                            *s = self.state.last_status();
                        }
                    }
                    Err(e) => {
                        self.background = saved_bg;
                        return Err(e);
                    }
                }
                if let Some(slot) = slot {
                    input = take_slot(&slot);
                }
            }
            self.background = saved_bg;
            if background {
                let children: Vec<Child> = procs.into_iter().map(|(_, sp)| sp.child).collect();
                if !children.is_empty() {
                    let (id, pids) = self.jobs.add(&self.text, children);
                    self.state.set_last_bg(pids.last().copied());
                    let pid = pids.last().map_or_else(String::new, u32::to_string);
                    self.write_sink(&Sink::Stderr, format!("[{id}] {pid}\n").as_bytes())?;
                }
                self.state.set_last_status(0);
                return Ok(Ctl::Normal);
            }
            let fg = self.state.foreground();
            fg.clear_stop();
            self.jobs.end_stop_group();
            fg.set(procs.iter().filter_map(|(_, sp)| sp.child.id()).collect());
            for (i, sp) in procs {
                let code = self.finish(sp).await;
                let code = match code {
                    Ok(c) => c,
                    Err(e) => {
                        fg.set(Vec::new());
                        return Err(e);
                    }
                };
                if let Some(s) = statuses.get_mut(i) {
                    *s = code;
                }
            }
            fg.set(Vec::new());
            let status = if self.state.options().pipefail {
                statuses
                    .iter()
                    .rev()
                    .find(|s| **s != 0)
                    .copied()
                    .unwrap_or(0)
            } else {
                statuses.last().copied().unwrap_or(0)
            };
            self.state.set_last_status(status);
            Ok(Ctl::Normal)
        })
    }

    /// Streams for a compound command or function inside a pipeline: input
    /// read into memory, and output collected for the next stage.
    async fn for_compound(&self, io: &Io) -> Io {
        let to_buf = |s: &Sink| match s {
            Sink::Pipe(slot) => slot
                .lock()
                .map_or(Sink::Null, |g| Sink::Buf(Arc::clone(&g.buf))),
            other => other.clone(),
        };
        Io {
            input: drain(&io.input).await,
            out: to_buf(&io.out),
            err: to_buf(&io.err),
        }
    }

    fn background_item<'s>(&'s mut self, ao: &'s AndOr, io: &'s Io) -> BoxFut<'s, Ctl> {
        Box::pin(async move {
            let simple = ao.rest.is_empty()
                && !ao.first.negated
                && ao
                    .first
                    .commands
                    .iter()
                    .all(|c| matches!(c, Command::Simple(_)));
            if simple {
                return self.run_pipe(&ao.first, io, true).await;
            }
            let mut st = self.state.clone();
            let ao = ao.clone();
            let mut io = io.clone();
            if matches!(io.input, Input::Inherit) {
                io.input = Input::Null;
            }
            let text = self.text.clone();
            let result = Arc::new(Mutex::new(None));
            let cell = Arc::clone(&result);
            let handle = tokio::spawn(async move {
                let mut jobs = JobTable::default();
                let code = {
                    let mut ctx = Ctx::new(&mut st, &mut jobs, text);
                    ctx.top = false;
                    // `Ctrl-Z` belongs to the foreground command, not to this job.
                    ctx.background = true;
                    match ctx.run_and_or(&ao, &io).await {
                        Ok(Ctl::Exit(c)) => c,
                        Ok(_) => ctx.state.last_status(),
                        Err(_) => 1,
                    }
                };
                if let Ok(mut g) = cell.lock() {
                    *g = Some(code);
                }
                code
            });
            let id = self.jobs.add_task(&self.text, handle, result);
            self.state.set_last_bg(None);
            self.write_sink(&Sink::Stderr, format!("[{id}]\n").as_bytes())?;
            self.state.set_last_status(0);
            Ok(Ctl::Normal)
        })
    }

    pub(crate) fn run_command<'s>(&'s mut self, cmd: &'s Command, io: &'s Io) -> BoxFut<'s, Ctl> {
        Box::pin(async move {
            let ctl = match cmd {
                Command::Simple(s) => match self.exec_simple(s, io).await? {
                    SimpleRes::Done(c) => c,
                    SimpleRes::Spawned(sp) => {
                        let fg = self.state.foreground();
                        fg.clear_stop();
                        self.jobs.end_stop_group();
                        fg.set(sp.child.id().into_iter().collect());
                        let code = self.finish(sp).await;
                        fg.set(Vec::new());
                        self.state.set_last_status(code?);
                        Ctl::Normal
                    }
                },
                Command::Compound(c, redirs) => match self.open_redirs(redirs, io).await? {
                    Ok(io2) => self.run_compound(c, &io2).await?,
                    Err(msg) => {
                        self.write_err(io, &msg)?;
                        self.state.set_last_status(1);
                        Ctl::Normal
                    }
                },
                Command::Function(name, body) => {
                    self.state.set_function(name.clone(), Arc::clone(body));
                    self.state.set_last_status(0);
                    Ctl::Normal
                }
            };
            self.run_after().await?;
            Ok(ctl)
        })
    }

    async fn run_after(&mut self) -> Result<()> {
        let pending = std::mem::take(&mut self.after);
        for (body, path) in pending {
            let file = match std::fs::File::open(&path) {
                Ok(f) => Arc::new(f),
                Err(_) => continue,
            };
            let io = Io {
                input: Input::File(file),
                ..Io::default()
            };
            let status = self.state.last_status();
            self.subshell(&body, &io).await?;
            self.state.set_last_status(status);
        }
        Ok(())
    }

    /// Waits for a spawned process. In the foreground, `Ctrl-Z` ends the
    /// wait: the process is kept as a stopped job and the status is 148.
    async fn finish(&mut self, sp: Spawned) -> Result<i32> {
        let Spawned { mut child, readers } = sp;
        let fg = self.state.foreground();
        let waited = if self.background || fg.pids().is_empty() {
            Some(child.wait().await?)
        } else if fg.is_stopped() {
            None
        } else {
            tokio::select! {
                s = child.wait() => Some(s?),
                () = fg.stopped() => None,
            }
        };
        let Some(status) = waited else {
            let (id, new) = self.jobs.add_stopped(&self.text, child);
            if new {
                let msg = format!("\n[{id}]+  Stopped    {}\n", self.text);
                self.write_sink(&Sink::Stderr, msg.as_bytes())?;
            }
            return Ok(148);
        };
        let code = exit_code(status);
        for r in readers {
            match r {
                Reader::OutTee(h) => {
                    if let Ok(b) = h.await {
                        self.state.push_stdout(&b);
                    }
                }
                Reader::ErrTee(h) => {
                    if let Ok(b) = h.await {
                        self.state.push_stderr(&b);
                    }
                }
                Reader::Fill(h) => {
                    let _ = h.await;
                }
            }
        }
        Ok(code)
    }

    fn trace(&mut self, assigns: &[(String, String)], argv: &[String], io: &Io) -> Result<()> {
        let mut parts: Vec<String> = assigns
            .iter()
            .map(|(k, v)| format!("{k}={}", builtins::quote_if_needed(v)))
            .collect();
        parts.extend(argv.iter().map(|a| builtins::quote_if_needed(a)));
        self.write_err(io, &format!("+ {}", parts.join(" ")))
    }

    fn apply_temp(&mut self, assigns: &[(String, String)]) -> Vec<(String, Option<String>)> {
        assigns
            .iter()
            .map(|(k, v)| {
                let old = self.state.var(k).map(str::to_owned);
                self.state.set_var(k.clone(), v.clone());
                (k.clone(), old)
            })
            .collect()
    }

    fn restore_temp(&mut self, saved: Vec<(String, Option<String>)>) {
        for (k, old) in saved.into_iter().rev() {
            match old {
                Some(v) => self.state.set_var(k, v),
                None => self.state.unset_var(&k),
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        clippy::similar_names,
        reason = "argv is split into name and args"
    )]
    pub(crate) fn exec_simple<'s>(
        &'s mut self,
        s: &'s Simple,
        io: &'s Io,
    ) -> BoxFut<'s, SimpleRes> {
        Box::pin(async move {
            self.subst_status = None;
            let mut argv = Vec::new();
            for w in &s.words {
                match self.expand_fields(w).await? {
                    Ok(f) => argv.extend(f),
                    Err(msg) => return self.abort(io, &msg),
                }
            }
            let mut assigns = Vec::with_capacity(s.assigns.len());
            for a in &s.assigns {
                let v = match self.expand_single(&a.value).await? {
                    Ok(v) => v,
                    Err(msg) => return self.abort(io, &msg),
                };
                let v = if a.append {
                    format!("{}{v}", self.state.var(&a.name).unwrap_or_default())
                } else {
                    v
                };
                assigns.push((a.name.clone(), v));
            }
            if self.state.options().xtrace {
                self.trace(&assigns, &argv, io)?;
            }
            let io2 = match self.open_redirs(&s.redirects, io).await? {
                Ok(io2) => io2,
                Err(msg) => {
                    self.write_err(io, &msg)?;
                    self.state.set_last_status(1);
                    return Ok(SimpleRes::Done(Ctl::Normal));
                }
            };
            let Some((first, rest)) = argv.split_first() else {
                for (k, v) in assigns {
                    self.state.set_var(k, v);
                }
                let status = self.subst_status.unwrap_or(0);
                self.state.set_last_status(status);
                return Ok(SimpleRes::Done(Ctl::Normal));
            };
            let (name, args) = (first.clone(), rest.to_vec());
            if let Some(body) = self.state.functions().get(&name).cloned() {
                let saved = self.apply_temp(&assigns);
                let io3 = self.for_compound(&io2).await;
                let ctl = self.call_function(&body, args, &io3).await;
                self.restore_temp(saved);
                return Ok(SimpleRes::Done(ctl?));
            }
            if builtins::is_builtin(&name) {
                let saved = self.apply_temp(&assigns);
                let ctl = self.run_builtin(&name, &args, &io2).await;
                self.restore_temp(saved);
                return Ok(SimpleRes::Done(ctl?));
            }
            if args.is_empty()
                && assigns.is_empty()
                && self.state.options().autocd
                && builtins::find_programs(&name, self.state).is_empty()
                && self.state.resolve(&name).is_dir()
            {
                let ctl = self.run_builtin("cd", &[name], &io2).await?;
                return Ok(SimpleRes::Done(ctl));
            }
            match self.spawn(&name, &args, &assigns, &io2)? {
                Ok(sp) => Ok(SimpleRes::Spawned(sp)),
                Err(status) => {
                    self.state.set_last_status(status);
                    Ok(SimpleRes::Done(Ctl::Normal))
                }
            }
        })
    }

    fn plan(&self, sink: &Sink, is_err: bool, same_pipe: bool) -> Result<(Stdio, Plan)> {
        let bg = self.background;
        Ok(match sink {
            Sink::Stdout => {
                if self.state.log_enabled() && !bg {
                    (Stdio::piped(), Plan::OutTee)
                } else if is_err {
                    (Stdio::from(io::stdout()), Plan::Direct)
                } else {
                    (Stdio::inherit(), Plan::Direct)
                }
            }
            Sink::Stderr => {
                if (self.state.capture_stderr() || self.state.log_enabled()) && !bg {
                    (Stdio::piped(), Plan::ErrTee)
                } else if is_err {
                    (Stdio::inherit(), Plan::Direct)
                } else {
                    (Stdio::from(io::stderr()), Plan::Direct)
                }
            }
            Sink::Null => (Stdio::null(), Plan::Direct),
            Sink::File(f) => (Stdio::from(f.try_clone()?), Plan::Direct),
            Sink::Buf(b) => (Stdio::piped(), Plan::Fill(Arc::clone(b))),
            Sink::Pipe(slot) => {
                let (empty, buf) = slot.lock().map_or((false, Arc::default()), |g| {
                    let empty = g.child.is_none() && g.buf.lock().is_ok_and(|b| b.is_empty());
                    (empty, Arc::clone(&g.buf))
                });
                if !is_err && !same_pipe && empty {
                    (Stdio::piped(), Plan::SlotChild(Arc::clone(slot)))
                } else {
                    (Stdio::piped(), Plan::Fill(buf))
                }
            }
        })
    }

    fn spawn(
        &mut self,
        name: &str,
        args: &[String],
        assigns: &[(String, String)],
        io: &Io,
    ) -> Result<std::result::Result<Spawned, i32>> {
        let mut command = Proc::new(name);
        command
            .args(args)
            .env_clear()
            .envs(self.state.env_vars())
            .envs(assigns.iter().map(|(k, v)| (k, v)))
            .current_dir(self.state.cwd());
        if self.background {
            command.process_group(0);
        }
        let mut feed = None;
        let stdin = match &io.input {
            Input::Inherit => Stdio::inherit(),
            Input::Null => Stdio::null(),
            Input::File(f) => Stdio::from(f.try_clone()?),
            Input::Buf(b) => {
                let bytes = b
                    .lock()
                    .map(|mut g| {
                        let rest = g.data.get(g.pos..).unwrap_or_default().to_vec();
                        g.pos = g.data.len();
                        rest
                    })
                    .unwrap_or_default();
                feed = Some(bytes);
                Stdio::piped()
            }
            Input::Pipe(p) => match p.lock().ok().and_then(|mut g| g.take()) {
                Some(out) => out.try_into()?,
                None => Stdio::null(),
            },
        };
        let same_pipe =
            matches!((&io.out, &io.err), (Sink::Pipe(a), Sink::Pipe(b)) if Arc::ptr_eq(a, b));
        let (stdout, out_plan) = self.plan(&io.out, false, same_pipe)?;
        let (stderr, err_plan) = self.plan(&io.err, true, same_pipe)?;
        command.stdin(stdin).stdout(stdout).stderr(stderr);
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                let (status, msg) = match e.kind() {
                    ErrorKind::NotFound => (127, format!("xsh: command not found: {name}")),
                    ErrorKind::PermissionDenied => (126, format!("xsh: permission denied: {name}")),
                    _ => (126, format!("xsh: {name}: {e}")),
                };
                self.write_err(io, &msg)?;
                return Ok(Err(status));
            }
        };
        if let (Some(bytes), Some(mut sink)) = (feed, child.stdin.take()) {
            tokio::spawn(async move {
                let _ = sink.write_all(&bytes).await;
            });
        }
        let mut readers = Vec::new();
        match out_plan {
            Plan::Direct => {}
            Plan::OutTee => {
                readers.extend(child.stdout.take().map(|s| Reader::OutTee(tee(s, true))));
            }
            Plan::ErrTee => {
                readers.extend(child.stdout.take().map(|s| Reader::ErrTee(tee(s, false))));
            }
            Plan::Fill(b) => readers.extend(child.stdout.take().map(|s| Reader::Fill(fill(s, b)))),
            Plan::SlotChild(slot) => {
                if let Ok(mut g) = slot.lock() {
                    g.child = child.stdout.take();
                }
            }
        }
        match err_plan {
            Plan::Direct | Plan::SlotChild(_) => {}
            Plan::OutTee => {
                readers.extend(child.stderr.take().map(|s| Reader::OutTee(tee(s, true))));
            }
            Plan::ErrTee => {
                readers.extend(child.stderr.take().map(|s| Reader::ErrTee(tee(s, false))));
            }
            Plan::Fill(b) => readers.extend(child.stderr.take().map(|s| Reader::Fill(fill(s, b)))),
        }
        Ok(Ok(Spawned { child, readers }))
    }

    async fn call_function(
        &mut self,
        body: &Arc<Command>,
        args: Vec<String>,
        io: &Io,
    ) -> Result<Ctl> {
        if self.depth >= MAX_NESTING {
            self.write_err(io, "xsh: maximum function nesting level exceeded")?;
            self.state.set_last_status(1);
            return Ok(Ctl::Abort);
        }
        self.depth += 1;
        self.funcs += 1;
        let loops = std::mem::take(&mut self.loops);
        let old = self.state.replace_positional(args);
        self.state.push_frame();
        let res = self.run_command(body, io).await;
        self.state.pop_frame();
        self.state.replace_positional(old);
        self.loops = loops;
        self.funcs -= 1;
        self.depth -= 1;
        Ok(match res? {
            Ctl::Return | Ctl::Break(_) | Ctl::Continue(_) => Ctl::Normal,
            other => other,
        })
    }

    /// Runs `list` in a copy of the state; returns its status.
    pub(crate) async fn subshell(&mut self, list: &List, io: &Io) -> Result<i32> {
        let mut st = self.state.clone();
        let ctl = {
            let mut sub = self.sub(&mut st);
            sub.run_list(list, io).await?
        };
        let status = match ctl {
            Ctl::Exit(c) => c,
            _ => st.last_status(),
        };
        self.state.adopt_logs(&mut st);
        self.state.set_last_status(status);
        Ok(status)
    }

    /// Runs `list` in a subshell and returns its output without trailing
    /// newlines.
    pub(crate) async fn capture(&mut self, list: &List) -> Result<String> {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let io = Io {
            out: Sink::Buf(Arc::clone(&buf)),
            ..Io::default()
        };
        let status = self.subshell(list, &io).await?;
        self.subst_status = Some(status);
        let bytes = buf.lock().map(|g| g.clone()).unwrap_or_default();
        let mut out = String::from_utf8_lossy(&bytes).into_owned();
        while out.ends_with('\n') {
            out.pop();
        }
        Ok(out)
    }

    async fn loop_body(&mut self, body: &List, io: &Io) -> Result<Option<Ctl>> {
        self.loops += 1;
        let res = self.run_list(body, io).await;
        self.loops -= 1;
        Ok(match res? {
            Ctl::Break(n) if n > 1 => Some(Ctl::Break(n - 1)),
            Ctl::Break(_) => Some(Ctl::Normal),
            Ctl::Continue(n) if n > 1 => Some(Ctl::Continue(n - 1)),
            Ctl::Continue(_) | Ctl::Normal => None,
            other => Some(other),
        })
    }

    async fn cond_list(&mut self, list: &List, io: &Io) -> Result<Ctl> {
        self.cond += 1;
        let res = self.run_list(list, io).await;
        self.cond -= 1;
        res
    }

    #[allow(clippy::too_many_lines)]
    fn run_compound<'s>(&'s mut self, c: &'s Compound, io: &'s Io) -> BoxFut<'s, Ctl> {
        Box::pin(async move {
            match c {
                Compound::Group(list) => self.run_list(list, io).await,
                Compound::Subshell(list) => {
                    self.subshell(list, io).await?;
                    Ok(Ctl::Normal)
                }
                Compound::If {
                    branches,
                    otherwise,
                } => {
                    for (cond, body) in branches {
                        let ctl = self.cond_list(cond, io).await?;
                        if ctl != Ctl::Normal {
                            return Ok(ctl);
                        }
                        if self.state.last_status() == 0 {
                            return self.run_list(body, io).await;
                        }
                    }
                    if let Some(list) = otherwise {
                        self.run_list(list, io).await
                    } else {
                        self.state.set_last_status(0);
                        Ok(Ctl::Normal)
                    }
                }
                Compound::While { until, cond, body } => {
                    let mut status = 0;
                    loop {
                        let ctl = self.cond_list(cond, io).await?;
                        if ctl != Ctl::Normal {
                            return Ok(ctl);
                        }
                        if (self.state.last_status() == 0) == *until {
                            break;
                        }
                        let stop = self.loop_body(body, io).await?;
                        status = self.state.last_status();
                        match stop {
                            None => {}
                            Some(Ctl::Normal) => break,
                            Some(other) => return Ok(other),
                        }
                    }
                    self.state.set_last_status(status);
                    Ok(Ctl::Normal)
                }
                Compound::For { var, items, body } => {
                    let values = match items {
                        None => self.state.positional().to_vec(),
                        Some(words) => {
                            let mut v = Vec::new();
                            for w in words {
                                match self.expand_fields(w).await? {
                                    Ok(f) => v.extend(f),
                                    Err(msg) => {
                                        self.write_err(io, &msg)?;
                                        self.state.set_last_status(1);
                                        return Ok(Ctl::Abort);
                                    }
                                }
                            }
                            v
                        }
                    };
                    self.state.set_last_status(0);
                    for value in values {
                        self.state.set_var(var.clone(), value);
                        match self.loop_body(body, io).await? {
                            None => {}
                            Some(Ctl::Normal) => break,
                            Some(other) => return Ok(other),
                        }
                    }
                    Ok(Ctl::Normal)
                }
                Compound::ArithFor {
                    init,
                    cond,
                    step,
                    body,
                } => {
                    if let Err(msg) = self.arith_word(init).await? {
                        return self.arith_fail(io, &msg);
                    }
                    self.state.set_last_status(0);
                    loop {
                        let empty = cond.segs.iter().all(|s| {
                            matches!(s, crate::ast::Seg::Lit { text, .. } if text.trim().is_empty())
                        });
                        if !empty {
                            match self.arith_word(cond).await? {
                                Ok(0) => break,
                                Ok(_) => {}
                                Err(msg) => return self.arith_fail(io, &msg),
                            }
                        }
                        match self.loop_body(body, io).await? {
                            None => {}
                            Some(Ctl::Normal) => break,
                            Some(other) => return Ok(other),
                        }
                        if let Err(msg) = self.arith_word(step).await? {
                            return self.arith_fail(io, &msg);
                        }
                    }
                    Ok(Ctl::Normal)
                }
                Compound::Case { word, arms } => {
                    let subject = match self.expand_single(word).await? {
                        Ok(s) => s,
                        Err(msg) => return self.arith_fail(io, &msg),
                    };
                    self.state.set_last_status(0);
                    let mut fall = false;
                    for arm in arms {
                        let mut hit = fall;
                        if !hit {
                            for p in &arm.patterns {
                                let pat = match self.expand_pattern(p).await? {
                                    Ok(p) => p,
                                    Err(msg) => return self.arith_fail(io, &msg),
                                };
                                if glob::matches(&pat, &subject) {
                                    hit = true;
                                    break;
                                }
                            }
                        }
                        if !hit {
                            continue;
                        }
                        let ctl = self.run_list(&arm.body, io).await?;
                        if ctl != Ctl::Normal {
                            return Ok(ctl);
                        }
                        match arm.end {
                            CaseEnd::Break => return Ok(Ctl::Normal),
                            CaseEnd::Fall => fall = true,
                            CaseEnd::Test => fall = false,
                        }
                    }
                    Ok(Ctl::Normal)
                }
                Compound::Arith(expr) => match self.arith_word(expr).await? {
                    Ok(v) => {
                        if self.state.options().xtrace {
                            self.write_err(io, &format!("+ (( {v} ))"))?;
                        }
                        self.state.set_last_status(i32::from(v == 0));
                        Ok(Ctl::Normal)
                    }
                    Err(msg) => self.arith_fail(io, &msg),
                },
                Compound::Cond(words) => self.cond_expr(words, io).await,
            }
        })
    }

    fn arith_fail(&mut self, io: &Io, msg: &str) -> Result<Ctl> {
        self.write_err(io, msg)?;
        self.state.set_last_status(1);
        Ok(Ctl::Normal)
    }

    async fn cond_expr(&mut self, words: &[crate::ast::Word], io: &Io) -> Result<Ctl> {
        const OPS: &[&str] = &["&&", "||", "!", "(", ")", "==", "=", "!=", "<", ">", "=~"];
        let mut toks = Vec::with_capacity(words.len());
        let mut pattern_next = false;
        for w in words {
            let op = w
                .plain()
                .is_some_and(|p| OPS.contains(&p) || p.starts_with('-'));
            let text = match self.expand_single(w).await? {
                Ok(t) => t,
                Err(msg) => return self.arith_fail(io, &msg),
            };
            let pattern = if pattern_next {
                match self.expand_pattern(w).await? {
                    Ok(p) => Some(p),
                    Err(msg) => return self.arith_fail(io, &msg),
                }
            } else {
                None
            };
            pattern_next = op && matches!(text.as_str(), "==" | "=" | "!=");
            toks.push(Tok { text, op, pattern });
        }
        if self.state.options().xtrace {
            let line: Vec<&str> = toks.iter().map(|t| t.text.as_str()).collect();
            self.write_err(io, &format!("+ [[ {} ]]", line.join(" ")))?;
        }
        let res = {
            let state = &*self.state;
            test::eval(&toks, true, state.cwd(), &|n| state.var(n).is_some())
        };
        match res {
            Ok(v) => self.state.set_last_status(i32::from(!v)),
            Err(e) => {
                self.write_err(io, &format!("xsh: [[: {e}"))?;
                self.state.set_last_status(2);
            }
        }
        Ok(Ctl::Normal)
    }

    /// Parses and runs `text` in this context (for traps and `eval`).
    pub(crate) async fn run_text(&mut self, text: &str, io: &Io) -> Result<Ctl> {
        match parser::parse_program(text, self.state.aliases()) {
            Ok(list) => self.run_list(&list, io).await,
            Err(e) => {
                self.write_err(io, &format!("xsh: {e}"))?;
                self.state.set_last_status(2);
                Ok(Ctl::Normal)
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn open_redirs(&mut self, redirs: &[Redir], io: &Io) -> Result<Exp<Io>> {
        let mut io = io.clone();
        for r in redirs {
            let input_op = matches!(
                r.op,
                RedirOp::In(_)
                    | RedirOp::ReadWrite(_)
                    | RedirOp::DupIn(_)
                    | RedirOp::HereDoc(_)
                    | RedirOp::HereString(_)
            );
            let fd = r.fd.unwrap_or(u32::from(!input_op));
            let target = match &r.op {
                RedirOp::HereDoc(w) | RedirOp::HereString(w) => {
                    match self.expand_single(w).await? {
                        Ok(t) => t,
                        Err(e) => return Ok(Err(e)),
                    }
                }
                RedirOp::In(w)
                | RedirOp::Out { target: w, .. }
                | RedirOp::Append(w)
                | RedirOp::ReadWrite(w)
                | RedirOp::DupOut(w)
                | RedirOp::DupIn(w)
                | RedirOp::Both { target: w, .. } => match self.expand_single(w).await? {
                    Ok(t) if t.is_empty() => return Ok(Err("xsh: ambiguous redirect".into())),
                    Ok(t) => t,
                    Err(e) => return Ok(Err(e)),
                },
            };
            if input_op {
                if fd != 0 {
                    return Ok(Err(format!("xsh: {fd}: input redirection only works on 0")));
                }
                io.input = match &r.op {
                    RedirOp::HereDoc(_) => buf_input(target.into_bytes()),
                    RedirOp::HereString(_) => buf_input(format!("{target}\n").into_bytes()),
                    RedirOp::DupIn(_) => match target.as_str() {
                        "-" => Input::Null,
                        "0" => io.input.clone(),
                        _ => return Ok(Err(format!("xsh: {target}: bad file descriptor"))),
                    },
                    RedirOp::ReadWrite(_) => {
                        match self.open_file(&target, true, true, false, false) {
                            Ok(f) => Input::File(f),
                            Err(e) => return Ok(Err(e)),
                        }
                    }
                    _ => match self.open_file(&target, true, false, false, false) {
                        Ok(f) => Input::File(f),
                        Err(e) => return Ok(Err(e)),
                    },
                };
                continue;
            }
            let sink = match &r.op {
                RedirOp::Out { clobber, .. } => {
                    if self.state.options().noclobber
                        && !clobber
                        && self.state.resolve(&target).is_file()
                    {
                        return Ok(Err(format!(
                            "xsh: {target}: cannot overwrite existing file"
                        )));
                    }
                    match self.open_file(&target, false, true, false, true) {
                        Ok(f) => Sink::File(f),
                        Err(e) => return Ok(Err(e)),
                    }
                }
                RedirOp::Append(_) => match self.open_file(&target, false, true, true, false) {
                    Ok(f) => Sink::File(f),
                    Err(e) => return Ok(Err(e)),
                },
                RedirOp::Both { append, .. } => {
                    let f = match self.open_file(&target, false, true, *append, !append) {
                        Ok(f) => f,
                        Err(e) => return Ok(Err(e)),
                    };
                    io.out = Sink::File(Arc::clone(&f));
                    io.err = Sink::File(f);
                    continue;
                }
                RedirOp::DupOut(_) => match target.as_str() {
                    "-" => Sink::Null,
                    "1" => io.out.clone(),
                    "2" => io.err.clone(),
                    t if r.fd.is_none() && !t.bytes().all(|b| b.is_ascii_digit()) => {
                        let f = match self.open_file(t, false, true, false, true) {
                            Ok(f) => f,
                            Err(e) => return Ok(Err(e)),
                        };
                        io.out = Sink::File(Arc::clone(&f));
                        io.err = Sink::File(f);
                        continue;
                    }
                    t => return Ok(Err(format!("xsh: {t}: bad file descriptor"))),
                },
                _ => continue,
            };
            match fd {
                1 => io.out = sink,
                2 => io.err = sink,
                _ => {
                    return Ok(Err(format!(
                        "xsh: {fd}: only file descriptors 0, 1, and 2 can be redirected"
                    )));
                }
            }
        }
        Ok(Ok(io))
    }

    #[allow(clippy::fn_params_excessive_bools, reason = "mirrors OpenOptions")]
    fn open_file(
        &self,
        target: &str,
        read: bool,
        write: bool,
        append: bool,
        truncate: bool,
    ) -> std::result::Result<Arc<std::fs::File>, String> {
        let mut opts = std::fs::OpenOptions::new();
        opts.read(read);
        if write {
            opts.create(true);
            if append {
                opts.append(true);
            } else {
                opts.write(true).truncate(truncate);
            }
        }
        if let Some(mask) = self.state.umask() {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o666 & !mask);
        }
        opts.open(self.state.resolve(target))
            .map(Arc::new)
            .map_err(|e| format!("xsh: {target}: {e}"))
    }

    #[allow(clippy::too_many_lines)]
    fn run_builtin<'s>(
        &'s mut self,
        name: &'s str,
        args: &'s [String],
        io: &'s Io,
    ) -> BoxFut<'s, Ctl> {
        Box::pin(async move {
            match name {
                "eval" => {
                    let text = args.join(" ");
                    self.run_text(&text, io).await
                }
                "source" | "." => self.source(name, args, io).await,
                "exec" => self.exec_builtin(args, io).await,
                "command" => self.command_builtin(args, io).await,
                "builtin" => match args.split_first() {
                    Some((n, rest)) if builtins::is_builtin(n) => {
                        self.run_builtin(n, rest, io).await
                    }
                    Some((n, _)) => {
                        let out =
                            Output::fail(1, format!("xsh: builtin: {n}: not a shell builtin"));
                        self.emit(out, io)
                    }
                    None => {
                        self.state.set_last_status(0);
                        Ok(Ctl::Normal)
                    }
                },
                "read" => self.read_builtin(args, io).await,
                "break" | "continue" => {
                    let n = match args.first().map(|a| a.parse::<u32>()) {
                        None => 1,
                        Some(Ok(n)) if n >= 1 => n,
                        Some(_) => {
                            let out = Output::fail(
                                1,
                                format!("{name}: loop count must be a positive number"),
                            );
                            return self.emit(out, io);
                        }
                    };
                    if self.loops == 0 {
                        let out = Output {
                            stderr: format!("xsh: {name}: only meaningful in a loop\n"),
                            ..Output::default()
                        };
                        return self.emit(out, io);
                    }
                    self.state.set_last_status(0);
                    let n = n.min(self.loops);
                    Ok(if name == "break" {
                        Ctl::Break(n)
                    } else {
                        Ctl::Continue(n)
                    })
                }
                "return" => {
                    if self.funcs == 0 && self.sources == 0 {
                        let out = Output::fail(
                            2,
                            "return: can only return from a function or sourced script",
                        );
                        return self.emit(out, io);
                    }
                    let status = match args.first().map(|a| a.parse::<i32>()) {
                        None => self.state.last_status(),
                        Some(Ok(n)) => n & 0xff,
                        Some(Err(_)) => {
                            let out = Output::fail(2, "return: numeric argument required");
                            self.emit(out, io)?;
                            2
                        }
                    };
                    self.state.set_last_status(status);
                    Ok(Ctl::Return)
                }
                "local" => {
                    let mut out = Output::default();
                    if self.funcs == 0 {
                        out.error(1, "local: can only be used in a function");
                        return self.emit(out, io);
                    }
                    for a in args {
                        let (n, v) = a
                            .split_once('=')
                            .map_or((a.as_str(), None), |(n, v)| (n, Some(v)));
                        if !parser::is_var_name(n) {
                            out.error(1, format!("local: `{a}`: not a valid identifier"));
                            continue;
                        }
                        let keep = v
                            .is_none()
                            .then(|| self.state.var(n).map(str::to_owned))
                            .flatten();
                        self.state.make_local(n);
                        match v.map(str::to_owned).or(keep) {
                            Some(v) => self.state.set_var(n, v),
                            None => self.state.unset_var(n),
                        }
                    }
                    self.emit(out, io)
                }
                _ => {
                    let out = builtins::run(name, args, self.state, self.jobs).await;
                    self.emit(out, io)
                }
            }
        })
    }

    async fn source(&mut self, name: &str, args: &[String], io: &Io) -> Result<Ctl> {
        let Some((file, rest)) = args.split_first() else {
            let out = Output::fail(2, format!("{name}: filename argument required"));
            return self.emit(out, io);
        };
        let path = if file.contains('/') {
            self.state.resolve(file)
        } else {
            let path_var = self.state.var("PATH").unwrap_or_default().to_owned();
            std::env::split_paths(&path_var)
                .map(|d| d.join(file))
                .find(|p| p.is_file())
                .unwrap_or_else(|| self.state.resolve(file))
        };
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) => {
                let out = Output::fail(1, format!("xsh: {name}: {file}: {e}"));
                return self.emit(out, io);
            }
        };
        let saved = (!rest.is_empty()).then(|| self.state.replace_positional(rest.to_vec()));
        self.sources += 1;
        let res = self.run_script(&text, io).await;
        self.sources -= 1;
        if let Some(old) = saved {
            self.state.replace_positional(old);
        }
        Ok(match res? {
            Ctl::Return => Ctl::Normal,
            other => other,
        })
    }

    /// Runs a script one complete command at a time, so aliases defined on
    /// one line apply to the next.
    pub(crate) async fn run_script(&mut self, text: &str, io: &Io) -> Result<Ctl> {
        let mut buf = String::new();
        self.state.set_last_status(0);
        for line in text.lines() {
            buf.push_str(line);
            buf.push('\n');
            match parser::parse_program(&buf, self.state.aliases()) {
                Ok(list) => {
                    buf.clear();
                    let ctl = self.run_list(&list, io).await?;
                    if ctl != Ctl::Normal && ctl != Ctl::Abort {
                        return Ok(ctl);
                    }
                }
                Err(crate::Error::Incomplete) => {}
                Err(e) => {
                    buf.clear();
                    self.write_err(io, &format!("xsh: {e}"))?;
                    self.state.set_last_status(2);
                }
            }
        }
        if !buf.trim().is_empty() {
            self.write_err(io, "xsh: syntax error: unexpected end of file")?;
            self.state.set_last_status(2);
        }
        Ok(Ctl::Normal)
    }

    async fn exec_builtin(&mut self, args: &[String], io: &Io) -> Result<Ctl> {
        let Some((prog, rest)) = args.split_first() else {
            self.state.set_last_status(0);
            return Ok(Ctl::Normal);
        };
        if let Some(cmd) = self.state.trap("EXIT").map(str::to_owned) {
            self.state.traps_mut().remove("EXIT");
            self.run_text(&cmd, io).await?;
        }
        let err = std::process::Command::new(prog)
            .args(rest)
            .env_clear()
            .envs(self.state.env_vars())
            .current_dir(self.state.cwd())
            .exec();
        let status = if err.kind() == ErrorKind::NotFound {
            127
        } else {
            126
        };
        let out = Output::fail(status, format!("xsh: exec: {prog}: {err}"));
        self.emit(out, io)
    }

    async fn command_builtin(&mut self, args: &[String], io: &Io) -> Result<Ctl> {
        let mut i = 0;
        let (mut v, mut big_v) = (false, false);
        while let Some(a) = args.get(i) {
            match a.as_str() {
                "-v" => v = true,
                "-V" => big_v = true,
                "-p" => {}
                "--" => {
                    i += 1;
                    break;
                }
                _ => break,
            }
            i += 1;
        }
        let rest = args.get(i..).unwrap_or_default();
        if v || big_v {
            let mut out = Output::default();
            for name in rest {
                let line = if let Some(a) = self.state.aliases().get(name.as_str()) {
                    if big_v {
                        format!("{name} is aliased to `{a}'")
                    } else {
                        format!("alias {name}={}", builtins::quote(a))
                    }
                } else if builtins::KEYWORDS.contains(&name.as_str()) {
                    if big_v {
                        format!("{name} is a shell keyword")
                    } else {
                        name.clone()
                    }
                } else if self.state.functions().contains_key(name.as_str()) {
                    if big_v {
                        format!("{name} is a function")
                    } else {
                        name.clone()
                    }
                } else if builtins::is_builtin(name) {
                    if big_v {
                        format!("{name} is a shell builtin")
                    } else {
                        name.clone()
                    }
                } else if let Some(p) = builtins::find_programs(name, self.state).first() {
                    if big_v {
                        format!("{name} is {}", p.display())
                    } else {
                        p.display().to_string()
                    }
                } else {
                    if big_v {
                        let _ = std::fmt::Write::write_fmt(
                            &mut out.stderr,
                            format_args!("xsh: command: {name}: not found\n"),
                        );
                    }
                    out.status = 1;
                    continue;
                };
                out.stdout.push_str(&line);
                out.stdout.push('\n');
            }
            return self.emit(out, io);
        }
        let Some((prog, rest)) = rest.split_first() else {
            self.state.set_last_status(0);
            return Ok(Ctl::Normal);
        };
        if builtins::is_builtin(prog) {
            return self.run_builtin(prog, rest, io).await;
        }
        match self.spawn(prog, rest, &[], io)? {
            Ok(sp) => {
                let code = self.finish(sp).await?;
                self.state.set_last_status(code);
            }
            Err(status) => self.state.set_last_status(status),
        }
        Ok(Ctl::Normal)
    }

    async fn read_record(&mut self, input: &Input, delim: u8) -> Result<(Vec<u8>, bool)> {
        let mut out = Vec::new();
        match input {
            Input::Null => Ok((out, true)),
            Input::Buf(b) => {
                let Ok(mut g) = b.lock() else {
                    return Ok((out, true));
                };
                let rest = g.data.get(g.pos..).unwrap_or_default();
                if let Some(i) = rest.iter().position(|c| *c == delim) {
                    out.extend_from_slice(rest.get(..i).unwrap_or_default());
                    g.pos += i + 1;
                    Ok((out, false))
                } else {
                    out.extend_from_slice(rest);
                    g.pos = g.data.len();
                    Ok((out, true))
                }
            }
            Input::File(f) => {
                let mut r: &std::fs::File = f;
                let mut byte = [0u8; 1];
                loop {
                    match r.read(&mut byte)? {
                        0 => return Ok((out, true)),
                        _ if byte[0] == delim => return Ok((out, false)),
                        _ => out.push(byte[0]),
                    }
                }
            }
            Input::Inherit => {
                let res = tokio::task::spawn_blocking(move || {
                    let mut data = Vec::new();
                    let mut byte = [0u8; 1];
                    let mut stdin = io::stdin().lock();
                    loop {
                        match stdin.read(&mut byte) {
                            Ok(0) | Err(_) => return (data, true),
                            Ok(_) if byte[0] == delim => return (data, false),
                            Ok(_) => data.push(byte[0]),
                        }
                    }
                })
                .await;
                Ok(res.unwrap_or((out, true)))
            }
            Input::Pipe(_) => {
                let drained = drain(input).await;
                return Box::pin(self.read_record(&drained, delim)).await;
            }
        }
    }

    async fn read_builtin(&mut self, args: &[String], io: &Io) -> Result<Ctl> {
        let (mut raw, mut prompt, mut delim) = (false, None, b'\n');
        let mut names = Vec::new();
        let mut i = 0;
        while let Some(a) = args.get(i) {
            i += 1;
            if !names.is_empty() || !a.starts_with('-') || a == "-" {
                names.push(a.clone());
                continue;
            }
            if a == "--" {
                names.extend(args.get(i..).unwrap_or_default().iter().cloned());
                break;
            }
            let flags: Vec<char> = a.chars().skip(1).collect();
            for (k, c) in flags.iter().enumerate() {
                match c {
                    'r' => raw = true,
                    's' => {}
                    'p' | 'd' => {
                        let tail: String = flags.get(k + 1..).unwrap_or_default().iter().collect();
                        let value = if tail.is_empty() {
                            i += 1;
                            args.get(i - 1).cloned().unwrap_or_default()
                        } else {
                            tail
                        };
                        if *c == 'p' {
                            prompt = Some(value);
                        } else {
                            delim = value.bytes().next().unwrap_or(0);
                        }
                        break;
                    }
                    _ => {
                        let out = Output::fail(2, format!("read: -{c}: invalid option"));
                        return self.emit(out, io);
                    }
                }
            }
        }
        if let Some(bad) = names.iter().find(|n| !parser::is_var_name(n)) {
            let out = Output::fail(1, format!("read: `{bad}': not a valid identifier"));
            return self.emit(out, io);
        }
        if let Some(p) = prompt {
            self.write_sink(&io.err, p.as_bytes())?;
        }
        let input = drain(&io.input).await;
        let mut line = Vec::new();
        let mut eof;
        loop {
            let (chunk, end) = self.read_record(&input, delim).await?;
            eof = end;
            if raw {
                line.extend(chunk);
                break;
            }
            let mut it = chunk.into_iter().peekable();
            let mut cont = false;
            while let Some(b) = it.next() {
                if b == b'\\' {
                    match it.next() {
                        Some(n) => line.push(n),
                        None => cont = true,
                    }
                } else {
                    line.push(b);
                }
            }
            if !cont || eof {
                break;
            }
        }
        let text = String::from_utf8_lossy(&line).into_owned();
        let no_data = eof && text.is_empty();
        if names.is_empty() {
            self.state.set_var("REPLY", text);
        } else {
            let ifs = self.state.var("IFS").unwrap_or(" \t\n").to_owned();
            let fields = split_fields(&text, &ifs, names.len());
            for (k, name) in names.iter().enumerate() {
                self.state
                    .set_var(name.clone(), fields.get(k).cloned().unwrap_or_default());
            }
        }
        self.state.set_last_status(i32::from(eof || no_data));
        Ok(Ctl::Normal)
    }
}
