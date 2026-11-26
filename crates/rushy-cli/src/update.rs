//! `xsh app update-*`: finds and downloads a new version from GitHub releases.
//!
//! The addresses come from the version number, never from the reply of the
//! server. A download must match its `.sha256` file and contain a correctly
//! signed app of that version. The app then replaces itself.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use semver::Version;
use serde_json::{Value, json};

use crate::app::App;

const REPO: &str = "its-ash/xsh";
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(10);
const MAX_ARCHIVE_BYTES: usize = 400 * 1024 * 1024;
const NOTES_LIMIT: usize = 1_500;
const ALLOWED_REDIRECT_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

fn target() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64-apple-darwin"
    } else {
        "x86_64-apple-darwin"
    }
}

fn redirect_allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some_and(|h| {
            ALLOWED_REDIRECT_HOSTS
                .iter()
                .any(|allowed| h.eq_ignore_ascii_case(allowed))
        })
}

fn client(timeout: Duration) -> Result<reqwest::Client> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("too many redirects");
        }
        if redirect_allowed(attempt.url()) {
            attempt.follow()
        } else {
            let msg = format!("disallowed redirect target: {}", attempt.url());
            attempt.error(msg)
        }
    });
    Ok(reqwest::Client::builder()
        .https_only(true)
        .redirect(policy)
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
    let mut res = http
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
    let mut data = Vec::new();
    while let Some(chunk) = res.chunk().await? {
        if data.len().saturating_add(chunk.len()) > MAX_ARCHIVE_BYTES {
            bail!("the download is too large");
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
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
    let current = Version::parse(env!("CARGO_PKG_VERSION")).context("bad current version")?;
    if version <= current {
        bail!("version {version} is not newer than current version {current}");
    }
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
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let archive = dir.join("update.tar.gz");
    std::fs::write(&archive, &data)?;
    run(
        "/usr/bin/tar",
        &[
            "--no-same-owner",
            "--no-same-permissions",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_policy_allows_only_github_https() {
        for ok in [
            "https://github.com/its-ash/xsh/releases/download/v1.0.0/a.tar.gz",
            "https://objects.githubusercontent.com/github-production-release-asset/foo",
            "https://release-assets.githubusercontent.com/foo",
        ] {
            let u = reqwest::Url::parse(ok).ok();
            assert!(u.as_ref().is_some_and(redirect_allowed), "{ok}");
        }
        for bad in [
            "http://github.com/its-ash/xsh",
            "https://evil.example.com/a.tar.gz",
            "https://github.com.evil.com/a.tar.gz",
        ] {
            let u = reqwest::Url::parse(bad).ok();
            assert!(!u.as_ref().is_some_and(redirect_allowed), "{bad}");
        }
    }
}
