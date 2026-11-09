//! macOS directories for config, data, and cache files.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// The base directory for data files: `$XDG_DATA_HOME` or `~/.local/share`, so
/// data never lands inside the (watched) config directory. The native config
/// and data directories on macOS are the same folder.
fn data_root() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
}

/// Root directories used by every xsh crate.
///
/// The config root is `~/Library/Application Support/xsh`. The data root is
/// `$XDG_DATA_HOME/xsh` (default `~/.local/share/xsh`). `XSH_CONFIG_DIR` and
/// `XSH_DATA_DIR` override both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    config: PathBuf,
    data: PathBuf,
}

impl Paths {
    /// Paths rooted at explicit directories.
    #[must_use]
    pub fn new(config: impl Into<PathBuf>, data: impl Into<PathBuf>) -> Self {
        Self {
            config: config.into(),
            data: data.into(),
        }
    }

    /// Paths from the environment and the macOS conventions.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoDir`] when there is no home directory and no
    /// override is set.
    pub fn discover() -> Result<Self> {
        let config = match std::env::var_os("XSH_CONFIG_DIR").filter(|v| !v.is_empty()) {
            Some(v) => PathBuf::from(v),
            None => dirs::config_dir()
                .ok_or(Error::NoDir("config"))?
                .join("xsh"),
        };
        let data = match std::env::var_os("XSH_DATA_DIR").filter(|v| !v.is_empty()) {
            Some(v) => PathBuf::from(v),
            None => data_root().ok_or(Error::NoDir("data"))?.join("xsh"),
        };
        Ok(Self::new(config, data))
    }

    /// Config root.
    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    /// Data root.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    /// `config.toml`.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    /// `ai.toml`.
    #[must_use]
    pub fn ai_file(&self) -> PathBuf {
        self.config.join("ai.toml")
    }

    /// Directory of theme files.
    #[must_use]
    pub fn themes_dir(&self) -> PathBuf {
        self.config.join("themes")
    }

    /// Directory of installed bundles, one subdirectory per bundle.
    #[must_use]
    pub fn bundles_dir(&self) -> PathBuf {
        self.config.join("bundles")
    }

    /// Encrypted history database.
    #[must_use]
    pub fn history_db(&self) -> PathBuf {
        self.data.join("history.db")
    }

    /// Encrypted AI transcripts.
    #[must_use]
    pub fn ai_dir(&self) -> PathBuf {
        self.data.join("ai")
    }

    /// Cached registry index.
    #[must_use]
    pub fn registry_cache(&self) -> PathBuf {
        self.data.join("registry.json")
    }

    /// Multiplexer sockets and session state.
    #[must_use]
    pub fn mux_dir(&self) -> PathBuf {
        self.data.join("mux")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On macOS the native config and data folders are the same
    /// (`~/Library/Application Support`); xsh must not share them, because the
    /// config directory is watched for hot reload and history writes would
    /// look like config edits.
    #[test]
    fn data_never_lives_inside_the_config_dir() {
        if std::env::var_os("XSH_CONFIG_DIR").is_some()
            || std::env::var_os("XSH_DATA_DIR").is_some()
        {
            return;
        }
        let Ok(p) = Paths::discover() else {
            return;
        };
        assert!(
            !p.data_dir().starts_with(p.config_dir()) && !p.config_dir().starts_with(p.data_dir()),
            "data {:?} overlaps config {:?}",
            p.data_dir(),
            p.config_dir()
        );
        assert!(p.history_db().starts_with(p.data_dir()));
    }
}
