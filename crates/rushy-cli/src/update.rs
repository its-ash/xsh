//! `xsh app update-*`: finds and downloads a new version from GitHub releases.
//!
//! The addresses come from the version number, never from the reply of the
//! server. A download must match its `.sha256` file and contain a correctly
//! signed app of that version. The app then replaces itself.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use semver::Version;
use serde_json::{Value, json};

use crate::app::App;

const REPO: &str = "its-ash/rushy";
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(10);
const MAX_ARCHIVE_BYTES: usize = 400 * 1024 * 1024;
const NOTES_LIMIT: usize = 1_500;

fn target() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-apple-darwin"
    }
}

fn client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .https_only(true)
        .timeout(timeout)
        .user_agent(concat!("xsh/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Prints whether a newer release exists, as JSON.
pub async fn check() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let res = client(CHECK_TIMEOUT)?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .context("cannot reach GitHub")?;
    if res.status() == reqwest::StatusCode::NOT_FOUND {
        println!("{}", json!({ "current": current, "update": false }));
        return Ok(());
    }
    let v: Value = res.error_for_status()?.json().await?;
    let tag = v["tag_name"].as_str().context("the release has no tag")?;
    let latest = Version::parse(tag.trim_start_matches('v')).context("bad release tag")?;
    let notes: String = v["body"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(NOTES_LIMIT)
        .collect();
    let update = latest > Version::parse(current)?;
    println!(
        "{}",
        json!({
            "current": current,
            "latest": latest.to_string(),
            "update": update,
            "notes": notes,
        })
    );
    Ok(())
}

async fn fetch(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let res = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("cannot download {url}"))?
        .error_for_status()
        .with_context(|| format!("cannot download {url}"))?;
    if res
        .content_length()
        .is_some_and(|n| n > MAX_ARCHIVE_BYTES as u64)
    {
        bail!("the download is too large");
    }
    let bytes = res.bytes().await?;
    if bytes.len() > MAX_ARCHIVE_BYTES {
        bail!("the download is too large");
    }
    Ok(bytes.to_vec())
}

fn run(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("cannot run {program}"))?;
    if !out.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn check_app(app: &Path, version: &Version) -> Result<()> {
    let text = |p: &Path| p.to_string_lossy().into_owned();
    for name in ["XshApp", "xsh"] {
        if !app.join("Contents/MacOS").join(name).is_file() {
            bail!("the download has no {name} program");
        }
    }
    let plist = text(&app.join("Contents/Info.plist"));
    let found = run(
        "/usr/bin/plutil",
        &[
            "-extract",
            "CFBundleShortVersionString",
            "raw",
            "-o",
            "-",
            &plist,
        ],
    )?;
    if found != version.to_string() {
        bail!("the download is version {found}, not {version}");
    }
    run(
        "/usr/bin/codesign",
        &["--verify", "--deep", "--strict", &text(app)],
    )?;
    Ok(())
}

/// Downloads and checks `version`, and prints the path of the new app as JSON.
pub async fn download(app: &App, version: &str) -> Result<()> {
    let version = Version::parse(version).context("bad version")?;
    let tag = format!("v{version}");
    let name = format!("xsh-{tag}-{}", target());
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/{name}.tar.gz");
    let http = client(DOWNLOAD_TIMEOUT)?;
    let sums = fetch(&http, &format!("{url}.sha256")).await?;
    let sums = String::from_utf8_lossy(&sums);
    let want = sums
        .split_whitespace()
        .next()
        .filter(|s| s.len() == 64)
        .context("bad checksum file")?;
    let data = fetch(&http, &url).await?;
    if rushy_bundles::sha256_hex(&data) != want.to_ascii_lowercase() {
        bail!("the download does not match its checksum");
    }

    let dir = app.paths.data_dir().join("updates");
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("cannot clear the old update"),
    }
    std::fs::create_dir_all(&dir)?;
    let archive = dir.join("update.tar.gz");
    std::fs::write(&archive, &data)?;
    run(
        "/usr/bin/tar",
        &[
            "-xzf",
            &archive.to_string_lossy(),
            "-C",
            &dir.to_string_lossy(),
        ],
    )?;
    std::fs::remove_file(&archive)?;
    let new_app = dir.join(&name).join("xsh.app");
    check_app(&new_app, &version)?;
    println!("{}", json!({ "app": new_app }));
    Ok(())
}
