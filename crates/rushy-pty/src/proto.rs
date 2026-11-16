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
/// Fails on serialization or write errors, and on frames larger than the
/// reader accepts (16 MiB), which would otherwise break the connection.
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> Result<()> {
    let body = serde_json::to_vec(msg)?;
    let len = u32::try_from(body.len())
        .ok()
        .filter(|&n| n <= MAX_FRAME)
        .ok_or_else(|| Error::Protocol(format!("frame of {} bytes is too large", body.len())))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// Reads one frame; `None` at a clean end of stream (between frames).
///
/// # Errors
///
/// Fails on oversized, truncated, or malformed frames and read errors.
pub async fn read_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(r: &mut R) -> Result<Option<T>> {
    let mut len = [0u8; 4];
    let mut got = 0;
    while got < len.len() {
        let n = r.read(len.get_mut(got..).unwrap_or_default()).await?;
        if n == 0 {
            if got == 0 {
                return Ok(None);
            }
            return Err(Error::Protocol("stream ended inside a frame header".into()));
        }
        got += n;
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

    async fn read_raw(bytes: &[u8]) -> Result<Option<ClientMsg>> {
        let (mut a, mut b) = tokio::io::duplex(1024);
        a.write_all(bytes).await?;
        drop(a);
        read_frame(&mut b).await
    }

    #[tokio::test]
    async fn bad_frames_are_errors() -> Result<()> {
        // Oversized length prefix: rejected before allocating.
        assert!(matches!(
            read_raw(&u32::MAX.to_be_bytes()).await,
            Err(Error::Protocol(_))
        ));
        // Header cut short.
        assert!(matches!(read_raw(&[0, 0]).await, Err(Error::Protocol(_))));
        // Body cut short.
        let mut short = 10u32.to_be_bytes().to_vec();
        short.extend_from_slice(b"\"Li");
        assert!(read_raw(&short).await.is_err());
        // Malformed JSON and an unknown message.
        for body in [&b"{not json"[..], b"\"Nope\""] {
            let mut frame = u32::try_from(body.len())
                .map_err(|_| Error::Protocol("len".into()))?
                .to_be_bytes()
                .to_vec();
            frame.extend_from_slice(body);
            assert!(read_raw(&frame).await.is_err());
        }
        // Zero-length frame is malformed JSON, not a clean end.
        assert!(read_raw(&[0, 0, 0, 0]).await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn oversized_writes_are_refused() -> Result<()> {
        let mut sink = Vec::new();
        let huge = ClientMsg::Input(vec![255; (MAX_FRAME / 4) as usize]);
        assert!(matches!(
            write_frame(&mut sink, &huge).await,
            Err(Error::Protocol(_))
        ));
        assert!(sink.is_empty(), "nothing is written for a refused frame");
        Ok(())
    }
}
