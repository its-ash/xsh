use std::error::Error;
use std::path::{Path, PathBuf};

use rushy_bundles::{
    BundleStore, Download, Effects, Index, IndexSource, Installed, Manifest, RegistryClient,
    RegistryEntry, RegistryFile, sha256_hex,
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
            repo: "https://github.com/its-ash/xsh".into(),
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

fn manifest_text(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n")
}

fn download(name: &str, toml_name: &str) -> Result<Download, Box<dyn Error>> {
    Ok(Download {
        manifest: Manifest::parse(Path::new("bundle.toml"), &manifest_text(name))?,
        files: vec![("bundle.toml".into(), manifest_text(toml_name).into_bytes())],
    })
}

/// Regression: an interrupted install leaves `.NAME.tmp/bundle.toml`, which
/// `list` loaded as a second copy of the bundle.
#[test]
fn list_skips_leftover_temp_dirs() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = BundleStore::new(tmp.path());
    store.install(&download("demo", "demo")?)?;
    let leftover = tmp.path().join(".demo.tmp");
    std::fs::create_dir_all(&leftover)?;
    std::fs::write(leftover.join("bundle.toml"), manifest_text("demo"))?;
    let (ok, bad) = store.list();
    assert!(bad.is_empty(), "{bad:?}");
    assert_eq!(ok.iter().map(Installed::name).collect::<Vec<_>>(), ["demo"]);
    // A later install cleans the leftover up.
    store.install(&download("demo", "demo")?)?;
    assert!(!leftover.exists());
    Ok(())
}

/// Regression: `install` used the name from `Download::manifest` for the
/// directory but never checked that the `bundle.toml` it wrote agreed.
#[test]
fn install_rejects_a_manifest_name_mismatch() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = BundleStore::new(tmp.path());
    assert!(matches!(
        store.install(&download("demo", "other")?),
        Err(rushy_bundles::Error::Invalid { .. })
    ));
    assert!(store.list().0.is_empty());
    assert!(!tmp.path().join(".demo.tmp").exists());
    assert!(!tmp.path().join("demo").exists());
    Ok(())
}

#[test]
fn install_rejects_unexpected_or_missing_files() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = BundleStore::new(tmp.path().join("bundles"));
    for file in ["../escape", "/etc/passwd", "sub/x", "run.sh", ""] {
        let mut d = download("demo", "demo")?;
        d.files.push((file.into(), b"x".to_vec()));
        assert!(store.install(&d).is_err(), "{file:?}");
    }
    assert!(!tmp.path().join("escape").exists());
    let mut d = download("demo", "demo")?;
    d.files.clear();
    assert!(store.install(&d).is_err(), "no bundle.toml");
    let mut d = download("demo", "demo")?;
    d.files.push(("init.rushy".into(), b"rm -rf ~\n".to_vec()));
    assert!(store.install(&d).is_err(), "bad init.rushy");
    assert!(store.list().0.is_empty());
    Ok(())
}

#[test]
fn store_names_cannot_escape_the_bundles_dir() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = BundleStore::new(tmp.path().join("bundles"));
    std::fs::create_dir_all(tmp.path().join("victim"))?;
    for name in ["../victim", "..", ".", "", "/tmp", "a/b", "Demo", "démo"] {
        assert!(store.get(name).is_err(), "{name:?}");
        assert!(store.remove(name).is_err(), "{name:?}");
    }
    assert!(tmp.path().join("victim").is_dir());
    assert!(matches!(
        store.remove("missing"),
        Err(rushy_bundles::Error::NotInstalled(_))
    ));
    Ok(())
}

#[tokio::test]
async fn registry_entries_cannot_escape_or_leave_the_allowlist() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let reg = tmp.path().join("registry");
    std::fs::create_dir_all(reg.join("demo"))?;
    let manifest = manifest_text("demo");
    std::fs::write(reg.join("demo/bundle.toml"), &manifest)?;
    std::fs::write(tmp.path().join("secret"), "x")?;
    let index_path = reg.join("registry.json");
    std::fs::write(&index_path, "{\"version\":1,\"bundles\":[]}")?;
    let client = RegistryClient::new(&file_url(&index_path)?, &[], tmp.path().join("cache.json"))?;
    let entry = |name: &str, url: &str, files: Vec<RegistryFile>| RegistryEntry {
        name: name.into(),
        version: "0.1.0".into(),
        description: String::new(),
        repo: String::new(),
        sha256: sha256_hex(manifest.as_bytes()),
        url: url.into(),
        files,
    };
    assert!(
        client
            .download(&entry("demo", "demo/bundle.toml", vec![]))
            .await
            .is_ok()
    );
    for name in ["../demo", "", "Demo", "a/b"] {
        assert!(
            client
                .download(&entry(name, "demo/bundle.toml", vec![]))
                .await
                .is_err(),
            "{name:?}"
        );
    }
    // The registry name must match the manifest name.
    assert!(
        client
            .download(&entry("other", "demo/bundle.toml", vec![]))
            .await
            .is_err()
    );
    // Remote URLs must be on the allowlist (empty here).
    assert!(matches!(
        client
            .download(&entry("demo", "https://evil.example/bundle.toml", vec![]))
            .await,
        Err(rushy_bundles::Error::Url { .. })
    ));
    assert!(
        client
            .download(&entry("demo", "http://localhost/bundle.toml", vec![]))
            .await
            .is_err()
    );
    // Extra files are limited to the known names.
    let extra = |path: &str| RegistryFile {
        path: path.into(),
        sha256: sha256_hex(b"x"),
    };
    for path in ["../../secret", "../secret", "/etc/passwd", "bundle.toml"] {
        assert!(
            client
                .download(&entry("demo", "demo/bundle.toml", vec![extra(path)]))
                .await
                .is_err(),
            "{path:?}"
        );
    }
    // An uppercase or padded hash still matches; a short or empty one does not.
    let mut e = entry("demo", "demo/bundle.toml", vec![]);
    e.sha256 = format!(" {} ", e.sha256.to_uppercase());
    assert!(client.download(&e).await.is_ok());
    for bad in ["", "00", &sha256_hex(b"other")] {
        e.sha256 = (*bad).to_owned();
        assert!(matches!(
            client.download(&e).await,
            Err(rushy_bundles::Error::Checksum { .. })
        ));
    }
    Ok(())
}

