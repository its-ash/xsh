//! The `bundle.toml` schema and its validation rules.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Version of rushy that bundles are checked against.
pub const RUSHY_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A parsed `bundle.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Package metadata.
    pub package: Package,
    /// Activation conditions.
    #[serde(default)]
    pub bundle: Rules,
    /// Variables exported while active.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Aliases defined while active.
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    /// Prompt segment.
    #[serde(default)]
    pub prompt: Option<Segment>,
    /// Completion data.
    #[serde(default)]
    pub completion: Completion,
    /// Commands run on lifecycle events.
    #[serde(default)]
    pub hooks: Vec<Hook>,
}

/// `[package]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    /// Bundle name: lowercase letters, digits, `-`, `_`.
    pub name: String,
    /// Semantic version.
    pub version: String,
    /// One-line summary.
    #[serde(default)]
    pub description: String,
    /// Author.
    #[serde(default)]
    pub author: String,
    /// SPDX license expression.
    #[serde(default)]
    pub license: String,
    /// Oldest rushy that can load the bundle.
    #[serde(default)]
    pub min_rushy_version: Option<String>,
}

/// `[bundle]`: all conditions must hold for the bundle to activate.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    /// Glob patterns relative to `$PWD`. A pattern starting with `**/` also
    /// matches in any parent directory (so `**/.git` means "inside a Git
    /// repository"); other patterns match entries of `$PWD` itself.
    #[serde(default)]
    pub detect: Vec<String>,
    /// Commands that must exist on `PATH`.
    #[serde(default)]
    pub requires_commands: Vec<String>,
}

/// `[prompt]`: one prompt segment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    /// Segment name, used as `{name}` in theme prompt formats.
    pub segment: String,
    /// Color name or `#rrggbb`.
    #[serde(default)]
    pub color: String,
    /// Text with placeholders such as `{branch}`, `{dirty}`, `{version}`.
    pub format: String,
    /// Higher priority segments are kept when the prompt is too wide.
    #[serde(default = "default_priority")]
    pub priority: u8,
    /// Truncate the rendered segment to this many columns.
    #[serde(default)]
    pub max_width: Option<u16>,
}

fn default_priority() -> u8 {
    50
}

/// `[completion]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completion {
    /// Commands this bundle completes.
    #[serde(default)]
    pub commands: Vec<String>,
    /// First-argument candidates per command.
    #[serde(default)]
    pub subcommands: BTreeMap<String, Vec<String>>,
}

/// When a hook runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookEvent {
    /// The bundle became active.
    Activate,
    /// The bundle stopped being active.
    Deactivate,
}

/// `[[hooks]]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// Trigger.
    pub on: HookEvent,
    /// Command line, checked by the safety guard before it runs.
    pub run: String,
}

/// Whether `name` is a valid bundle name.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn valid_alias(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_whitespace() || "=/|&;<>'\"\\$`".contains(c))
}

impl Manifest {
    /// Parses and validates manifest text.
    ///
    /// ```
    /// let m = rushy_bundles::Manifest::parse(
    ///     "x.toml".as_ref(),
    ///     "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n[aliases]\nd = \"echo demo\"\n",
    /// );
    /// assert_eq!(m.map(|m| m.aliases.len()).ok(), Some(1));
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a located parse error or [`Error::Invalid`].
    pub fn parse(path: &Path, text: &str) -> Result<Self> {
        let m: Self = rushy_config::parse_toml(path, text)?;
        m.validate()?;
        Ok(m)
    }

    /// Reads and validates a manifest file.
    ///
    /// # Errors
    ///
    /// Fails if the file is unreadable or invalid.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(path, &text)
    }

    fn invalid(&self, reason: impl Into<String>) -> Error {
        Error::Invalid {
            bundle: self.package.name.clone(),
            reason: reason.into(),
        }
    }

    /// Checks names, versions, and keys.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] describing the first problem.
    pub fn validate(&self) -> Result<()> {
        if !valid_name(&self.package.name) {
            return Err(self.invalid("name must use only a-z, 0-9, `-`, and `_`"));
        }
        semver::Version::parse(&self.package.version)
            .map_err(|e| self.invalid(format!("version: {e}")))?;
        if let Some(min) = &self.package.min_rushy_version {
            let min = semver::Version::parse(min)
                .map_err(|e| self.invalid(format!("min_rushy_version: {e}")))?;
            let current =
                semver::Version::parse(RUSHY_VERSION).map_err(|e| self.invalid(e.to_string()))?;
            if min > current {
                return Err(self.invalid(format!("needs rushy {min} or later")));
            }
        }
        if let Some(k) = self
            .env
            .keys()
            .find(|k| !rushy_core::parser::is_var_name(k))
        {
            return Err(self.invalid(format!("`{k}` is not a valid variable name")));
        }
        if let Some(k) = self.aliases.keys().find(|k| !valid_alias(k)) {
            return Err(self.invalid(format!("`{k}` is not a valid alias name")));
        }
        if let Some(p) = &self.prompt {
            if p.segment.is_empty()
                || !p
                    .segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(self.invalid("prompt.segment must be alphanumeric"));
            }
        }
        if let Some(h) = self.hooks.iter().find(|h| h.run.trim().is_empty()) {
            return Err(self.invalid(format!("empty {:?} hook", h.on)));
        }
        Ok(())
    }
}
