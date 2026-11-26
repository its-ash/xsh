//! The multiplexer server: owns a [`SessionManager`] and serves clients over a
//! Unix socket.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::net::UnixListener;
/// A connection to a multiplexer server. Use `into_split` to read and write
/// it from different tasks.
pub use tokio::net::UnixStream as Connection;
use tokio::sync::mpsc::{self, Sender, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::pane::{PaneCommand, PaneEvent};
use crate::proto::{ClientMsg, ServerMsg, read_frame, write_frame};
use crate::session::SessionManager;
use crate::{Error, Result};

const CLIENT_QUEUE: usize = 256;
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

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

struct ClientHandle {
    tx: Sender<ServerMsg>,
    writer: JoinHandle<()>,
}

enum Event {
    Connected(u64, ClientHandle),
    Msg(u64, ClientMsg),
    Gone(u64),
}

async fn bind_socket(dir: &Path) -> Result<(PathBuf, UnixListener)> {
    std::fs::create_dir_all(dir)?;
    restrict_dir(dir)?;
    let path = socket_path(dir);
    if path.exists() {
        if Connection::connect(&path).await.is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("multiplexer server already running at {}", path.display()),
            )
            .into());
        }
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(e.into());
        }
    }
    let listener = UnixListener::bind(&path)?;
    restrict_socket(&path)?;
    Ok((path, listener))
}

/// Runs the server until the last session ends or a termination signal arrives.
///
/// # Errors
///
/// Fails if another server is already listening on the socket or if the socket
/// cannot be created.
pub async fn serve(dir: &Path, command: PaneCommand) -> Result<()> {
    let (path, listener) = bind_socket(dir).await?;
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Event>();
    let (pane_tx, mut pane_rx) = mpsc::unbounded_channel::<PaneEvent>();
    let accept_tx = ev_tx.clone();
    let accept_task = tokio::spawn(async move {
        let mut next = 0u64;
        loop {
            match listener.accept().await {
                Ok((conn, _)) => {
                    next += 1;
                    spawn_client(next, conn, &accept_tx);
                }
                // Transient failures (e.g. EMFILE) must not stop the server
                // from accepting clients for the rest of its life.
                Err(_) => tokio::time::sleep(ACCEPT_BACKOFF).await,
            }
        }
    });

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    let mut mgr = SessionManager::new(command, pane_tx, (80, 24));
    let mut clients: HashMap<u64, ClientHandle> = HashMap::new();
    let mut attached: Option<u64> = None;
    let mut had_session = false;
    loop {
        tokio::select! {
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
            Some(ev) = ev_rx.recv() => match ev {
                Event::Connected(id, handle) => { clients.insert(id, handle); }
                Event::Gone(id) => {
                    clients.remove(&id);
                    if attached == Some(id) {
                        attached = None;
                        let _ = mgr.detach();
                    }
                }
                Event::Msg(id, msg) => {
                    let tx = clients.get(&id).map(|c| c.tx.clone());
                    let Some(tx) = tx else { continue };
                    handle(&mut mgr, &clients, &mut attached, id, &tx, msg);
                    if !mgr.is_empty() {
                        had_session = true;
                    }
                }
            },
            Some(ev) = pane_rx.recv() => {
                let out = attached.and_then(|c| clients.get(&c)).map(|c| &c.tx);
                match ev {
                    PaneEvent::Output(pane, bytes) => {
                        if let Some(tx) = out.filter(|_| mgr.is_visible(pane)) {
                            let _ = tx.try_send(ServerMsg::Output(pane, bytes));
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
                        if mgr.attached().is_none() {
                            // `send_view` told the client its session exited.
                            attached = None;
                        }
                    }
                }
            }
            else => break,
        }
        if had_session && mgr.is_empty() {
            break;
        }
    }
    accept_task.abort();
    // Drop client senders so writer tasks finish flushing queued replies before
    // the server exits.
    let writers: Vec<JoinHandle<()>> = clients.into_values().map(|c| c.writer).collect();
    let _ = tokio::time::timeout(FLUSH_TIMEOUT, async {
        for w in writers {
            let _ = w.await;
        }
    })
    .await;
    let _ = std::fs::remove_file(&path);
    Ok(())
}

fn restrict_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

fn restrict_socket(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

fn spawn_client(id: u64, conn: Connection, events: &UnboundedSender<Event>) {
    let (mut rx, mut tx) = conn.into_split();
    let (out_tx, mut out_rx) = mpsc::channel::<ServerMsg>(CLIENT_QUEUE);
    let reader_events = events.clone();
    let writer_events = events.clone();
    // The reader waits until `Connected` is queued, so the server never sees a
    // message from a client it does not know yet.
    let (start_tx, start_rx) = oneshot::channel::<()>();
    let reader = tokio::spawn(async move {
        if start_rx.await.is_err() {
            return;
        }
        while let Ok(Some(msg)) = read_frame::<_, ClientMsg>(&mut rx).await {
            if reader_events.send(Event::Msg(id, msg)).is_err() {
                return;
            }
        }
        let _ = reader_events.send(Event::Gone(id));
    });
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let last = matches!(msg, ServerMsg::Detached | ServerMsg::Exited);
            if write_frame(&mut tx, &msg).await.is_err() || last {
                break;
            }
        }
        reader.abort();
        let _ = writer_events.send(Event::Gone(id));
    });
    let _ = events.send(Event::Connected(id, ClientHandle { tx: out_tx, writer }));
    let _ = start_tx.send(());
}

