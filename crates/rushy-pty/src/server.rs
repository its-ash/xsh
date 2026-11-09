//! The multiplexer server: owns a [`SessionManager`] and serves clients over a
//! Unix socket.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;
/// A connection to a multiplexer server. Use `into_split` to read and write
/// it from different tasks.
pub use tokio::net::UnixStream as Connection;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::pane::{PaneCommand, PaneEvent};
use crate::proto::{ClientMsg, ServerMsg, read_frame, write_frame};
use crate::session::SessionManager;
use crate::{Error, Result};

/// Socket file path.
#[must_use]
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("default.sock")
}

/// Connects to a running server.
///
/// # Errors
///
/// Fails if no server is listening.
pub async fn connect(dir: &Path) -> Result<Connection> {
    Ok(Connection::connect(socket_path(dir)).await?)
}

enum Event {
    Connected(u64, UnboundedSender<ServerMsg>),
    Msg(u64, ClientMsg),
    Gone(u64),
}

/// Runs the server until the last session ends.
///
/// # Errors
///
/// Fails if the socket cannot be created.
pub async fn serve(dir: &Path, command: PaneCommand) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    restrict_dir(dir)?;
    let path = socket_path(dir);
    // A stale socket file from an earlier server blocks the bind.
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return Err(e.into());
    }
    let listener = UnixListener::bind(&path)?;
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Event>();
    let (pane_tx, mut pane_rx) = mpsc::unbounded_channel::<PaneEvent>();
    let accept_tx = ev_tx.clone();
    tokio::spawn(async move {
        let mut next = 0u64;
        while let Ok((conn, _)) = listener.accept().await {
            next += 1;
            spawn_client(next, conn, accept_tx.clone());
        }
    });

    let mut mgr = SessionManager::new(command.clone(), pane_tx, (80, 24));
    let mut clients: HashMap<u64, UnboundedSender<ServerMsg>> = HashMap::new();
    let mut attached: Option<u64> = None;
    let mut served = false;
    loop {
        tokio::select! {
            Some(ev) = ev_rx.recv() => match ev {
                Event::Connected(id, tx) => { clients.insert(id, tx); }
                Event::Gone(id) => {
                    clients.remove(&id);
                    if attached == Some(id) {
                        attached = None;
                        let _ = mgr.detach();
                    }
                }
                Event::Msg(id, msg) => {
                    served = true;
                    let tx = clients.get(&id).cloned();
                    let Some(tx) = tx else { continue };
                    handle(&mut mgr, &command, &clients, &mut attached, id, &tx, msg);
                }
            },
            Some(ev) = pane_rx.recv() => {
                let out = attached.and_then(|c| clients.get(&c));
                match ev {
                    PaneEvent::Output(pane, bytes) => {
                        if let Some(tx) = out.filter(|_| mgr.visible_panes().contains(&pane)) {
                            let _ = tx.send(ServerMsg::Output(pane, bytes));
                        }
                    }
                    PaneEvent::Exited(pane) => {
                        if mgr.pane(pane).is_none() {
                            continue;
                        }
                        let ours = mgr.session_of(pane) == mgr.attached();
                        mgr.remove_pane(pane);
                        if let Some(tx) = out.filter(|_| ours) {
                            send_view(&mgr, tx, true);
                        }
                    }
                }
            }
            else => break,
        }
        if served && mgr.is_empty() {
            break;
        }
    }
    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn restrict_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn spawn_client(id: u64, conn: Connection, events: UnboundedSender<Event>) {
    let (mut rx, mut tx) = conn.into_split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMsg>();
    if events.send(Event::Connected(id, out_tx)).is_err() {
        return;
    }
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let last = matches!(msg, ServerMsg::Detached | ServerMsg::Exited);
            if write_frame(&mut tx, &msg).await.is_err() || last {
                break;
            }
        }
    });
    tokio::spawn(async move {
        while let Ok(Some(msg)) = read_frame::<_, ClientMsg>(&mut rx).await {
            if events.send(Event::Msg(id, msg)).is_err() {
                return;
            }
        }
        let _ = events.send(Event::Gone(id));
    });
}

fn send_view(mgr: &SessionManager, tx: &UnboundedSender<ServerMsg>, snapshots: bool) {
    let Some(view) = mgr.view() else {
        let _ = tx.send(ServerMsg::Exited);
        return;
    };
    let ids: Vec<_> = view.panes.iter().map(|p| p.id).collect();
    let _ = tx.send(ServerMsg::View(view));
    if snapshots {
        for id in ids {
            if let Some(p) = mgr.pane(id) {
                let _ = tx.send(ServerMsg::Snapshot(id, p.snapshot()));
            }
        }
    }
}

fn handle(
    mgr: &mut SessionManager,
    command: &PaneCommand,
    clients: &HashMap<u64, UnboundedSender<ServerMsg>>,
    attached: &mut Option<u64>,
    id: u64,
    tx: &UnboundedSender<ServerMsg>,
    msg: ClientMsg,
) {
    let mine = *attached == Some(id);
    let reply = |r: Result<()>| {
        let _ = tx.send(match r {
            Ok(()) => ServerMsg::Ok,
            Err(e) => ServerMsg::Error(e.to_string()),
        });
    };
    match msg {
        ClientMsg::List => {
            let _ = tx.send(ServerMsg::Sessions(mgr.list_sessions()));
        }
        ClientMsg::Kill(name) => reply(
            mgr.find(&name)
                .ok_or(Error::NoSession(name))
                .and_then(|s| mgr.kill_session(s)),
        ),
        ClientMsg::Attach {
            session,
            create,
            cols,
            rows,
            cwd,
        } => {
            let sid = match mgr.find(&session) {
                Some(s) => Ok(s),
                None if create => {
                    let mut cmd = command.clone();
                    cmd.cwd = cwd.map(PathBuf::from).or(cmd.cwd);
                    mgr.set_command(cmd);
                    mgr.create_session(&session)
                }
                None => Err(Error::NoSession(session)),
            };
            let sid = match sid {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error(e.to_string()));
                    return;
                }
            };
            if let Some(prev) = attached.filter(|p| *p != id).and_then(|p| clients.get(&p)) {
                let _ = prev.send(ServerMsg::Detached);
            }
            let _ = mgr.resize(cols, rows);
            match mgr.attach(sid) {
                Ok(()) => {
                    *attached = Some(id);
                    send_view(mgr, tx, true);
                }
                Err(e) => {
                    let _ = tx.send(ServerMsg::Error(e.to_string()));
                }
            }
        }
        ClientMsg::Input(bytes) if mine => {
            let _ = mgr.write_input(&bytes);
        }
        ClientMsg::Resize { cols, rows } if mine => {
            if mgr.resize(cols, rows).is_ok() {
                send_view(mgr, tx, false);
            }
        }
        ClientMsg::Command(cmd) if mine => match mgr.apply(cmd) {
            Ok(()) if mgr.attached().is_none() => {
                *attached = None;
                let _ = tx.send(if cmd == crate::MuxCommand::Detach {
                    ServerMsg::Detached
                } else {
                    ServerMsg::Exited
                });
            }
            Ok(()) => send_view(mgr, tx, true),
            Err(e) => {
                let _ = tx.send(ServerMsg::Error(e.to_string()));
            }
        },
        ClientMsg::Input(_) | ClientMsg::Resize { .. } | ClientMsg::Command(_) => {
            let _ = tx.send(ServerMsg::Error(Error::NotAttached.to_string()));
        }
    }
}
