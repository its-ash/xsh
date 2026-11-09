//! Error type for `rushy-config`.

use std::path::PathBuf;

/// Errors produced by `rushy-config`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An underlying I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A TOML file is malformed or holds unknown or mistyped keys.
    #[error("{}:{line}:{column}: {message}", path.display())]
    Invalid {
        /// File that failed to parse.
        path: PathBuf,
        /// 1-based line of the error.
        line: usize,
        /// 1-based column of the error.
        column: usize,
        /// Parser message.
        message: String,
    },
    /// The merged layers (including `XSH_*` variables) do not form a valid config.
    #[error("invalid configuration: {0}")]
    Extract(String),
    /// A platform directory could not be found.
    #[error("cannot find the {0} directory; set XSH_CONFIG_DIR and XSH_DATA_DIR")]
    NoDir(&'static str),
}

/// Convenience alias for results in `rushy-config`.
pub type Result<T> = std::result::Result<T, Error>;
