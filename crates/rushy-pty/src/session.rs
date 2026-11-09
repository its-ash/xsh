//! Sessions, windows, and the manager that owns every pane.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::layout::{Axis, Dir, Layout, Rect, neighbor};
use crate::pane::{Pane, PaneCommand, PaneEvent, PaneId};
use crate::{Error, Result};

const RESIZE_STEP: u16 = 5;

/// Session identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub u32);

/// Summary of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Session id.
    pub id: SessionId,
    /// Session name.
    pub name: String,
    /// Number of windows.
    pub windows: usize,
    /// Number of panes across all windows.
    pub panes: usize,
    /// Whether a client is attached.
    pub attached: bool,
}

/// A window in the attached session, as shown in the status bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// 0-based index.
    pub index: usize,
    /// Whether this is the visible window.
    pub active: bool,
    /// Pane count.
    pub panes: usize,
}

/// A visible pane and where to draw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneView {
    /// Pane id.
    pub id: PaneId,
    /// Outer rectangle, including the border.
    pub rect: Rect,
    /// Whether the pane has input focus.
    pub focused: bool,
}

/// Everything a client needs to draw the attached session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    /// Attached session.
    pub session: SessionInfo,
    /// Windows of the session.
    pub windows: Vec<WindowInfo>,
    /// Panes of the active window.
    pub panes: Vec<PaneView>,
}

/// A tmux-style command applied to the attached session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MuxCommand {
    /// Split the focused pane side by side (`%`).
    SplitHorizontal,
    /// Split the focused pane top and bottom (`"`).
    SplitVertical,
    /// Move focus (`h/j/k/l`, arrows).
    Focus(Dir),
    /// Move the focused pane's divider (`Ctrl+arrows`).
    Resize(Dir),
    /// Open a window (`c`).
    NewWindow,
    /// Next window (`n`).
    NextWindow,
    /// Previous window (`p`).
    PrevWindow,
    /// Kill the focused pane (`x`).
    KillPane,
    /// Detach the client (`d`).
    Detach,
}

#[derive(Debug)]
struct Window {
    layout: Layout,
    focused: PaneId,
}

#[derive(Debug)]
struct Session {
    id: SessionId,
    name: String,
    windows: Vec<Window>,
    active: usize,
}

impl Session {
    fn window(&self) -> Option<&Window> {
        self.windows.get(self.active)
    }

    fn window_mut(&mut self) -> Option<&mut Window> {
        self.windows.get_mut(self.active)
    }
}

/// Owns every session, window, and pane on a server.
///
/// ```no_run
/// use rushy_pty::{PaneCommand, SessionManager};
/// # fn main() -> rushy_pty::Result<()> {
/// let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
/// let mut mgr = SessionManager::new(PaneCommand::new("/bin/sh"), tx, (80, 24));
/// let id = mgr.create_session("work")?;
/// mgr.attach(id)?;
/// assert_eq!(mgr.list_sessions().len(), 1);
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct SessionManager {
    command: PaneCommand,
    events: UnboundedSender<PaneEvent>,
    sessions: BTreeMap<SessionId, Session>,
    panes: BTreeMap<PaneId, Pane>,
    attached: Option<SessionId>,
    size: (u16, u16),
    next_session: u32,
    next_pane: u32,
}

impl SessionManager {
    /// A manager whose panes run `command`, reporting output on `events`.
    /// `size` is the pane area as `(cols, rows)`.
    #[must_use]
    pub fn new(command: PaneCommand, events: UnboundedSender<PaneEvent>, size: (u16, u16)) -> Self {
        Self {
            command,
            events,
            sessions: BTreeMap::new(),
            panes: BTreeMap::new(),
            attached: None,
            size,
            next_session: 0,
            next_pane: 0,
        }
    }

    /// Sets the program for new panes.
    pub fn set_command(&mut self, command: PaneCommand) {
        self.command = command;
    }

    fn area(&self) -> Rect {
        Rect::new(0, 0, self.size.0.max(4), self.size.1.max(3))
    }

    fn spawn_pane(&mut self, rect: Rect) -> Result<PaneId> {
        self.next_pane += 1;
        let id = PaneId(self.next_pane);
        let inner = rect.inner();
        let pane = Pane::spawn(
            id,
            &self.command,
            inner.width,
            inner.height,
            self.events.clone(),
        )?;
        self.panes.insert(id, pane);
        Ok(id)
    }

