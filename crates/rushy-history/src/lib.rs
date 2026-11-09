//! Encrypted command history and fuzzy search for rushy.
//!
//! Entries live in SQLite (`rusqlite`, bundled). The `command` and `cwd`
//! columns are sealed with XChaCha20-Poly1305 using a key from the OS keychain,
//! or derived with Argon2id from `XSH_HISTORY_PASSPHRASE` when no keychain is
//! available.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::pedantic)]

pub mod crypto;
mod error;
pub mod fuzzy;
mod store;

pub use error::{Error, Result};
pub use store::{Entry, HistoryStore, NewEntry, now_ms};

/// Keychain service name for the history key.
pub const KEYRING_SERVICE: &str = "xsh-history";
