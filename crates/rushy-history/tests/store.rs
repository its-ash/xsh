use std::error::Error;

use rushy_history::crypto::{Key, KeySource};
use rushy_history::{HistoryStore, NewEntry};
use zeroize::Zeroizing;

type TestResult = Result<(), Box<dyn Error>>;

fn entry(command: &str) -> NewEntry {
    NewEntry {
        command: command.into(),
        cwd: "/srv/project-zeta".into(),
        exit_code: Some(0),
        duration_ms: Some(12),
        host: "box".into(),
        session_id: "s1".into(),
        ts: None,
    }
}

#[test]
fn round_trip_and_dedupe() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("h.db");
    let key = KeySource::Key(Key::from_bytes([7; 32]));
    {
        let mut store = HistoryStore::open(&path, &key)?;
        assert!(store.record(&entry("git status"))?.is_some());
        assert!(store.record(&entry("git status"))?.is_none());
        store.record(&entry("cargo test"))?;
        store.record(&entry("git status"))?;
    }
    let store = HistoryStore::open(&path, &key)?;
    let cmds: Vec<String> = store.all()?.into_iter().map(|e| e.command).collect();
    assert_eq!(cmds, ["git status", "cargo test", "git status"]);
    let found = store.search("gitst", 10)?;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].command, "git status");
    assert_eq!(found[0].cwd, "/srv/project-zeta");
    Ok(())
}

#[test]
fn commands_are_encrypted_at_rest() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("h.db");
    let mut store = HistoryStore::open(&path, &KeySource::Key(Key::generate()))?;
    store.record(&entry("echo super-secret-token-4242"))?;
    drop(store);
    let mut bytes = std::fs::read(&path)?;
    for suffix in ["-wal", "-journal"] {
        if let Ok(extra) = std::fs::read(dir.path().join(format!("h.db{suffix}"))) {
            bytes.extend(extra);
        }
    }
    let hay = String::from_utf8_lossy(&bytes);
    assert!(!hay.contains("super-secret-token-4242"));
    assert!(!hay.contains("project-zeta"));
    Ok(())
}

#[test]
fn wrong_key_is_rejected() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("h.db");
    let pass = |p: &str| KeySource::Passphrase(Zeroizing::new(p.to_owned()));
    HistoryStore::open(&path, &pass("right"))?.record(&entry("ls"))?;
    assert!(matches!(
        HistoryStore::open(&path, &pass("wrong")),
        Err(rushy_history::Error::WrongKey)
    ));
    assert_eq!(HistoryStore::open(&path, &pass("right"))?.all()?.len(), 1);
    Ok(())
}

#[test]
fn pause_export_import() -> TestResult {
    let dir = tempfile::tempdir()?;
    let key = KeySource::Key(Key::generate());
    let mut store = HistoryStore::open(&dir.path().join("a.db"), &key)?;
    store.record(&entry("one"))?;
    store.pause()?;
    assert!(store.record(&entry("hidden"))?.is_none());
    store.resume()?;
    store.record(&entry("two"))?;
    let mut json = Vec::new();
    store.export_json(&mut json)?;
    let mut other = HistoryStore::open(&dir.path().join("b.db"), &key)?;
    assert_eq!(other.import_json(json.as_slice())?, 2);
    let cmds: Vec<String> = other.all()?.into_iter().map(|e| e.command).collect();
    assert_eq!(cmds, ["one", "two"]);
    Ok(())
}