fn send_view(mgr: &SessionManager, tx: &Sender<ServerMsg>, snapshots: bool) {
    let Some(view) = mgr.view() else {
        let _ = tx.try_send(ServerMsg::Exited);
        return;
    };
    let ids: Vec<_> = view.panes.iter().map(|p| p.id).collect();
    let _ = tx.try_send(ServerMsg::View(view));
    if snapshots {
        for id in ids {
            if let Some(p) = mgr.pane(id) {
                let _ = tx.try_send(ServerMsg::Snapshot(id, p.snapshot()));
            }
        }
    }
}

fn handle(
    mgr: &mut SessionManager,
    clients: &HashMap<u64, ClientHandle>,
    attached: &mut Option<u64>,
    id: u64,
    tx: &Sender<ServerMsg>,
    msg: ClientMsg,
) {
    let mine = *attached == Some(id);
    let reply = |r: Result<()>| {
        let _ = tx.try_send(match r {
            Ok(()) => ServerMsg::Ok,
            Err(e) => ServerMsg::Error(e.to_string()),
        });
    };
    match msg {
        ClientMsg::List => {
            let _ = tx.try_send(ServerMsg::Sessions(mgr.list_sessions()));
        }
        ClientMsg::Kill(name) => {
            let killed = mgr
                .find(&name)
                .ok_or(Error::NoSession(name))
                .and_then(|s| mgr.kill_session(s));
            reply(killed);
            // The attached session is gone: tell its client instead of
            // leaving it attached to nothing.
            if mgr.attached().is_none()
                && let Some(c) = attached.take().and_then(|c| clients.get(&c))
            {
                let _ = c.tx.try_send(ServerMsg::Exited);
            }
        }
        ClientMsg::Attach {
            session,
            create,
            cols,
            rows,
            cwd,
        } => {
            let sid = match mgr.find(&session) {
                Some(s) => Ok(s),
                None if create => mgr.create_session_in(&session, cwd.map(PathBuf::from)),
                None => Err(Error::NoSession(session)),
            };
            let sid = match sid {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx.try_send(ServerMsg::Error(e.to_string()));
                    return;
                }
            };
            if let Some(prev) = attached.filter(|p| *p != id).and_then(|p| clients.get(&p)) {
                let _ = prev.tx.try_send(ServerMsg::Detached);
            }
            let _ = mgr.resize(cols, rows);
            match mgr.attach(sid) {
                Ok(()) => {
                    *attached = Some(id);
                    send_view(mgr, tx, true);
                }
                Err(e) => {
                    let _ = tx.try_send(ServerMsg::Error(e.to_string()));
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
                let _ = tx.try_send(if cmd == crate::MuxCommand::Detach {
                    ServerMsg::Detached
                } else {
                    ServerMsg::Exited
                });
            }
            Ok(()) => send_view(mgr, tx, true),
            Err(e) => {
                let _ = tx.try_send(ServerMsg::Error(e.to_string()));
            }
        },
        ClientMsg::Input(_) | ClientMsg::Resize { .. } | ClientMsg::Command(_) => {
            let _ = tx.try_send(ServerMsg::Error(Error::NotAttached.to_string()));
        }
    }
}
