//! Error type for `rushy-pty`.

/// Errors produced by `rushy-pty`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An underlying I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The platform PTY layer failed.
    #[error("pty error: {0}")]
    Pty(String),
    /// No session has this id or name.
    #[error("no such session: {0}")]
    NoSession(String),
    /// A session with this name already exists.
    #[error("session already exists: {0}")]
    SessionExists(String),
    /// The operation needs an attached session.
    #[error("no session is attached")]
    NotAttached,
    /// A protocol frame is malformed.
    #[error("protocol error: {0}")]
    Protocol(String),
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Protocol(e.to_string())
    }
}

/// Convenience alias for results in `rushy-pty`.
pub type Result<T> = std::result::Result<T, Error>;
