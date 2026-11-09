//! Registry index (`registry.json`) and a client with a local cache.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::manifest::{Manifest, valid_name};
use crate::{Error, Result};

const MAX_DOWNLOAD: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

/// Files a bundle may ship besides `bundle.toml`.
pub const EXTRA_FILES: &[&str] = &["init.rushy", "README.md"];

/// The registry index.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    /// Schema version; currently `1`.
    pub version: u32,
    /// Available bundles.
    pub bundles: Vec<RegistryEntry>,
}

/// One bundle in the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryEntry {
    /// Bundle name.
    pub name: String,
    /// Latest version.
    pub version: String,
    /// One-line summary.
    #[serde(default)]
    pub description: String,
    /// Source repository (informational).
    pub repo: String,
    /// SHA-256 of `bundle.toml`, lowercase hex.
    pub sha256: String,
    /// Location of `bundle.toml`, absolute or relative to the index.
    pub url: String,
    /// Extra files (`init.rushy`, `README.md`), relative to `bundle.toml`.
    #[serde(default)]
    pub files: Vec<RegistryFile>,
}

/// An extra file listed in the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryFile {
    /// File name, one of [`EXTRA_FILES`].
    pub path: String,
    /// SHA-256, lowercase hex.
    pub sha256: String,
}

/// Where an index came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexSource {
    /// Freshly fetched.
    Network,
    /// Read from the local cache because the fetch failed.
    Cache,
}

/// A downloaded, verified bundle ready to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    /// Parsed manifest.
    pub manifest: Manifest,
    /// `(file name, bytes)`, including `bundle.toml`.
    pub files: Vec<(String, Vec<u8>)>,
}

/// Lowercase hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Fetches the registry index and bundle files.
///
/// Remote URLs must be `https://` on an allowlisted host. A `file://` index
/// (a local registry) may reference `file://` bundles; a remote index may not.
#[derive(Debug, Clone)]
pub struct RegistryClient {
    index_url: Url,
    allowed_hosts: Vec<String>,
    cache: PathBuf,
    http: reqwest::Client,
}

impl RegistryClient {
    /// A client for the index at `url`, caching it at `cache`.
    ///
    /// # Errors
    ///
    /// Fails if the URL is invalid or not allowed.
    pub fn new(url: &str, allowed_hosts: &[String], cache: impl Into<PathBuf>) -> Result<Self> {
        let index_url = Url::parse(url).map_err(|e| Error::Url {
            url: url.to_owned(),
            reason: e.to_string(),
        })?;
        let http = reqwest::Client::builder()
            .https_only(true)
            .timeout(TIMEOUT)
            .user_agent(concat!("xsh/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let client = Self {
            index_url,
            allowed_hosts: allowed_hosts.to_vec(),
            cache: cache.into(),
            http,
        };
        client.check(&client.index_url)?;
        Ok(client)
    }

    /// Index URL.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.index_url
    }

    fn check(&self, url: &Url) -> Result<()> {
        let refuse = |reason: &str| Error::Url {
            url: url.to_string(),
            reason: reason.to_owned(),
        };
        match url.scheme() {
            "https" => {
                let host = url.host_str().unwrap_or_default();
                if self
                    .allowed_hosts
                    .iter()
                    .any(|h| h.eq_ignore_ascii_case(host))
                {
                    Ok(())
                } else {
                    Err(refuse("host is not in bundles.allowed_hosts"))
                }
            }
            "file" if self.index_url.scheme() == "file" => Ok(()),
            "file" => Err(refuse("a remote registry cannot point to local files")),
            _ => Err(refuse("only https:// is allowed")),
        }
    }

    async fn get(&self, url: &Url) -> Result<Vec<u8>> {
        self.check(url)?;
        let bytes = if url.scheme() == "file" {
            let path = url.to_file_path().map_err(|()| Error::Url {
                url: url.to_string(),
                reason: "not a local path".into(),
            })?;
            tokio::fs::read(path).await?
        } else {
            let res = self
                .http
                .get(url.clone())
                .send()
                .await?
                .error_for_status()?;
            if res
                .content_length()
                .is_some_and(|n| n > MAX_DOWNLOAD as u64)
            {
                return Err(Error::Url {
                    url: url.to_string(),
                    reason: "file is larger than 1 MiB".into(),
                });
            }
            res.bytes().await?.to_vec()
        };
        if bytes.len() > MAX_DOWNLOAD {
            return Err(Error::Url {
                url: url.to_string(),
                reason: "file is larger than 1 MiB".into(),
            });
        }
        Ok(bytes)
    }

