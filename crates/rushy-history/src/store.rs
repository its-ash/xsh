//! SQLite history store with encrypted command and directory columns.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::RngCore as _;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::crypto::{Cipher, KeySource};
use crate::{Error, Result, fuzzy};

const CHECK_PLAINTEXT: &[u8] = b"rushy-history-v1";
const AAD_COMMAND: &[u8] = b"command";
const AAD_CWD: &[u8] = b"cwd";
const AAD_CHECK: &[u8] = b"check";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS history (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    command BLOB NOT NULL,
    cwd BLOB NOT NULL,
    exit_code INTEGER,
    duration_ms INTEGER,
    host TEXT NOT NULL,
    session_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS history_ts ON history (ts);
";

/// A stored history entry, decrypted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Row id.
    pub id: i64,
    /// Start time in milliseconds since the Unix epoch.
    pub ts: i64,
    /// Command line.
    pub command: String,
    /// Working directory.
    pub cwd: String,
    /// Exit status, if known.
    pub exit_code: Option<i32>,
    /// Run time in milliseconds, if known.
    pub duration_ms: Option<i64>,
    /// Host name.
    pub host: String,
    /// Shell session that ran the command.
    pub session_id: String,
}

/// A command to record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewEntry {
    /// Command line.
    pub command: String,
    /// Working directory.
    pub cwd: String,
    /// Exit status.
    pub exit_code: Option<i32>,
    /// Run time in milliseconds.
    pub duration_ms: Option<i64>,
    /// Host name.
    pub host: String,
    /// Shell session id.
    pub session_id: String,
    /// Start time; `None` means now.
    pub ts: Option<i64>,
}

/// Encrypted history database.
///
/// Commands and directories are sealed with XChaCha20-Poly1305 before they
/// reach SQLite, so neither the database nor its journal ever holds them in
/// plaintext.
///
/// ```
/// use rushy_history::{HistoryStore, NewEntry, crypto::{Key, KeySource}};
/// # fn main() -> rushy_history::Result<()> {
/// let dir = tempfile::tempdir()?;
/// let mut store = HistoryStore::open(&dir.path().join("h.db"), &KeySource::Key(Key::generate()))?;
/// store.record(&NewEntry { command: "ls".into(), ..NewEntry::default() })?;
/// assert_eq!(store.recent(10)?[0].command, "ls");
/// # Ok(())
/// # }
/// ```
pub struct HistoryStore {
    conn: Connection,
    cipher: Cipher,
    last: Option<String>,
}

impl std::fmt::Debug for HistoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistoryStore")
            .field("path", &self.conn.path())
            .field("cipher", &self.cipher)
            .finish_non_exhaustive()
    }
}

/// Milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl HistoryStore {
    /// Opens or creates the database at `path` with a key from `source`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::WrongKey`] when the key differs from the one the
    /// database was created with, and database or key errors otherwise.
    pub fn open(path: &Path, source: &KeySource) -> Result<Self> {
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
            restrict_permissions(dir, 0o700)?;
        }
        let conn = Connection::open(path)?;
        restrict_permissions(path, 0o600)?;
        conn.execute_batch("PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(SCHEMA)?;
        let user_version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if user_version == 0 {
            conn.execute_batch("PRAGMA user_version = 1;")?;
        }
        // Two shells can create the database at the same moment. The first
        // salt and check written win and everyone reads them back, so no
        // process keeps a key that no longer matches the stored check.
        let is_new = meta(&conn, "check")?.is_none();
        let salt = {
            let mut s = vec![0u8; 16];
            rand::rngs::OsRng.fill_bytes(&mut s);
            meta_or_insert(&conn, "salt", &s)?
        };
        let stored_params = read_argon2_params(&conn)?;
        let params = stored_params.clone().unwrap_or_default();
        let (key, used_passphrase) = source.resolve_with_params(&salt, params.clone())?;
        if is_new && used_passphrase && stored_params.is_none() {
            meta_or_insert(
                &conn,
                "argon2_m_cost",
                params.m_cost().to_string().as_bytes(),
            )?;
            meta_or_insert(
                &conn,
                "argon2_t_cost",
                params.t_cost().to_string().as_bytes(),
            )?;
            meta_or_insert(
                &conn,
                "argon2_p_cost",
                params.p_cost().to_string().as_bytes(),
            )?;
        }
        let cipher = Cipher::new(&key);
        let check = match meta(&conn, "check")? {
            Some(check) => check,
            None => meta_or_insert(&conn, "check", &cipher.seal(CHECK_PLAINTEXT, AAD_CHECK)?)?,
        };
        if cipher.open(&check, AAD_CHECK).ok().as_deref() != Some(CHECK_PLAINTEXT) {
            return Err(Error::WrongKey);
        }
        let mut store = Self {
            conn,
            cipher,
            last: None,
        };
        store.last = store.recent(1)?.pop().map(|e| e.command);
        Ok(store)
    }

    /// Records a command unless recording is paused or it repeats the
    /// previous command. Returns the new row id.
    ///
    /// # Errors
    ///
    /// Fails on database or encryption errors.
    pub fn record(&mut self, entry: &NewEntry) -> Result<Option<i64>> {
        let command = entry.command.trim();
        if command.is_empty() || self.is_paused()? || self.last.as_deref() == Some(command) {
            return Ok(None);
        }
        let id = self.insert(entry, command)?;
        self.last = Some(command.to_owned());
        Ok(Some(id))
    }

    fn insert(&self, entry: &NewEntry, command: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO history (ts, command, cwd, exit_code, duration_ms, host, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.ts.unwrap_or_else(now_ms),
                self.cipher.seal(command.as_bytes(), AAD_COMMAND)?,
                self.cipher.seal(entry.cwd.as_bytes(), AAD_CWD)?,
                entry.exit_code,
                entry.duration_ms,
                entry.host,
                entry.session_id,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// The newest `limit` entries, oldest first.
    ///
    /// # Errors
    ///
    /// Fails on database or decryption errors.
    pub fn recent(&self, limit: usize) -> Result<Vec<Entry>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, command, cwd, exit_code, duration_ms, host, session_id
             FROM history ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, Vec<u8>>(3)?,
                r.get::<_, Option<i32>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, ts, command, cwd, exit_code, duration_ms, host, session_id) = row?;
            out.push(Entry {
                id,
                ts,
                command: self.cipher.open_string(&command, AAD_COMMAND)?,
                cwd: self.cipher.open_string(&cwd, AAD_CWD)?,
                exit_code,
                duration_ms,
                host,
                session_id,
            });
        }
        out.reverse();
        Ok(out)
    }

    /// All entries, oldest first.
    ///
    /// # Errors
    ///
    /// Fails on database or decryption errors.
    pub fn all(&self) -> Result<Vec<Entry>> {
        self.recent(usize::MAX)
    }

    /// Fuzzy search, best match first, one entry per distinct command
    /// (the newest occurrence).
    ///
    /// # Errors
    ///
    /// Fails on database or decryption errors.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Entry>> {
        let mut entries: Vec<Entry> = self.all()?;
        entries.reverse();
        let (keep, order) = {
            let mut seen = std::collections::HashSet::new();
            let keep: Vec<usize> = (0..entries.len())
                .filter(|&i| {
                    entries
                        .get(i)
                        .is_some_and(|e| seen.insert(e.command.as_str()))
                })
                .collect();
            let commands: Vec<&str> = keep
                .iter()
                .filter_map(|&i| entries.get(i).map(|e| e.command.as_str()))
                .collect();
            let order = fuzzy::rank(query, &commands);
            (keep, order)
        };
        let mut slots: Vec<Option<Entry>> = entries.into_iter().map(Some).collect();
        Ok(order
            .into_iter()
            .take(limit)
            .filter_map(|i| keep.get(i).and_then(|&j| slots.get_mut(j)?.take()))
            .collect())
    }

    /// Writes all entries as a JSON array.
    ///
    /// # Errors
    ///
    /// Fails on database, decryption, or write errors.
    pub fn export_json(&self, out: impl std::io::Write) -> Result<()> {
        serde_json::to_writer_pretty(out, &self.all()?)?;
        Ok(())
    }

    /// Imports entries from a JSON array produced by [`HistoryStore::export_json`].
    /// Returns the number of entries added; blank commands are skipped.
    ///
    /// # Errors
    ///
    /// Fails on malformed JSON or database errors.
    pub fn import_json(&mut self, input: impl std::io::Read) -> Result<usize> {
        let entries: Vec<Entry> = serde_json::from_reader(input)?;
        let tx = self.conn.unchecked_transaction()?;
        let mut added = 0;
        for e in &entries {
            let command = e.command.trim();
            if command.is_empty() {
                continue;
            }
            let new = NewEntry {
                command: e.command.clone(),
                cwd: e.cwd.clone(),
                exit_code: e.exit_code,
                duration_ms: e.duration_ms,
                host: e.host.clone(),
                session_id: e.session_id.clone(),
                ts: Some(e.ts),
            };
            self.insert(&new, command)?;
            added += 1;
        }
        tx.commit()?;
        self.last = self.recent(1)?.pop().map(|e| e.command);
        Ok(added)
    }

    /// Deletes every entry.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn clear(&mut self) -> Result<()> {
        self.conn.execute("DELETE FROM history", [])?;
        self.conn.execute_batch("VACUUM")?;
        self.last = None;
        Ok(())
    }

    /// Stops recording until [`HistoryStore::resume`]; persists across sessions.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn pause(&self) -> Result<()> {
        set_meta(&self.conn, "paused", b"1")
    }

    /// Resumes recording.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn resume(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM meta WHERE k = 'paused'", [])?;
        Ok(())
    }

    /// Whether recording is paused.
    ///
    /// # Errors
    ///
    /// Fails on database errors.
    pub fn is_paused(&self) -> Result<bool> {
        Ok(meta(&self.conn, "paused")?.is_some())
    }
}

