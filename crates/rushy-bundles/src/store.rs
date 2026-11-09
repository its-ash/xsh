//! Installed bundles under `<config>/bundles/<name>/`.

use std::path::{Path, PathBuf};

use crate::init::{InitFile, parse_init};
use crate::manifest::{Manifest, valid_name};
use crate::registry::{Download, EXTRA_FILES};
use crate::{Error, Result};

/// An installed bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// Parsed `bundle.toml`.
    pub manifest: Manifest,
    /// Parsed `init.rushy`, if present.
    pub init: InitFile,
    /// Bundle directory.
    pub dir: PathBuf,
}

impl Installed {
    /// Bundle name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.manifest.package.name
    }
}

/// The bundles directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleStore {
    dir: PathBuf,
}

impl BundleStore {
    /// A store rooted at `dir`.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Root directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Loads one bundle directory.
    ///
    /// # Errors
    ///
    /// Fails if `bundle.toml` or `init.rushy` is invalid.
    pub fn load_dir(dir: &Path) -> Result<Installed> {
        let manifest = Manifest::load(&dir.join("bundle.toml"))?;
        let init_path = dir.join("init.rushy");
        let init = match std::fs::read_to_string(&init_path) {
            Ok(text) => parse_init(&init_path.display().to_string(), &text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => InitFile::default(),
            Err(e) => return Err(e.into()),
        };
        Ok(Installed {
            manifest,
            init,
            dir: dir.to_path_buf(),
        })
    }

    /// All installed bundles sorted by name, plus errors for broken ones.
    #[must_use]
    pub fn list(&self) -> (Vec<Installed>, Vec<Error>) {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return (ok, bad);
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.join("bundle.toml").is_file() {
                match Self::load_dir(&path) {
                    Ok(b) => ok.push(b),
                    Err(err) => bad.push(err),
                }
            }
        }
        ok.sort_by(|a, b| a.name().cmp(b.name()));
        (ok, bad)
    }

    /// One installed bundle.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotInstalled`] or a parse error.
    pub fn get(&self, name: &str) -> Result<Installed> {
        let dir = self.path(name)?;
        if !dir.join("bundle.toml").is_file() {
            return Err(Error::NotInstalled(name.to_owned()));
        }
        Self::load_dir(&dir)
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        if valid_name(name) {
            Ok(self.dir.join(name))
        } else {
            Err(Error::Invalid {
                bundle: name.to_owned(),
                reason: "invalid bundle name".into(),
            })
        }
    }

    /// Installs or replaces a bundle atomically.
    ///
    /// # Errors
    ///
    /// Fails on invalid file names or write errors.
    pub fn install(&self, download: &Download) -> Result<Installed> {
        let name = &download.manifest.package.name;
        let dest = self.path(name)?;
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(".{name}.tmp"));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        for (file, bytes) in &download.files {
            if file != "bundle.toml" && !EXTRA_FILES.contains(&file.as_str()) {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(Error::Invalid {
                    bundle: name.clone(),
                    reason: format!("unexpected file `{file}`"),
                });
            }
            std::fs::write(tmp.join(file), bytes)?;
        }
        if let Err(e) = Self::load_dir(&tmp) {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&tmp, &dest)?;
        Self::load_dir(&dest)
    }

    /// Copies a bundle from a local directory (such as the in-tree `plugins/`).
    ///
    /// # Errors
    ///
    /// Fails if the source is not a valid bundle.
    pub fn install_dir(&self, src: &Path) -> Result<Installed> {
        let manifest = Manifest::load(&src.join("bundle.toml"))?;
        let mut files = vec![(
            "bundle.toml".to_owned(),
            std::fs::read(src.join("bundle.toml"))?,
        )];
        for f in EXTRA_FILES {
            if let Ok(bytes) = std::fs::read(src.join(f)) {
                files.push(((*f).to_owned(), bytes));
            }
        }
        self.install(&Download { manifest, files })
    }

    /// Removes a bundle.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotInstalled`] when absent.
    pub fn remove(&self, name: &str) -> Result<()> {
        let dir = self.path(name)?;
        if !dir.is_dir() {
            return Err(Error::NotInstalled(name.to_owned()));
        }
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
}
