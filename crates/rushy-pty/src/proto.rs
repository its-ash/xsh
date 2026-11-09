//! Client/server messages and length-prefixed JSON framing.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::pane::PaneId;
use crate::session::{MuxCommand, SessionInfo, View};
use crate::{Error, Result};

const MAX_FRAME: u32 = 16 * 1024 * 1024;

/// Messages a client sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// Attach to `session`, creating it when `create` is set.
    Attach {
        /// Session name.
        session: String,
        /// Create the session if it does not exist.
        create: bool,
        /// Pane area width.
        cols: u16,
        /// Pane area height.
        rows: u16,
        /// Working directory for a newly created session.
        cwd: Option<String>,
    },
    /// Keyboard input for the focused pane.
    Input(Vec<u8>),
    /// The client terminal changed size.
    Resize {
        /// Pane area width.
        cols: u16,
        /// Pane area height.
        rows: u16,
    },
    /// A prefix-key command.
    Command(MuxCommand),
    /// List sessions, then close.
    List,
    /// Kill a session by name.
    Kill(String),
}

/// Messages the server sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    /// Current layout of the attached session.
    View(View),
    /// Full redraw for one pane.
    Snapshot(PaneId, Vec<u8>),
    /// Incremental output for one pane.
    Output(PaneId, Vec<u8>),
    /// The client was detached.
    Detached,
    /// The attached session ended.
    Exited,
    /// Reply to [`ClientMsg::List`].
    Sessions(Vec<SessionInfo>),
    /// A request succeeded.
    Ok,
    /// A request failed.
    Error(String),
}

/// Writes one frame: a big-endian `u32` length, then JSON.
///
/// # Errors
///
/// Fails on serialization or write errors.
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> Result<()> {
    let body = serde_json::to_vec(msg)?;
    let len = u32::try_from(body.len()).map_err(|_| Error::Protocol("frame too large".into()))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// Reads one frame; `None` at a clean end of stream.
///
/// # Errors
///
/// Fails on oversized or malformed frames and read errors.
pub async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(r: &mut R) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len);
    if len > MAX_FRAME {
        return Err(Error::Protocol(format!(
            "frame of {len} bytes is too large"
        )));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    Ok(Some(serde_json::from_slice(&body)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip() -> Result<()> {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, &ClientMsg::Input(b"ls\r".to_vec())).await?;
        drop(a);
        let got: Option<ClientMsg> = read_frame(&mut b).await?;
        assert_eq!(got, Some(ClientMsg::Input(b"ls\r".to_vec())));
        let end: Option<ClientMsg> = read_frame(&mut b).await?;
        assert_eq!(end, None);
        Ok(())
    }
}