    /// Creates a session with one window and one pane.
    ///
    /// # Errors
    ///
    /// Fails if the name is taken or the pane cannot start.
    pub fn create_session(&mut self, name: &str) -> Result<SessionId> {
        if self.find(name).is_some() {
            return Err(Error::SessionExists(name.to_owned()));
        }
        let pane = self.spawn_pane(self.area())?;
        self.next_session += 1;
        let id = SessionId(self.next_session);
        let name = if name.is_empty() {
            self.next_session.to_string()
        } else {
            name.to_owned()
        };
        self.sessions.insert(
            id,
            Session {
                id,
                name,
                windows: vec![Window {
                    layout: Layout::Pane(pane),
                    focused: pane,
                }],
                active: 0,
            },
        );
        Ok(id)
    }

    /// Finds a session by name or numeric id.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<SessionId> {
        self.sessions
            .values()
            .find(|s| s.name == name || s.id.0.to_string() == name)
            .map(|s| s.id)
    }

    fn info(&self, s: &Session) -> SessionInfo {
        SessionInfo {
            id: s.id,
            name: s.name.clone(),
            windows: s.windows.len(),
            panes: s.windows.iter().map(|w| w.layout.panes().len()).sum(),
            attached: self.attached == Some(s.id),
        }
    }

    /// All sessions, oldest first.
    #[must_use]
    pub fn list_sessions(&self) -> Vec<SessionInfo> {
        self.sessions.values().map(|s| self.info(s)).collect()
    }

    /// Attaches to a session and fits its panes to the current size.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoSession`] for an unknown id.
    pub fn attach(&mut self, id: SessionId) -> Result<()> {
        if !self.sessions.contains_key(&id) {
            return Err(Error::NoSession(id.0.to_string()));
        }
        self.attached = Some(id);
        self.relayout()
    }

    /// Detaches; the session keeps running.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotAttached`] when nothing is attached.
    pub fn detach(&mut self) -> Result<()> {
        self.attached.take().map(|_| ()).ok_or(Error::NotAttached)
    }

    /// The attached session, if any.
    #[must_use]
    pub fn attached(&self) -> Option<SessionId> {
        self.attached
    }

    /// Kills a session and all of its panes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoSession`] for an unknown id.
    pub fn kill_session(&mut self, id: SessionId) -> Result<()> {
        let s = self
            .sessions
            .remove(&id)
            .ok_or_else(|| Error::NoSession(id.0.to_string()))?;
        for w in &s.windows {
            for p in w.layout.panes() {
                self.panes.remove(&p);
            }
        }
        if self.attached == Some(id) {
            self.attached = None;
        }
        Ok(())
    }

    /// Whether no sessions remain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Sets the pane area and resizes visible panes.
    ///
    /// # Errors
    ///
    /// Fails if a PTY rejects its new size.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.size = (cols, rows);
        self.relayout()
    }

    fn relayout(&mut self) -> Result<()> {
        let area = self.area();
        let Some(w) = self
            .attached
            .and_then(|id| self.sessions.get(&id))
            .and_then(Session::window)
        else {
            return Ok(());
        };
        for (id, rect) in w.layout.rects(area) {
            if let Some(p) = self.panes.get_mut(&id) {
                let inner = rect.inner();
                p.resize(inner.width, inner.height)?;
            }
        }
        Ok(())
    }

    /// Sends input to the focused pane of the attached session.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotAttached`] when nothing is attached.
    pub fn write_input(&self, bytes: &[u8]) -> Result<()> {
        let focused = self.focused().ok_or(Error::NotAttached)?;
        if let Some(p) = self.panes.get(&focused) {
            p.write(bytes);
        }
        Ok(())
    }

    /// Focused pane of the attached session.
    #[must_use]
    pub fn focused(&self) -> Option<PaneId> {
        self.attached
            .and_then(|id| self.sessions.get(&id))
            .and_then(Session::window)
            .map(|w| w.focused)
    }

    /// A pane by id.
    #[must_use]
    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.get(&id)
    }

    /// Session that owns `pane`.
    #[must_use]
    pub fn session_of(&self, pane: PaneId) -> Option<SessionId> {
        self.sessions
            .values()
            .find(|s| s.windows.iter().any(|w| w.layout.contains(pane)))
            .map(|s| s.id)
    }

    /// What the attached client should draw.
    #[must_use]
    pub fn view(&self) -> Option<View> {
        let s = self.sessions.get(&self.attached?)?;
        let w = s.window()?;
        Some(View {
            session: self.info(s),
            windows: s
                .windows
                .iter()
                .enumerate()
                .map(|(index, w)| WindowInfo {
                    index,
                    active: index == s.active,
                    panes: w.layout.panes().len(),
                })
                .collect(),
            panes: w
                .layout
                .rects(self.area())
                .into_iter()
                .map(|(id, rect)| PaneView {
                    id,
                    rect,
                    focused: id == w.focused,
                })
                .collect(),
        })
    }

    /// Applies a multiplexer command to the attached session.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotAttached`] when nothing is attached, or a PTY error
    /// when a new pane cannot start.
    pub fn apply(&mut self, cmd: MuxCommand) -> Result<()> {
        let sid = self.attached.ok_or(Error::NotAttached)?;
        let area = self.area();
        match cmd {
            MuxCommand::SplitHorizontal | MuxCommand::SplitVertical => {
                let axis = if cmd == MuxCommand::SplitHorizontal {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                let rect = self
                    .sessions
                    .get(&sid)
                    .and_then(Session::window)
                    .and_then(|w| {
                        w.layout
                            .rects(area)
                            .into_iter()
                            .find(|(id, _)| *id == w.focused)
                    })
                    .map_or(area, |(_, r)| r);
                let half = match axis {
                    Axis::Horizontal => Rect::new(0, 0, rect.width / 2, rect.height),
                    Axis::Vertical => Rect::new(0, 0, rect.width, rect.height / 2),
                };
                let new = self.spawn_pane(half)?;
                if let Some(w) = self.sessions.get_mut(&sid).and_then(Session::window_mut) {
                    w.layout.split(w.focused, axis, new);
                    w.focused = new;
                }
            }
            MuxCommand::Focus(dir) => {
                if let Some(w) = self.sessions.get_mut(&sid).and_then(Session::window_mut) {
                    if let Some(next) = neighbor(&w.layout.rects(area), w.focused, dir) {
                        w.focused = next;
                    }
                }
            }
            MuxCommand::Resize(dir) => {
                if let Some(w) = self.sessions.get_mut(&sid).and_then(Session::window_mut) {
                    let focused = w.focused;
                    w.layout.resize(focused, dir, RESIZE_STEP);
                }
            }
            MuxCommand::NewWindow => {
                let pane = self.spawn_pane(area)?;
                if let Some(s) = self.sessions.get_mut(&sid) {
                    s.windows.push(Window {
                        layout: Layout::Pane(pane),
                        focused: pane,
                    });
                    s.active = s.windows.len() - 1;
                }
            }
            MuxCommand::NextWindow | MuxCommand::PrevWindow => {
                if let Some(s) = self.sessions.get_mut(&sid) {
                    let n = s.windows.len().max(1);
                    s.active = if cmd == MuxCommand::NextWindow {
                        (s.active + 1) % n
                    } else {
                        (s.active + n - 1) % n
                    };
                }
            }
            MuxCommand::KillPane => {
                if let Some(id) = self.focused() {
                    self.remove_pane(id);
                }
            }
            MuxCommand::Detach => return self.detach(),
        }
        self.relayout()
    }

    /// Removes a pane (after its process exits or on `kill-pane`). Empty
    /// windows close, and a session with no windows ends.
    pub fn remove_pane(&mut self, id: PaneId) {
        self.panes.remove(&id);
        let Some(sid) = self.session_of(id) else {
            return;
        };
        let Some(s) = self.sessions.get_mut(&sid) else {
            return;
        };
        let mut windows = Vec::with_capacity(s.windows.len());
        for w in std::mem::take(&mut s.windows) {
            if !w.layout.contains(id) {
                windows.push(w);
                continue;
            }
            if let Some(layout) = w.layout.remove(id) {
                let focused = if w.focused == id {
                    layout.panes().first().copied().unwrap_or(w.focused)
                } else {
                    w.focused
                };
                windows.push(Window { layout, focused });
            }
        }
        s.windows = windows;
        s.active = s.active.min(s.windows.len().saturating_sub(1));
        if s.windows.is_empty() {
            self.sessions.remove(&sid);
            if self.attached == Some(sid) {
                self.attached = None;
            }
        }
        let _ = self.relayout();
    }

    /// Panes of the active window in the attached session.
    #[must_use]
    pub fn visible_panes(&self) -> Vec<PaneId> {
        self.attached
            .and_then(|id| self.sessions.get(&id))
            .and_then(Session::window)
            .map(|w| w.layout.panes())
            .unwrap_or_default()
    }
}
