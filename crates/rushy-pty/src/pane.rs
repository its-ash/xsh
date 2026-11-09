//! One PTY-backed pane: child process, reader, writer, and screen state.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::{Error, Result};

const SCROLLBACK: usize = 2_000;
const READ_CHUNK: usize = 8_192;

/// Pane identifier, unique per server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(pub u32);

impl std::fmt::Display for PaneId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// Something that happened in a pane, delivered on the manager's event channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneEvent {
    /// Bytes the child wrote to the terminal.
    Output(PaneId, Vec<u8>),
    /// The child exited or the PTY closed.
    Exited(PaneId),
}

/// The program each new pane runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCommand {
    /// Executable path.
    pub program: PathBuf,
    /// Arguments.
    pub args: Vec<String>,
    /// Working directory for new panes; `None` inherits the server's.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables.
    pub env: Vec<(String, String)>,
}

impl PaneCommand {
    /// Runs `program` with no arguments.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
        }
    }
}

fn pty_err(e: impl std::fmt::Display) -> Error {
    Error::Pty(e.to_string())
}

/// A pane: the PTY master, its child, and a terminal emulator tracking the screen.
///
/// A reader thread copies child output into the emulator and onto the event
/// channel; a writer thread drains input into the PTY so a slow child never
/// blocks the async runtime.
pub struct Pane {
    /// Pane id.
    pub id: PaneId,
    /// PTY master, used for resizing.
    pub pty: Box<dyn MasterPty + Send>,
    input: mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    screen: Arc<Mutex<vt100::Parser>>,
    size: (u16, u16),
}

impl std::fmt::Debug for Pane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pane")
            .field("id", &self.id)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Pane {
    /// Spawns `cmd` on a new PTY of `cols` x `rows`.
    ///
    /// # Errors
    ///
    /// Fails if the PTY cannot be opened or the program cannot start.
    pub fn spawn(
        id: PaneId,
        cmd: &PaneCommand,
        cols: u16,
        rows: u16,
        events: UnboundedSender<PaneEvent>,
    ) -> Result<Self> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(pty_err)?;
        let mut builder = CommandBuilder::new(&cmd.program);
        builder.args(&cmd.args);
        if let Some(cwd) = &cmd.cwd {
            builder.cwd(cwd);
        }
        builder.env("TERM", "xterm-256color");
        for (k, v) in &cmd.env {
            builder.env(k, v);
        }
        let mut child = pair.slave.spawn_command(builder).map_err(pty_err)?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(pty_err)?;
        let mut writer = pair.master.take_writer().map_err(pty_err)?;
        let screen = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));

        let (input, input_rx) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            while let Ok(bytes) = input_rx.recv() {
                if writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });

        let parser = Arc::clone(&screen);
        let out = events.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let chunk = buf.get(..n).unwrap_or_default().to_vec();
                        if let Ok(mut p) = parser.lock() {
                            p.process(&chunk);
                        }
                        if out.send(PaneEvent::Output(id, chunk)).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = out.send(PaneEvent::Exited(id));
        });

        std::thread::spawn(move || {
            let _ = child.wait();
            let _ = events.send(PaneEvent::Exited(id));
        });

        Ok(Self {
            id,
            pty: pair.master,
            input,
            killer,
            screen,
            size: (cols, rows),
        })
    }

    /// Queues bytes for the child's input.
    pub fn write(&self, bytes: &[u8]) {
        let _ = self.input.send(bytes.to_vec());
    }

    /// Resizes the PTY and the emulator; the child receives `SIGWINCH`.
    ///
    /// # Errors
    ///
    /// Fails if the PTY rejects the new size.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.size == (cols, rows) {
            return Ok(());
        }
        self.pty
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(pty_err)?;
        if let Ok(mut p) = self.screen.lock() {
            p.set_size(rows, cols);
        }
        self.size = (cols, rows);
        Ok(())
    }

    /// Current size as `(cols, rows)`.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// Escape sequences that redraw the current screen on a fresh terminal.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        self.screen
            .lock()
            .map(|p| p.screen().state_formatted())
            .unwrap_or_default()
    }

    /// Plain-text screen contents.
    #[must_use]
    pub fn contents(&self) -> String {
        self.screen
            .lock()
            .map(|p| p.screen().contents())
            .unwrap_or_default()
    }

    /// Kills the child process.
    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        self.kill();
    }
}