fn meta(conn: &Connection, key: &str) -> Result<Option<Vec<u8>>> {
    Ok(conn
        .query_row("SELECT v FROM meta WHERE k = ?1", [key], |r| r.get(0))
        .optional()?)
}

/// Stores `value` under `key` unless a value is already there, and returns
/// the stored value.
fn meta_or_insert(conn: &Connection, key: &str, value: &[u8]) -> Result<Vec<u8>> {
    conn.execute(
        "INSERT OR IGNORE INTO meta (k, v) VALUES (?1, ?2)",
        params![key, value],
    )?;
    meta(conn, key)?.ok_or(Error::Db(rusqlite::Error::QueryReturnedNoRows))
}

#[cfg(unix)]
fn restrict_permissions(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn read_u32_meta(conn: &Connection, key: &str) -> Result<Option<u32>> {
    let Some(bytes) = meta(conn, key)? else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|e| Error::Crypto(format!("invalid {key} metadata: {e}")))?;
    let val = text
        .parse::<u32>()
        .map_err(|e| Error::Crypto(format!("invalid {key} metadata: {e}")))?;
    Ok(Some(val))
}

fn read_argon2_params(conn: &Connection) -> Result<Option<argon2::Params>> {
    match (
        read_u32_meta(conn, "argon2_m_cost")?,
        read_u32_meta(conn, "argon2_t_cost")?,
        read_u32_meta(conn, "argon2_p_cost")?,
    ) {
        (Some(m), Some(t), Some(p)) => Ok(Some(
            argon2::Params::new(m, t, p, None).map_err(|e| Error::Crypto(e.to_string()))?,
        )),
        (None, None, None) => Ok(None),
        _ => Err(Error::Crypto(
            "incomplete Argon2id parameters in history metadata".into(),
        )),
    }
}

fn set_meta(conn: &Connection, key: &str, value: &[u8]) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![key, value],
    )?;
    Ok(())
}
