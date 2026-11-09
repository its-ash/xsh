use std::error::Error;
use std::path::{Path, PathBuf};

use rushy_bundles::{
    BundleStore, Effects, Index, IndexSource, RegistryClient, RegistryEntry, RegistryFile,
    sha256_hex,
};
use rushy_core::ShellState;

type TestResult = Result<(), Box<dyn Error>>;

const BUNDLES: &[&str] = &[
    "git", "rsync", "adb", "npm", "pip", "postgres", "sbt", "scala", "ubuntu", "node", "python",
    "history", "vscode", "docker",
];

fn plugins() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins")
}

fn file_url(p: &Path) -> Result<String, Box<dyn Error>> {
    Ok(url::Url::from_file_path(p)
        .map_err(|()| "not absolute")?
        .to_string())
}

fn fixture_registry(dir: &Path) -> Result<String, Box<dyn Error>> {
    let mut index = Index {
        version: 1,
        bundles: Vec::new(),
    };
    for name in BUNDLES {
        let src = plugins().join(name);
        let dest = dir.join(name);
        std::fs::create_dir_all(&dest)?;
        let manifest = std::fs::read(src.join("bundle.toml"))?;
        std::fs::write(dest.join("bundle.toml"), &manifest)?;
        let mut files = Vec::new();
        for extra in rushy_bundles::EXTRA_FILES {
            if let Ok(bytes) = std::fs::read(src.join(extra)) {
                std::fs::write(dest.join(extra), &bytes)?;
                files.push(RegistryFile {
                    path: (*extra).to_owned(),
                    sha256: sha256_hex(&bytes),
                });
            }
        }
        index.bundles.push(RegistryEntry {
            name: (*name).to_owned(),
            version: "0.1.0".into(),
            description: String::new(),
            repo: "https://github.com/its-ash/rushy".into(),
            sha256: sha256_hex(&manifest),
            url: format!("{name}/bundle.toml"),
            files,
        });
    }
    let path = dir.join("registry.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&index)?)?;
    file_url(&path)
}

#[test]
fn every_reference_bundle_is_valid() -> TestResult {
    for name in BUNDLES {
        let b = BundleStore::load_dir(&plugins().join(name))?;
        assert_eq!(b.name(), *name);
        assert!(
            plugins().join(name).join("README.md").is_file(),
            "{name} has no README"
        );
    }
    Ok(())
}

#[test]
fn shipped_registry_matches_plugins() -> TestResult {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let index: Index = serde_json::from_slice(&std::fs::read(root.join("registry.json"))?)?;
    let names: Vec<&str> = index.bundles.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, BUNDLES);
    for b in &index.bundles {
        let dir = plugins().join(&b.name);
        let manifest = std::fs::read(dir.join("bundle.toml"))?;
        assert_eq!(b.sha256, sha256_hex(&manifest), "{} bundle.toml", b.name);
        for f in &b.files {
            let bytes = std::fs::read(dir.join(&f.path))?;
            assert_eq!(f.sha256, sha256_hex(&bytes), "{} {}", b.name, f.path);
        }
        assert!(b.url.starts_with("https://raw.githubusercontent.com/"));
    }
    Ok(())
}

#[tokio::test]
async fn install_git_from_local_registry_defines_gs() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let url = fixture_registry(&tmp.path().join("registry"))?;
    let client = RegistryClient::new(&url, &[], tmp.path().join("cache.json"))?;
    let store = BundleStore::new(tmp.path().join("bundles"));

    let (hits, src) = client.search("git").await?;
    assert_eq!(src, IndexSource::Network);
    assert!(hits.iter().any(|h| h.name == "git"));

    store.install(&client.fetch("git").await?)?;
    let (installed, errors) = store.list();
    assert!(errors.is_empty());
    assert_eq!(installed.len(), 1);

    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git"))?;
    std::fs::create_dir_all(repo.join("src"))?;
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin)?;
    let git = bin.join("git");
    std::fs::write(&git, "")?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))?;
    }

    let fx = Effects::compute(&installed, &repo.join("src"), bin.as_os_str(), &[]);
    assert!(fx.active.contains("git"));
    let mut state = ShellState::new(repo.clone(), Vec::new());
    fx.apply(&Effects::default(), &mut state);
    assert_eq!(
        state.aliases().get("gs").map(String::as_str),
        Some("git status --short --branch")
    );
    assert_eq!(
        state.aliases().get("gco").map(String::as_str),
        Some("git checkout")
    );
    assert_eq!(fx.hooks(&Effects::default(), &installed).len(), 1);

    let outside = Effects::compute(&installed, tmp.path(), bin.as_os_str(), &[]);
    outside.apply(&fx, &mut state);
    assert!(state.aliases().get("gs").is_none());
    Ok(())
}

#[tokio::test]
async fn tampered_bundle_is_rejected_and_cache_is_used_offline() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let reg = tmp.path().join("registry");
    let url = fixture_registry(&reg)?;
    let cache = tmp.path().join("cache.json");
    let client = RegistryClient::new(&url, &[], &cache)?;
    client.index().await?;

    std::fs::write(
        reg.join("git/bundle.toml"),
        "[package]\nname = \"git\"\nversion = \"9.9.9\"\n",
    )?;
    assert!(matches!(
        client.fetch("git").await,
        Err(rushy_bundles::Error::Checksum { .. })
    ));

    std::fs::remove_file(reg.join("registry.json"))?;
    let (index, src) = client.index().await?;
    assert_eq!(src, IndexSource::Cache);
    assert_eq!(index.bundles.len(), BUNDLES.len());
    Ok(())
}
