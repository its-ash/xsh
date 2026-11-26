//! Encrypted local storage for prompts and transcripts.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
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
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let salt_path = dir.join("salt");
        let salt = match std::fs::read(&salt_path) {
            Ok(s) if s.len() >= 16 => s,
            _ => {
                let mut s = vec![0u8; 16];
                rand::rngs::OsRng.fill_bytes(&mut s);
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&salt_path)
                {
                    Ok(mut file) => {
                        file.write_all(&s)?;
                        file.sync_all()?;
                        s
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        std::fs::read(&salt_path)?
                    }
                    Err(e) => return Err(e.into()),
                }
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
        let (tmp_path, mut tmp_file) = loop {
            let nonce = rand::rngs::OsRng.next_u64();
            let candidate = self
                .dir
                .join(format!(".{}-{}-{nonce:016x}.tmp", t.ts, t.kind));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)
            {
                Ok(f) => break (candidate, f),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        };
        if let Err(e) = tmp_file
            .write_all(&sealed)
            .and_then(|()| tmp_file.sync_all())
        {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
        drop(tmp_file);
        let mut n = 0u64;
        loop {
            let dest = if n == 0 {
                self.dir.join(format!("{}-{}.bin", t.ts, t.kind))
            } else {
                self.dir.join(format!("{}-{}-{n}.bin", t.ts, t.kind))
            };
            match std::fs::hard_link(&tmp_path, &dest) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Ok(dest);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    n += 1;
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(e.into());
                }
            }
        }
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
        out.sort_by_key(|p| transcript_sort_key(p));
        out
    }
}

fn parse_leading_i64(s: &str) -> Option<(i64, &str)> {
    let bytes = s.as_bytes();
    let start = usize::from(bytes.first() == Some(&b'-'));
    let end = bytes
        .get(start..)?
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count()
        + start;
    if end == start {
        return None;
    }
    let num = s.get(..end)?.parse::<i64>().ok()?;
    Some((num, s.get(end..).unwrap_or_default()))
}

fn transcript_sort_key(path: &Path) -> (i64, u64, String) {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let (ts, rest) = parse_leading_i64(stem).unwrap_or((i64::MAX, stem));
    let rest = rest.strip_prefix('-').unwrap_or(rest);
    let suffix = rest
        .rsplit_once('-')
        .and_then(|(_, n)| n.parse::<u64>().ok())
        .unwrap_or(0);
    (ts, suffix, stem.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_sorts_numerically_by_timestamp_and_suffix() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = TranscriptStore::new(dir.path(), &Key::generate());
        let mk = |ts: i64| Transcript {
            kind: "agent".into(),
            ts,
            provider: "mock".into(),
            messages: Vec::new(),
        };
        let p10 = store.save(&mk(10))?;
        let p10_1 = store.save(&mk(10))?;
        let p9 = store.save(&mk(9))?;
        assert_eq!(store.list(), vec![p9, p10, p10_1]);
        Ok(())
    }
}
