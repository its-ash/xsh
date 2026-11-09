//! Error type for `rushy-ai`.

/// Errors produced by `rushy-ai`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An underlying I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// `ai.toml` is invalid.
    #[error(transparent)]
    Config(#[from] rushy_config::Error),
    /// The HTTP request failed.
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The provider returned an error status.
    #[error("{provider} returned {status}: {message}")]
    Provider {
        /// Provider name.
        provider: String,
        /// HTTP status.
        status: u16,
        /// Error text from the provider.
        message: String,
    },
    /// The provider's reply could not be understood.
    #[error("unexpected response: {0}")]
    BadResponse(String),
    /// JSON encoding or decoding failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// No API key is configured or the keychain entry is missing.
    #[error("no API key for {0}; set it in ai.toml or the OS keychain")]
    NoApiKey(String),
    /// The OS keychain failed.
    #[error("keychain error: {0}")]
    Keyring(#[from] keyring::Error),
    /// `default.provider` names an unknown provider.
    #[error("unknown AI provider `{0}` (expected openai, anthropic, ollama, or custom)")]
    UnknownProvider(String),
    /// The `custom` provider lacks a required setting.
    #[error("custom AI provider: {0}")]
    CustomProvider(&'static str),
    /// A URL is not allowed.
    #[error("refusing URL {0}: use https://, or http:// only for localhost")]
    Url(String),
    /// Transcript encryption failed.
    #[error(transparent)]
    Crypto(#[from] rushy_history::Error),
}

/// Convenience alias for results in `rushy-ai`.
pub type Result<T> = std::result::Result<T, Error>;
