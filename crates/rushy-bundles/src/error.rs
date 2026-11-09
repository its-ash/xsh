//! Error type for `rushy-bundles`.

/// Errors produced by `rushy-bundles`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An underlying I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A manifest or config file is invalid.
    #[error(transparent)]
    Config(#[from] rushy_config::Error),
    /// A manifest parsed but breaks a rule.
    #[error("invalid bundle `{bundle}`: {reason}")]
    Invalid {
        /// Bundle name.
        bundle: String,
        /// What is wrong.
        reason: String,
    },
    /// A URL is not allowed (not HTTPS, or the host is not allowlisted).
    #[error("refusing URL {url}: {reason}")]
    Url {
        /// The URL.
        url: String,
        /// Why it was refused.
        reason: String,
    },
    /// A download failed.
    #[error("download failed: {0}")]
    Http(#[from] reqwest::Error),
    /// A downloaded file does not match its registry checksum.
    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    Checksum {
        /// File that failed.
        file: String,
        /// Registry hash.
        expected: String,
        /// Hash of the downloaded bytes.
        actual: String,
    },
    /// The registry has no such bundle.
    #[error("no bundle named `{0}` in the registry")]
    NotInRegistry(String),
    /// The bundle is not installed.
    #[error("bundle `{0}` is not installed")]
    NotInstalled(String),
    /// The registry index is malformed.
    #[error("invalid registry index: {0}")]
    Json(#[from] serde_json::Error),
    /// `init.rushy` contains something other than `alias` and `export`.
    #[error("{file}:{line}: {reason}")]
    Init {
        /// File name.
        file: String,
        /// 1-based line.
        line: usize,
        /// What is wrong.
        reason: String,
    },
}

/// Convenience alias for results in `rushy-bundles`.
pub type Result<T> = std::result::Result<T, Error>;
