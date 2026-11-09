//! Error type for `rushy-core`.

/// Errors produced by `rushy-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An underlying I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The command line is malformed.
    #[error("syntax error: {0}")]
    Parse(String),
    /// The command line needs more input (unclosed quote or trailing `\`).
    #[error("syntax error: unexpected end of input")]
    Incomplete,
}

/// Convenience alias for results in `rushy-core`.
pub type Result<T> = std::result::Result<T, Error>;