#[tokio::test]
async fn oversized_local_file_is_refused() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let index_path = tmp.path().join("registry.json");
    let mut big = b"{\"version\":1,\"bundles\":[]}".to_vec();
    big.resize(1024 * 1024 + 1, b' ');
    std::fs::write(&index_path, &big)?;
    let client = RegistryClient::new(&file_url(&index_path)?, &[], tmp.path().join("cache.json"))?;
    assert!(matches!(
        client.index().await,
        Err(rushy_bundles::Error::Url { .. })
    ));
    Ok(())
}

#[test]
fn init_files_are_strictly_declarative() -> TestResult {
    let ok = rushy_bundles::parse_init(
        "init.rushy",
        "\n# c\nalias ll='ls -l' la=\"ls -a\"\nexport A=1 B='x y' C=a=b\n",
    )?;
    assert_eq!(ok.aliases.len(), 2);
    assert_eq!(ok.env.get("C").map(String::as_str), Some("a=b"));
    assert_eq!(ok.env.get("B").map(String::as_str), Some("x y"));
    for bad in [
        "echo hi",
        "alias a=b | sh",
        "alias a=b &",
        "alias a=b > /tmp/x",
        "alias a=b && rm -rf ~",
        "export A=$HOME",
        "export ~",
        "export A=$(id)",
        "export A=`id`",
        "export 1A=x",
        "export A",
        "alias =x",
        "alias 'a b'=x",
        "alias 'a;b'=x",
        "alias a/b=x",
        "alias 'x",
        "if true; then alias a=b; fi",
        "f() { alias a=b; }",
        "A=1 alias a=b",
    ] {
        assert!(
            rushy_bundles::parse_init("init.rushy", bad).is_err(),
            "{bad:?} must be rejected"
        );
    }
    Ok(())
}

#[test]
fn manifests_reject_bad_values() {
    let base = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";
    for extra in [
        "[env]\n\"1BAD\" = \"x\"\n",
        "[env]\n\"A-B\" = \"x\"\n",
        "[aliases]\n\"a b\" = \"x\"\n",
        "[aliases]\n\"\" = \"x\"\n",
        "[prompt]\nsegment = \"\"\nformat = \"x\"\n",
        "[prompt]\nsegment = \"a-b\"\nformat = \"x\"\n",
        "[prompt]\nsegment = \"a\"\nformat = \"x\"\npriority = 300\n",
        "[[hooks]]\non = \"activate\"\nrun = \"  \"\n",
        "[[hooks]]\non = \"boot\"\nrun = \"x\"\n",
        "[unknown]\n",
        "[bundle]\nscript = \"rm -rf ~\"\n",
    ] {
        let text = format!("{base}{extra}");
        assert!(
            Manifest::parse(Path::new("b.toml"), &text).is_err(),
            "{text:?}"
        );
    }
    for package in [
        "name = \"../x\"\nversion = \"0.1.0\"",
        "name = \"\"\nversion = \"0.1.0\"",
        "name = \"Demo\"\nversion = \"0.1.0\"",
        "name = \"demo\"\nversion = \"1\"",
        "name = \"demo\"\nversion = \"0.1.0\"\nmin_rushy_version = \"999.0.0\"",
        "name = \"demo\"\nversion = \"0.1.0\"\nmin_rushy_version = \"x\"",
    ] {
        let text = format!("[package]\n{package}\n");
        assert!(
            Manifest::parse(Path::new("b.toml"), &text).is_err(),
            "{text:?}"
        );
    }
    let long = format!(
        "[package]\nname = \"{}\"\nversion = \"0.1.0\"\n",
        "a".repeat(65)
    );
    assert!(Manifest::parse(Path::new("b.toml"), &long).is_err());
}

#[test]
fn detect_handles_odd_patterns() -> TestResult {
    let tmp = tempfile::tempdir()?;
    std::fs::write(tmp.path().join("Cargo.toml"), "")?;
    assert!(rushy_bundles::detect("Cargo.toml", tmp.path()));
    assert!(rushy_bundles::detect("*.toml", tmp.path()));
    assert!(!rushy_bundles::detect("package.json", tmp.path()));
    for bad in ["[", "**/[", "{a,", ""] {
        assert!(!rushy_bundles::detect(bad, tmp.path()), "{bad:?}");
    }
    assert!(!rushy_bundles::detect("x", &tmp.path().join("missing")));
    assert!(!rushy_bundles::on_path("", tmp.path().as_os_str()));
    assert!(!rushy_bundles::on_path("../bin/sh", tmp.path().as_os_str()));
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
