//! Authenticated encryption (XChaCha20-Poly1305) and key management.
//!
//! ```
//! use rushy_history::crypto::{Cipher, Key};
//! # fn main() -> rushy_history::Result<()> {
//! let cipher = Cipher::new(&Key::generate());
//! let sealed = cipher.seal(b"secret", b"field")?;
//! assert_eq!(cipher.open(&sealed, b"field")?, b"secret");
//! assert!(cipher.open(&sealed, b"other").is_err());
//! # Ok(())
//! # }
//! ```

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore as _;
use zeroize::Zeroizing;

use crate::{Error, Result};

const NONCE_LEN: usize = 24;
const KEYRING_USER: &str = "xsh";

/// A 256-bit symmetric key. Its bytes never appear in `Debug` output.
#[derive(Clone)]
pub struct Key(Zeroizing<[u8; 32]>);

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(<redacted>)")
    }
}

impl Key {
    /// A fresh random key.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = Zeroizing::new([0u8; 32]);
        rand::rngs::OsRng.fill_bytes(bytes.as_mut());
        Self(bytes)
    }

    /// Wraps raw key bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Derives a key from a passphrase with Argon2id.
    ///
    /// # Errors
    ///
    /// Fails if the salt is shorter than 8 bytes.
    pub fn from_passphrase(passphrase: &str, salt: &[u8]) -> Result<Self> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        argon2::Argon2::default()
            .hash_password_into(passphrase.as_bytes(), salt, bytes.as_mut())
            .map_err(|e| Error::Crypto(e.to_string()))?;
        Ok(Self(bytes))
    }

    /// Loads the key stored in the OS keychain under `service`, creating and
    /// storing a new random key when none exists.
    ///
    /// # Errors
    ///
    /// Fails if the keychain is unavailable or holds a malformed key.
    pub fn from_keyring(service: &str) -> Result<Self> {
        let entry = keyring::Entry::new(service, KEYRING_USER)?;
        match entry.get_password() {
            Ok(encoded) => {
                let raw = Zeroizing::new(
                    STANDARD
                        .decode(encoded.trim())
                        .map_err(|e| Error::Crypto(e.to_string()))?,
                );
                let bytes: [u8; 32] = raw
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::Crypto("keychain key has the wrong length".into()))?;
                Ok(Self::from_bytes(bytes))
            }
            Err(keyring::Error::NoEntry) => {
                let key = Self::generate();
                entry.set_password(&STANDARD.encode(key.0.as_ref()))?;
                Ok(key)
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Where to obtain a key. Secrets never appear in `Debug` output.
#[derive(Clone)]
pub enum KeySource {
    /// Use this key directly.
    Key(Key),
    /// Load or create the key in the OS keychain under this service name.
    Keyring(String),
    /// Derive the key from a passphrase with Argon2id.
    Passphrase(Zeroizing<String>),
    /// Try the keychain, then `XSH_HISTORY_PASSPHRASE`.
    Auto(String),
}

impl fmt::Debug for KeySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(_) => f.write_str("KeySource::Key(<redacted>)"),
            Self::Keyring(s) => write!(f, "KeySource::Keyring({s:?})"),
            Self::Passphrase(_) => f.write_str("KeySource::Passphrase(<redacted>)"),
            Self::Auto(s) => write!(f, "KeySource::Auto({s:?})"),
        }
    }
}

impl KeySource {
    /// Builds a source from a `history.key_source` setting: `auto`, `keyring`,
    /// or `passphrase` (read from `XSH_HISTORY_PASSPHRASE`).
    ///
    /// # Errors
    ///
    /// Fails for an unknown mode, or when `passphrase` is chosen and the
    /// variable is not set.
    pub fn from_mode(mode: &str, service: &str) -> Result<Self> {
        match mode {
            "auto" => Ok(Self::Auto(service.to_owned())),
            "keyring" => Ok(Self::Keyring(service.to_owned())),
            "passphrase" => std::env::var("XSH_HISTORY_PASSPHRASE")
                .map(|p| Self::Passphrase(Zeroizing::new(p)))
                .map_err(|_| Error::NoKey),
            other => Err(Error::Crypto(format!(
                "unknown key_source `{other}` (expected auto, keyring, or passphrase)"
            ))),
        }
    }

    /// Resolves the source to a key; `salt` is used for passphrase derivation.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoKey`] when `Auto` finds neither a keychain nor a
    /// passphrase, and other errors from the keychain or derivation.
    pub fn resolve(&self, salt: &[u8]) -> Result<Key> {
        match self {
            Self::Key(k) => Ok(k.clone()),
            Self::Keyring(service) => Key::from_keyring(service),
            Self::Passphrase(p) => Key::from_passphrase(p, salt),
            Self::Auto(service) => Key::from_keyring(service).or_else(|_| {
                let pass = Zeroizing::new(
                    std::env::var("XSH_HISTORY_PASSPHRASE").map_err(|_| Error::NoKey)?,
                );
                Key::from_passphrase(&pass, salt)
            }),
        }
    }
}

/// Seals and opens byte strings with XChaCha20-Poly1305 and a random nonce.
#[derive(Clone)]
pub struct Cipher(XChaCha20Poly1305);

impl fmt::Debug for Cipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Cipher(<redacted>)")
    }
}

impl Cipher {
    /// Cipher using `key`.
    #[must_use]
    pub fn new(key: &Key) -> Self {
        Self(XChaCha20Poly1305::new(key.0.as_ref().into()))
    }

    /// Encrypts `plaintext`, binding it to `aad`. Output is `nonce || ciphertext`.
    ///
    /// # Errors
    ///
    /// Fails only if the plaintext exceeds the cipher's length limit.
    pub fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let sealed = self
            .0
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| Error::Crypto("encryption failed".into()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// Decrypts data produced by [`Cipher::seal`] with the same `aad`.
    ///
    /// # Errors
    ///
    /// Fails if the data is truncated, tampered with, or sealed with another key.
    pub fn open(&self, data: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        if data.len() < NONCE_LEN {
            return Err(Error::Crypto("ciphertext is truncated".into()));
        }
        let (nonce, ct) = data.split_at(NONCE_LEN);
        self.0
            .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
            .map_err(|_| Error::Crypto("decryption failed".into()))
    }

    /// Decrypts to a UTF-8 string.
    ///
    /// # Errors
    ///
    /// Fails like [`Cipher::open`], or if the plaintext is not UTF-8.
    pub fn open_string(&self, data: &[u8], aad: &[u8]) -> Result<String> {
        String::from_utf8(self.open(data, aad)?).map_err(|e| Error::Crypto(e.to_string()))
    }
}
