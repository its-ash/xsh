//! Background job tracking.

use std::io;
use std::sync::{Arc, Mutex};

use tokio::process::Child;
use tokio::task::JoinHandle;

use crate::exec::exit_code;
use crate::state::ForegroundHandle;

/// Lifecycle state of a background job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// At least one process is still running.
    Running,
    /// Stopped by `Ctrl-Z`; `fg` or `bg` resumes it.
    Stopped,
    /// All processes finished; holds the exit status of the last one.
    Done(i32),
}

/// Snapshot of a background job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobInfo {
    /// Job number, referenced as `%N`.
    pub id: usize,
    /// Command line that started the job.
    pub command: String,
    /// Current state.
    pub state: JobState,
}

#[derive(Debug)]
enum Kind {
    Child(Child),
    Task {
        handle: JoinHandle<i32>,
        result: Arc<Mutex<Option<i32>>>,
    },
}

#[derive(Debug)]
struct Proc {
    kind: Kind,
    status: Option<i32>,
}

#[derive(Debug)]
struct Job {
    id: usize,
    command: String,
    procs: Vec<Proc>,
    stopped: bool,
}

impl Job {
    fn poll(&mut self) {
        for p in self.procs.iter_mut().filter(|p| p.status.is_none()) {
            match &mut p.kind {
                Kind::Child(child) => match child.try_wait() {
                    Ok(Some(status)) => p.status = Some(exit_code(status)),
                    Ok(None) => {}
                    Err(_) => p.status = Some(1),
                },
                Kind::Task { handle, result } => {
                    if let Some(code) = result.lock().ok().and_then(|g| *g) {
                        p.status = Some(code);
                    } else if handle.is_finished() {
                        p.status = Some(1);
                    }
                }
            }
        }
    }

    fn state(&self) -> JobState {
        if self.procs.iter().all(|p| p.status.is_some()) {
            JobState::Done(self.procs.last().and_then(|p| p.status).unwrap_or(0))
        } else if self.stopped {
            JobState::Stopped
        } else {
            JobState::Running
        }
    }

    fn info(&self) -> JobInfo {
        JobInfo {
            id: self.id,
            command: self.command.clone(),
            state: self.state(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct JobTable {
    jobs: Vec<Job>,
    /// The job that the processes of the current command join when
    /// `Ctrl-Z` stops them (one job per pipeline).
    stop_group: Option<usize>,
}

impl JobTable {
    fn next_id(&self) -> usize {
        self.jobs.iter().map(|j| j.id).max().unwrap_or(0) + 1
    }

    pub(crate) fn add(&mut self, command: &str, children: Vec<Child>) -> (usize, Vec<u32>) {
        let id = self.next_id();
        let pids = children.iter().filter_map(Child::id).collect();
        self.jobs.push(Job {
            id,
            command: command.to_owned(),
            procs: children
                .into_iter()
                .map(|child| Proc {
                    kind: Kind::Child(child),
                    status: None,
                })
                .collect(),
            stopped: false,
        });
        (id, pids)
    }

    /// Starts a new foreground command: the next stopped process makes a
    /// new job.
    pub(crate) fn end_stop_group(&mut self) {
        self.stop_group = None;
    }

    /// Keeps a process that `Ctrl-Z` stopped. Returns the job id and whether
    /// the job is new (so the caller reports it once).
    pub(crate) fn add_stopped(&mut self, command: &str, child: Child) -> (usize, bool) {
        let proc = Proc {
            kind: Kind::Child(child),
            status: None,
        };
        if let Some(job) = self
            .stop_group
            .and_then(|id| self.jobs.iter_mut().find(|j| j.id == id))
        {
            job.procs.push(proc);
            return (job.id, false);
        }
        let id = self.next_id();
        self.jobs.push(Job {
            id,
            command: command.to_owned(),
            procs: vec![proc],
            stopped: true,
        });
        self.stop_group = Some(id);
        (id, true)
    }

    /// Marks a job as running again (after `SIGCONT`).
    pub(crate) fn set_running(&mut self, id: usize) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            job.stopped = false;
        }
    }

    /// Adds a job that runs inside the shell process as a task. The task
    /// stores its status in `result` when it ends.
    pub(crate) fn add_task(
        &mut self,
        command: &str,
        handle: JoinHandle<i32>,
        result: Arc<Mutex<Option<i32>>>,
    ) -> usize {
        let id = self.next_id();
        self.jobs.push(Job {
            id,
            command: command.to_owned(),
            procs: vec![Proc {
                kind: Kind::Task { handle, result },
                status: None,
            }],
            stopped: false,
        });
        id
    }

    pub(crate) fn list(&mut self) -> Vec<JobInfo> {
        self.jobs.iter_mut().for_each(Job::poll);
        self.jobs.iter().map(Job::info).collect()
    }

    pub(crate) fn reap(&mut self) -> Vec<JobInfo> {
        self.jobs.iter_mut().for_each(Job::poll);
        let (done, running): (Vec<_>, Vec<_>) = std::mem::take(&mut self.jobs)
            .into_iter()
            .partition(|j| matches!(j.state(), JobState::Done(_)));
        self.jobs = running;
        done.iter().map(Job::info).collect()
    }

    /// Jobs that were running or stopped when last checked.
    pub(crate) fn active(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| !matches!(j.state(), JobState::Done(_)))
            .count()
    }

    pub(crate) fn ids(&self) -> Vec<usize> {
        self.jobs.iter().map(|j| j.id).collect()
    }

    pub(crate) fn command(&self, id: usize) -> Option<String> {
        self.jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| j.command.clone())
    }

