//! `ai.toml`: provider choice, credentials, and context settings.
//!
//! ```toml
//! [default]
//! provider = "openai"
//! model = "gpt-4o-mini"
//! temperature = 0.2
//! max_context_tokens = 8000
//!
//! [providers.openai]
//! api_key = { keyring = "xsh-openai" }   # or { env = "OPENAI_API_KEY" }
//!
//! [providers.ollama]
//! base_url = "http://localhost:11434"
//! model = "llama3.1:8b"
//!
//! [providers.custom]                      # any OpenAI-compatible service
//! base_url = "http://127.0.0.1:9694/v1"  # models load from <base_url>/models
//! model = "my-model"
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;
use url::Url;

use crate::{Error, Result};

/// Default Anthropic model.
pub const ANTHROPIC_MODEL: &str = "claude-opus-5-5";
/// Default OpenAI model.
pub const OPENAI_MODEL: &str = "gpt-4o-mini";
/// Default Ollama model.
pub const OLLAMA_MODEL: &str = "llama3.1:8b";

/// A secret string that never appears in `Debug` or `Display` output.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret value; keep it out of logs.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Where an API key comes from.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum KeySpec {
    /// `{ keyring = "service" }`: the OS keychain entry `service` / `rushy`.
    Keyring {
        /// Keychain service name.
        keyring: String,
    },
    /// `{ env = "VAR" }`: an environment variable.
    Env {
        /// Variable name.
        env: String,
    },
}

impl fmt::Debug for KeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keyring { keyring } => write!(f, "KeySpec::Keyring({keyring:?})"),
            Self::Env { env } => write!(f, "KeySpec::Env({env:?})"),
        }
    }
}

impl KeySpec {
    /// Reads the key.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoApiKey`] when the entry or variable is missing.
    pub fn resolve(&self, provider: &str) -> Result<Secret> {
        match self {
            Self::Keyring { keyring } => keyring::Entry::new(keyring, "xsh")?
                .get_password()
                .map(Secret::new)
                .map_err(|e| match e {
                    keyring::Error::NoEntry => Error::NoApiKey(provider.to_owned()),
                    other => other.into(),
                }),
            Self::Env { env } => std::env::var(env)
                .ok()
                .filter(|v| !v.is_empty())
                .map(Secret::new)
                .ok_or_else(|| Error::NoApiKey(provider.to_owned())),
        }
    }
}

/// Stores an API key in the Keychain entry `service` / `xsh`, the entry that
/// [`KeySpec::Keyring`] reads.
///
/// # Errors
///
/// Fails if the Keychain rejects the write.
pub fn store_api_key(service: &str, key: &Secret) -> Result<()> {
    keyring::Entry::new(service, "xsh")?.set_password(key.expose())?;
    Ok(())
}

/// Removes the Keychain entry `service` / `xsh`. A missing entry is not an error.
///
/// # Errors
///
/// Fails if the Keychain rejects the removal.
pub fn delete_api_key(service: &str) -> Result<()> {
    match keyring::Entry::new(service, "xsh")?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// `[default]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    /// `openai`, `anthropic`, `ollama`, or `custom`.
    pub provider: String,
    /// Model override for the chosen provider.
    pub model: Option<String>,
    /// Sampling temperature.
    pub temperature: f32,
    /// Context budget in tokens (about 4 bytes per token).
    pub max_context_tokens: u32,
    /// Output token limit.
    pub max_tokens: u32,
    /// Ask the model for a second-opinion safety review of every command.
    pub safety_review: bool,
    /// Ask before every agent step.
    pub approve_each_step: bool,
    /// Run every agent without questions and with the longer step limit. The
    /// safety checks stay on.
    pub autopilot: bool,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            model: None,
            temperature: 0.2,
            max_context_tokens: 8_000,
            max_tokens: 1_024,
            safety_review: true,
            approve_each_step: true,
            autopilot: false,
        }
    }
}

/// `[providers.<name>]`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderConfig {
    /// API key source.
    pub api_key: Option<KeySpec>,
    /// Endpoint override.
    pub base_url: Option<String>,
    /// Model for this provider.
    pub model: Option<String>,
}

impl ProviderConfig {
    /// Base address of the `custom` provider, from `base_url`.
    #[must_use]
    pub fn chat_base(&self) -> Option<String> {
        endpoint_base(self.base_url.as_deref()?).map(String::from)
    }
}

/// Base address of an OpenAI-compatible endpoint. `/v1`, `/v1/`, and
/// `/v1/models` all give `/v1/`.
pub(crate) fn endpoint_base(raw: &str) -> Option<Url> {
    let mut url = Url::parse(raw.trim()).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/');
    let path = format!("{}/", path.strip_suffix("/models").unwrap_or(path));
    url.set_path(&path);
    Some(url)
}

/// `[context]`: what each request includes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools, reason = "independent TOML switches")]
pub struct ContextConfig {
    /// Depth-1 listing of the working directory.
    pub listing: bool,
    /// `$SHELL`.
    pub shell: bool,
    /// Active bundle names.
    pub bundles: bool,
    /// Number of recent history commands (0 disables).
    pub history: usize,
    /// `git status --short` in `ai fix`.
    pub git_status: bool,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            listing: true,
            shell: true,
            bundles: true,
            history: 20,
            git_status: true,
        }
    }
}

/// The whole `ai.toml`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AiConfig {
    /// Defaults.
    pub default: Defaults,
    /// Per-provider settings.
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Context injection.
    pub context: ContextConfig,
}

impl AiConfig {
    /// Loads `path`, or defaults when it does not exist.
    ///
    /// # Errors
    ///
    /// Fails with a located error when the file is invalid.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(rushy_config::parse_toml(path, &text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Settings for `name`, or empty ones.
    #[must_use]
    pub fn provider(&self, name: &str) -> ProviderConfig {
        self.providers.get(name).cloned().unwrap_or_default()
    }

    /// Model for `name`: `[default].model` when `name` is the default
    /// provider, else `[providers.name].model`, else the built-in default.
    #[must_use]
    pub fn model(&self, name: &str) -> String {
        let fallback = match name {
            "anthropic" => ANTHROPIC_MODEL,
            "ollama" => OLLAMA_MODEL,
            "custom" => "",
            _ => OPENAI_MODEL,
        };
        self.provider(name)
            .model
            .or_else(|| {
                self.default
                    .model
                    .clone()
                    .filter(|_| self.default.provider == name)
            })
            .unwrap_or_else(|| fallback.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spec_example_and_redacts() -> Result<()> {
        let text = r#"
[default]
provider = "openai"
model = "gpt-4o-mini"
temperature = 0.2
max_context_tokens = 8000

[providers.openai]
api_key = { keyring = "xsh-openai" }

[providers.ollama]
base_url = "http://localhost:11434"
model = "llama3.1:8b"
"#;
        let cfg: AiConfig = rushy_config::parse_toml(Path::new("ai.toml"), text)?;
        assert_eq!(cfg.model("openai"), "gpt-4o-mini");
        assert_eq!(cfg.model("ollama"), "llama3.1:8b");
        assert_eq!(cfg.model("anthropic"), ANTHROPIC_MODEL);
        assert_eq!(
            cfg.provider("openai").api_key,
            Some(KeySpec::Keyring {
                keyring: "xsh-openai".into()
            })
        );
        assert_eq!(
            format!("{:?}", Secret::new("sk-live-123")),
            "Secret(<redacted>)"
        );
        Ok(())
    }
}
