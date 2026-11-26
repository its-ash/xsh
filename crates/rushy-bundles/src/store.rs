//! Installed bundles under `<config>/bundles/<name>/`.

use std::io::Read as _;
use std::path::{Path, PathBuf};

const MAX_FILE_SIZE: usize = 1024 * 1024;

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
            // Only directories with a bundle name: this skips the hidden
            // `.NAME.tmp` folder an interrupted install leaves behind, which
            // would otherwise load as a second copy of the bundle.
            if !e.file_name().to_str().is_some_and(valid_name) {
                continue;
            }
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
        let _ = std::fs::remove_dir_all(self.dir.join(format!(".{name}.tmp")));
        let tmp_dir = tempfile::Builder::new()
            .prefix(&format!(".{name}."))
            .suffix(".tmp")
            .tempdir_in(&self.dir)?;
        let tmp = tmp_dir.path();
        for (file, bytes) in &download.files {
            if file != "bundle.toml" && !EXTRA_FILES.contains(&file.as_str()) {
                return Err(Error::Invalid {
                    bundle: name.clone(),
                    reason: format!("unexpected file `{file}`"),
                });
            }
            std::fs::write(tmp.join(file), bytes)?;
        }
        match Self::load_dir(tmp) {
            Ok(b) if b.name() == name => {}
            Ok(b) => {
                return Err(Error::Invalid {
                    bundle: name.clone(),
                    reason: format!("bundle.toml is named `{}`", b.name()),
                });
            }
            Err(e) => return Err(e),
        }
        let tmp_path = tmp_dir.keep();
        let _ = std::fs::remove_dir_all(&dest);
        if let Err(e) = std::fs::rename(&tmp_path, &dest) {
            let _ = std::fs::remove_dir_all(&tmp_path);
            return Err(e.into());
        }
        Self::load_dir(&dest)
    }

    /// Copies a bundle from a local directory (such as the in-tree `plugins/`).
    ///
    /// # Errors
    ///
    /// Fails if the source is not a valid bundle or contains symlinks.
    pub fn install_dir(&self, src: &Path) -> Result<Installed> {
        let label = src.display().to_string();
        if std::fs::symlink_metadata(src)?.file_type().is_symlink() {
            return Err(Error::Invalid {
                bundle: label,
                reason: "bundle source must not be a symlink".into(),
            });
        }
        let manifest_path = src.join("bundle.toml");
        let manifest_bytes = read_regular_file(&manifest_path, &label)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("missing {}", manifest_path.display()),
            )
        })?;
        let manifest_text =
            String::from_utf8(manifest_bytes.clone()).map_err(|_| Error::Invalid {
                bundle: label.clone(),
                reason: "bundle.toml is not valid UTF-8".into(),
            })?;
        let manifest = Manifest::parse(&manifest_path, &manifest_text)?;
        let mut files = vec![("bundle.toml".to_owned(), manifest_bytes)];
        for f in EXTRA_FILES {
            if let Some(bytes) = read_regular_file(&src.join(f), &manifest.package.name)? {
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

fn read_regular_file(path: &Path, bundle: &str) -> Result<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(Error::Invalid {
                    bundle: bundle.to_owned(),
                    reason: format!("refusing symlink `{}`", path.display()),
                });
            }
            if !meta.file_type().is_file() {
                return Err(Error::Invalid {
                    bundle: bundle.to_owned(),
                    reason: format!("expected regular file `{}`", path.display()),
                });
            }
            if meta.len() > MAX_FILE_SIZE as u64 {
                return Err(Error::Invalid {
                    bundle: bundle.to_owned(),
                    reason: format!("file `{}` exceeds {MAX_FILE_SIZE} bytes", path.display()),
                });
            }
            let file = std::fs::File::open(path)?;
            let mut bytes = Vec::new();
            file.take((MAX_FILE_SIZE as u64) + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_FILE_SIZE {
                return Err(Error::Invalid {
                    bundle: bundle.to_owned(),
                    reason: format!("file `{}` exceeds {MAX_FILE_SIZE} bytes", path.display()),
                });
            }
            Ok(Some(bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