    pub(crate) fn resolve(&self, spec: Option<&str>) -> Option<usize> {
        match spec {
            None | Some("%" | "%%" | "%+") => self.jobs.last().map(|j| j.id),
            Some("%-") => self
                .jobs
                .len()
                .checked_sub(2)
                .and_then(|i| self.jobs.get(i))
                .map(|j| j.id),
            Some(s) => {
                let id = s.strip_prefix('%').unwrap_or(s).parse().ok()?;
                self.jobs.iter().any(|j| j.id == id).then_some(id)
            }
        }
    }

    /// The job that holds process `pid`.
    pub(crate) fn by_pid(&self, pid: u32) -> Option<usize> {
        self.jobs
            .iter()
            .find(|j| {
                j.procs
                    .iter()
                    .any(|p| matches!(&p.kind, Kind::Child(c) if c.id() == Some(pid)))
            })
            .map(|j| j.id)
    }

    /// Removes a job from the table without stopping it.
    pub(crate) fn remove(&mut self, id: usize) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|j| j.id != id);
        self.jobs.len() != before
    }

    pub(crate) fn pids(&self, id: usize) -> Vec<u32> {
        self.jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| {
                j.procs
                    .iter()
                    .filter(|p| p.status.is_none())
                    .filter_map(|p| match &p.kind {
                        Kind::Child(c) => c.id(),
                        Kind::Task { .. } => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Stops the in-process tasks of a job; returns whether it had any.
    pub(crate) fn abort_tasks(&mut self, id: usize) -> bool {
        let mut any = false;
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            for p in job.procs.iter_mut().filter(|p| p.status.is_none()) {
                if let Kind::Task { handle, .. } = &p.kind {
                    handle.abort();
                    p.status = Some(143);
                    any = true;
                }
            }
        }
        any
    }

    /// Waits for a job in the foreground. When `Ctrl-Z` stops it again
    /// (reported through `fg`), the job goes back to the table as stopped
    /// and the status is 148.
    pub(crate) async fn wait(
        &mut self,
        id: usize,
        fg: &ForegroundHandle,
    ) -> io::Result<Option<(String, i32)>> {
        let Some(pos) = self.jobs.iter().position(|j| j.id == id) else {
            return Ok(None);
        };
        let mut job = self.jobs.remove(pos);
        job.stopped = false;
        fg.clear_stop();
        fg.set(
            job.procs
                .iter()
                .filter(|p| p.status.is_none())
                .filter_map(|p| match &p.kind {
                    Kind::Child(c) => c.id(),
                    Kind::Task { .. } => None,
                })
                .collect(),
        );
        let mut status = 0;
        for i in 0..job.procs.len() {
            let Some(p) = job.procs.get_mut(i) else {
                break;
            };
            let code = match (p.status, &mut p.kind) {
                (Some(s), _) => Some(s),
                (None, Kind::Child(c)) => tokio::select! {
                    s = c.wait() => Some(exit_code(s?)),
                    () = fg.stopped() => None,
                },
                (None, Kind::Task { handle, .. }) => Some(handle.await.unwrap_or(1)),
            };
            let Some(code) = code else {
                fg.set(Vec::new());
                job.stopped = true;
                let command = job.command.clone();
                self.jobs.push(job);
                return Ok(Some((command, 148)));
            };
            p.status = Some(code);
            status = code;
        }
        fg.set(Vec::new());
        Ok(Some((job.command, status)))
    }
}