    /// Fetches the index, updating the cache; falls back to the cache when
    /// offline.
    ///
    /// # Errors
    ///
    /// Fails when the fetch fails and there is no usable cache.
    pub async fn index(&self) -> Result<(Index, IndexSource)> {
        match self.get(&self.index_url).await {
            Ok(bytes) => {
                let index: Index = serde_json::from_slice(&bytes)?;
                if let Some(dir) = self.cache.parent() {
                    tokio::fs::create_dir_all(dir).await?;
                }
                tokio::fs::write(&self.cache, &bytes).await?;
                Ok((index, IndexSource::Network))
            }
            Err(e) => match self.cached().await {
                Some(index) => Ok((index, IndexSource::Cache)),
                None => Err(e),
            },
        }
    }

    /// The cached index, if readable.
    pub async fn cached(&self) -> Option<Index> {
        let bytes = tokio::fs::read(&self.cache).await.ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Entries whose name or description contains `query` (case-insensitive).
    ///
    /// # Errors
    ///
    /// Fails like [`RegistryClient::index`].
    pub async fn search(&self, query: &str) -> Result<(Vec<RegistryEntry>, IndexSource)> {
        let (index, src) = self.index().await?;
        let q = query.to_lowercase();
        let hits = index
            .bundles
            .into_iter()
            .filter(|b| {
                b.name.to_lowercase().contains(&q) || b.description.to_lowercase().contains(&q)
            })
            .collect();
        Ok((hits, src))
    }

    fn resolve(base: &Url, rel: &str) -> Result<Url> {
        base.join(rel).map_err(|e| Error::Url {
            url: rel.to_owned(),
            reason: e.to_string(),
        })
    }

    /// Downloads and verifies a bundle described by `entry`.
    ///
    /// # Errors
    ///
    /// Fails on refused URLs, download errors, checksum mismatches, or an
    /// invalid manifest.
    pub async fn download(&self, entry: &RegistryEntry) -> Result<Download> {
        if !valid_name(&entry.name) {
            return Err(Error::Invalid {
                bundle: entry.name.clone(),
                reason: "invalid name in registry".into(),
            });
        }
        let manifest_url = Self::resolve(&self.index_url, &entry.url)?;
        let bytes = self.get(&manifest_url).await?;
        verify("bundle.toml", &bytes, &entry.sha256)?;
        let text = String::from_utf8(bytes.clone()).map_err(|e| Error::Invalid {
            bundle: entry.name.clone(),
            reason: e.to_string(),
        })?;
        let manifest = Manifest::parse(Path::new("bundle.toml"), &text)?;
        if manifest.package.name != entry.name {
            return Err(Error::Invalid {
                bundle: entry.name.clone(),
                reason: format!("manifest is named `{}`", manifest.package.name),
            });
        }
        let mut files = vec![("bundle.toml".to_owned(), bytes)];
        for f in &entry.files {
            if !EXTRA_FILES.contains(&f.path.as_str()) {
                return Err(Error::Invalid {
                    bundle: entry.name.clone(),
                    reason: format!("unexpected file `{}`", f.path),
                });
            }
            let data = self.get(&Self::resolve(&manifest_url, &f.path)?).await?;
            verify(&f.path, &data, &f.sha256)?;
            files.push((f.path.clone(), data));
        }
        Ok(Download { manifest, files })
    }

    /// Looks up `name` and downloads it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotInRegistry`] for unknown names, and download errors.
    pub async fn fetch(&self, name: &str) -> Result<Download> {
        let (index, _) = self.index().await?;
        let entry = index
            .bundles
            .iter()
            .find(|b| b.name == name)
            .ok_or_else(|| Error::NotInRegistry(name.to_owned()))?;
        self.download(entry).await
    }
}

fn verify(file: &str, bytes: &[u8], expected: &str) -> Result<()> {
    let actual = sha256_hex(bytes);
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        Err(Error::Checksum {
            file: file.to_owned(),
            expected: expected.to_owned(),
            actual,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(url: &str) -> Result<RegistryClient> {
        RegistryClient::new(
            url,
            &["raw.githubusercontent.com".to_owned()],
            "/nonexistent",
        )
    }

    #[test]
    fn urls_are_validated() -> Result<()> {
        assert!(client("http://raw.githubusercontent.com/x/registry.json").is_err());
        assert!(client("https://evil.example/registry.json").is_err());
        let remote = client("https://raw.githubusercontent.com/x/registry.json")?;
        let local = Url::parse("file:///tmp/bundle.toml").map_err(|e| Error::Url {
            url: String::new(),
            reason: e.to_string(),
        })?;
        assert!(remote.check(&local).is_err());
        Ok(())
    }
}
