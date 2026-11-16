use std::error::Error;

use rushy_history::crypto::{Cipher, Key, KeySource};
use rushy_history::fuzzy::rank;
use zeroize::Zeroizing;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn every_seal_uses_a_fresh_nonce() -> TestResult {
    let cipher = Cipher::new(&Key::generate());
    let a = cipher.seal(b"same", b"aad")?;
    let b = cipher.seal(b"same", b"aad")?;
    assert_ne!(a.get(..24), b.get(..24), "nonce reused");
    assert_ne!(a, b);
    assert_eq!(cipher.open(&a, b"aad")?, b"same");
    assert_eq!(cipher.open(&b, b"aad")?, b"same");
    Ok(())
}

#[test]
fn tampered_truncated_and_foreign_data_is_rejected() -> TestResult {
    let cipher = Cipher::new(&Key::generate());
    let sealed = cipher.seal(b"secret", b"command")?;
    for len in [0, 1, 23, 24, 25, sealed.len() - 1] {
        assert!(
            cipher
                .open(sealed.get(..len).unwrap_or_default(), b"command")
                .is_err(),
            "{len}"
        );
    }
    for i in [0, 24, sealed.len() - 1] {
        let mut bad = sealed.clone();
        if let Some(b) = bad.get_mut(i) {
            *b ^= 1;
        }
        assert!(cipher.open(&bad, b"command").is_err(), "flipped byte {i}");
    }
    assert!(cipher.open(&sealed, b"cwd").is_err());
    assert!(
        Cipher::new(&Key::generate())
            .open(&sealed, b"command")
            .is_err()
    );
    Ok(())
}

#[test]
fn empty_plaintext_and_invalid_utf8() -> TestResult {
    let cipher = Cipher::new(&Key::generate());
    let empty = cipher.seal(b"", b"")?;
    assert_eq!(cipher.open_string(&empty, b"")?, "");
    let bad = cipher.seal(&[0xff, 0xfe], b"")?;
    assert!(cipher.open_string(&bad, b"").is_err());
    Ok(())
}

#[test]
fn passphrase_keys_depend_on_salt_and_need_a_long_salt() -> TestResult {
    let msg = b"m";
    let k1 = Key::from_passphrase("pass", b"saltsalt")?;
    let k2 = Key::from_passphrase("pass", b"saltsalt")?;
    let k3 = Key::from_passphrase("pass", b"othersalt")?;
    let sealed = Cipher::new(&k1).seal(msg, b"")?;
    assert_eq!(Cipher::new(&k2).open(&sealed, b"")?, msg);
    assert!(Cipher::new(&k3).open(&sealed, b"").is_err());
    assert!(Key::from_passphrase("pass", b"short").is_err());
    Ok(())
}

#[test]
fn secrets_never_appear_in_debug_output() {
    let pass = KeySource::Passphrase(Zeroizing::new("hunter2-secret".to_owned()));
    let key = Key::from_bytes([0xab; 32]);
    let text = format!(
        "{pass:?} {key:?} {:?} {:?}",
        KeySource::Key(key.clone()),
        Cipher::new(&key)
    );
    assert!(!text.contains("hunter2"), "{text}");
    assert!(!text.to_lowercase().contains("abab"), "{text}");
    assert!(!text.contains("171"), "{text}");
}

#[test]
fn unknown_key_source_mode_is_an_error() {
    for mode in ["", "Auto", "keychain", "file"] {
        assert!(KeySource::from_mode(mode, "svc").is_err(), "{mode:?}");
    }
    assert!(KeySource::from_mode("auto", "svc").is_ok());
    assert!(KeySource::from_mode("keyring", "svc").is_ok());
}

#[test]
fn fuzzy_edge_cases() {
    let none: [&str; 0] = [];
    assert!(rank("x", &none).is_empty());
    assert_eq!(rank("", &["a", "b"]), [0, 1]);
    assert_eq!(rank("   ", &["a", "b"]), [0, 1]);
    // Pattern syntax characters on their own must not panic.
    for q in ["!", "^", "$", "'", "\\", "!^$'", "| |", "\u{0}", "🦀", "é"] {
        let ranked = rank(q, &["", "!", "^x$", "é 🦀", "plain"]);
        assert!(ranked.iter().all(|&i| i < 5), "{q:?}");
    }
    // Ties keep input order, case is ignored.
    assert_eq!(rank("GIT", &["git a", "git b"]), [0, 1]);
    // Accented text matches its plain form.
    assert_eq!(rank("cafe", &["tea", "café"]), [1]);
    // A very long item is ranked without trouble.
    let long = "a".repeat(100_000);
    assert_eq!(rank("aaa", &[long.as_str()]), [0]);
}
