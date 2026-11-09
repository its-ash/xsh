//! Encrypted local storage for prompts and transcripts.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use rand::RngCore as _;
use rushy_history::crypto::{Cipher, Key, KeySource};
use serde::{Deserialize, Serialize};

use crate::provider::ChatMessage;
use crate::{Error, Result};

/// Keychain service for the transcript key.
pub const KEYRING_SERVICE: &str = "xsh-ai";
const AAD: &[u8] = b"rushy-ai-transcript";

/// A saved interaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// `suggest`, `explain`, `fix`, or `agent`.
    pub kind: String,
    /// Start time, ms since the epoch.
    pub ts: i64,
    /// Provider name.
    pub provider: String,
    /// Messages, including the final reply.
    pub messages: Vec<ChatMessage>,
}

/// Writes transcripts as sealed files under a directory.
#[derive(Debug, Clone)]
pub struct TranscriptStore {
    dir: PathBuf,
    cipher: Cipher,
}

impl TranscriptStore {
    /// A store in `dir` sealing with `key`.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>, key: &Key) -> Self {
        Self {
            dir: dir.into(),
            cipher: Cipher::new(key),
        }
    }

    /// A store in `dir` using a key from `source`. A random salt for
    /// passphrase keys lives in `dir/salt`.
    ///
    /// # Errors
    ///
    /// Fails if the key cannot be obtained or the salt cannot be stored.
    pub fn open(dir: impl Into<PathBuf>, source: &KeySource) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let salt_path = dir.join("salt");
        let salt = match std::fs::read(&salt_path) {
            Ok(s) if s.len() >= 16 => s,
            _ => {
                let mut s = vec![0u8; 16];
                rand::rngs::OsRng.fill_bytes(&mut s);
                std::fs::write(&salt_path, &s)?;
                s
            }
        };
        let key = source.resolve(&salt)?;
        Ok(Self::new(dir, &key))
    }

    /// Directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Seals and writes a transcript; returns its path.
    ///
    /// # Errors
    ///
    /// Fails on encryption or write errors.
    pub fn save(&self, t: &Transcript) -> Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700))?;
        let sealed = self.cipher.seal(&serde_json::to_vec(t)?, AAD)?;
        let mut path = self.dir.join(format!("{}-{}.bin", t.ts, t.kind));
        let mut n = 1;
        while path.exists() {
            path = self.dir.join(format!("{}-{}-{n}.bin", t.ts, t.kind));
            n += 1;
        }
        std::fs::write(&path, sealed)?;
        Ok(path)
    }

    /// Reads and opens a transcript.
    ///
    /// # Errors
    ///
    /// Fails on read, decryption, or parse errors.
    pub fn load(&self, path: &Path) -> Result<Transcript> {
        let data = std::fs::read(path)?;
        let plain = self.cipher.open(&data, AAD).map_err(Error::Crypto)?;
        Ok(serde_json::from_slice(&plain)?)
    }

    /// Transcript files, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "bin"))
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }
}
