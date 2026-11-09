//! Error type for `rushy-history`.

/// Errors produced by `rushy-history`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An underlying I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The SQLite database failed.
    #[error("history database error: {0}")]
    Db(#[from] rusqlite::Error),
    /// The OS keychain failed.
    #[error("keychain error: {0}")]
    Keyring(#[from] keyring::Error),
    /// Encryption, decryption, or key derivation failed.
    #[error("crypto error: {0}")]
    Crypto(String),
    /// The key does not match the one the database was created with.
    #[error("wrong history key or passphrase")]
    WrongKey,
    /// No key source is available.
    #[error("no history key: the keychain is unavailable and XSH_HISTORY_PASSPHRASE is not set")]
    NoKey,
    /// Export or import data is malformed.
    #[error("invalid history data: {0}")]
    Json(#[from] serde_json::Error),
}

/// Convenience alias for results in `rushy-history`.
pub type Result<T> = std::result::Result<T, Error>;
