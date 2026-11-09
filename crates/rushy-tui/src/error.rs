//! Error type for `rushy-tui`.

/// Errors produced by `rushy-tui`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An underlying I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A theme file is invalid.
    #[error(transparent)]
    Config(#[from] rushy_config::Error),
    /// A color is not `#rrggbb` or a known name.
    #[error("invalid color `{0}`: use #rrggbb or a color name")]
    Color(String),
    /// No theme has this name.
    #[error("no theme named `{0}`")]
    NoTheme(String),
    /// File watching failed.
    #[error("watch error: {0}")]
    Watch(#[from] notify::Error),
    /// The multiplexer failed.
    #[error(transparent)]
    Pty(#[from] rushy_pty::Error),
}

/// Convenience alias for results in `rushy-tui`.
pub type Result<T> = std::result::Result<T, Error>;
