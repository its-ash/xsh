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

/// Regression: two shells opening a fresh database at once could each write
/// their own salt and check, leaving one of them encrypting with a key that
/// no longer matched the stored check.
#[test]
fn concurrent_first_open_agrees_on_one_key() -> TestResult {
    const N: usize = 6;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("h.db");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(N));
    let handles: Vec<_> = (0..N)
        .map(|i| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || -> Result<(), String> {
                let pass = KeySource::Passphrase(Zeroizing::new("same".to_owned()));
                barrier.wait();
                let mut store = HistoryStore::open(&path, &pass).map_err(|e| e.to_string())?;
                store
                    .record(&entry(&format!("cmd {i}")))
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
        })
        .collect();
    for h in handles {
        h.join().map_err(|_| "thread panicked")??;
    }
    let pass = KeySource::Passphrase(Zeroizing::new("same".to_owned()));
    let store = HistoryStore::open(&path, &pass)?;
    assert_eq!(store.all()?.len(), N);
    Ok(())
}

#[test]
fn unicode_huge_and_blank_commands() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut store = HistoryStore::open(&dir.path().join("h.db"), &KeySource::Key(Key::generate()))?;
    assert!(store.record(&entry(""))?.is_none());
    assert!(store.record(&entry("  \t\n"))?.is_none());
    let uni = "echo '日本語 🦀 é\u{0}'";
    let huge = "x".repeat(1 << 20);
    store.record(&entry(uni))?;
    store.record(&entry(&huge))?;
    store.record(&entry("  padded  "))?;
    let cmds: Vec<String> = store.all()?.into_iter().map(|e| e.command).collect();
    assert_eq!(cmds, [uni, huge.as_str(), "padded"]);
    assert_eq!(store.recent(0)?.len(), 0);
    assert_eq!(store.recent(1)?.len(), 1);
    assert_eq!(store.search("", 0)?.len(), 0);
    assert_eq!(
        store.search("🦀", 10)?.first().map(|e| e.command.as_str()),
        Some(uni)
    );
    assert!(store.search("zzzz-nothing", 10)?.is_empty());
    Ok(())
}

#[test]
fn import_rejects_malformed_json_and_skips_blank_commands() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut store = HistoryStore::open(&dir.path().join("h.db"), &KeySource::Key(Key::generate()))?;
    for bad in ["", "{}", "[{\"id\": 1}]", "[", "null"] {
        assert!(store.import_json(bad.as_bytes()).is_err(), "{bad:?}");
    }
    assert!(store.all()?.is_empty(), "a failed import must add nothing");
    let row = |cmd: &str| {
        format!(
            "{{\"id\":1,\"ts\":5,\"command\":{cmd:?},\"cwd\":\"/\",\"exit_code\":null,\"duration_ms\":null,\"host\":\"h\",\"session_id\":\"s\"}}"
        )
    };
    let json = format!("[{},{},{}]", row("ls"), row("   "), row(" pwd "));
    assert_eq!(store.import_json(json.as_bytes())?, 2);
    let cmds: Vec<String> = store.all()?.into_iter().map(|e| e.command).collect();
    assert_eq!(cmds, ["ls", "pwd"]);
    // `pwd` is now the last command, so it is not recorded again.
    assert!(store.record(&entry("pwd"))?.is_none());
    Ok(())
}

#[test]
fn clear_removes_everything() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("h.db");
    let key = KeySource::Key(Key::generate());
    let mut store = HistoryStore::open(&path, &key)?;
    store.record(&entry("one"))?;
    store.clear()?;
    assert!(store.all()?.is_empty());
    // After a clear the same command may be recorded again.
    assert!(store.record(&entry("one"))?.is_some());
    drop(store);
    assert_eq!(HistoryStore::open(&path, &key)?.all()?.len(), 1);
    Ok(())
}

#[test]
fn not_a_database_is_an_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("h.db");
    std::fs::write(
        &path,
        "this is not sqlite, just some text that is long enough",
    )?;
    assert!(HistoryStore::open(&path, &KeySource::Key(Key::generate())).is_err());
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
