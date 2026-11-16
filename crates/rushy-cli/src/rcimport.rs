//! Imports environment and aliases from `~/.zshrc` or `~/.bashrc`.
//!
//! The login shell runs its rc file once; the result is cached in the data
//! directory under a SHA-256 of the rc files. A changed checksum re-runs it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::app::App;

const TIMEOUT: Duration = Duration::from_secs(5);
const SEP: &str = "\u{1}XSH-ALIASES\u{1}";
const CACHE: &str = "rc-cache.json";
const SKIP: &[&str] = &[
    "_", "PWD", "OLDPWD", "SHLVL", "PS1", "PS2", "RPS1", "PROMPT", "RPROMPT", "TERM", "COLUMNS",
    "LINES",
];

/// Returns the shell name and the rc files it reads.
fn target(mode: &str) -> Option<(&'static str, Vec<PathBuf>)> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let shell = match mode {
        "off" => return None,
        "zsh" | "bash" => mode.to_owned(),
        _ => Path::new(&std::env::var("SHELL").ok()?)
            .file_name()?
            .to_string_lossy()
            .into_owned(),
    };
    match shell.as_str() {
        "zsh" => {
            let dir = std::env::var_os("ZDOTDIR").map_or(home, PathBuf::from);
            Some(("zsh", vec![dir.join(".zshenv"), dir.join(".zshrc")]))
        }
        "bash" => Some(("bash", vec![home.join(".bashrc")])),
        _ => None,
    }
}

/// SHA-256 of the shell name and each rc file's contents; `None` if no rc file exists.
fn checksum(shell: &str, files: &[PathBuf]) -> Option<String> {
    let mut h = Sha256::new();
    h.update(shell);
    let mut any = false;
    for f in files {
        h.update(f.to_string_lossy().as_bytes());
        h.update([0]);
        if let Ok(b) = std::fs::read(f) {
            any = true;
            h.update(&b);
        }
        h.update([0]);
    }
    any.then(|| format!("{:x}", h.finalize()))
}

/// Strips one layer of shell quoting from an alias value.
fn unquote(v: &str) -> String {
    let v = v.trim();
    if let Some(s) = v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return s.replace("'\\''", "'");
    }
    if let Some(s) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return s.replace("\\\"", "\"");
    }
    v.to_owned()
}

/// Whether `k` is a valid variable name.
fn is_name(k: &str) -> bool {
    k.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The first `NAME=value` entry of `env -0`, without the text that the rc
/// file printed in front of it.
fn skip_banner(first: &str) -> &str {
    let starts_entry = |s: &str| s.split_once('=').is_some_and(|(k, _)| is_name(k));
    if starts_entry(first) {
        return first;
    }
    first
        .match_indices('\n')
        .map(|(i, _)| &first[i + 1..])
        .find(|s| starts_entry(s))
        .unwrap_or(first)
}

fn parse(out: &[u8]) -> Option<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    let text = String::from_utf8_lossy(out);
    let (env_part, alias_part) = text.split_once(SEP)?;
    let env = env_part
        .split('\0')
        .enumerate()
        .map(|(i, kv)| if i == 0 { skip_banner(kv) } else { kv })
        .filter_map(|kv| kv.split_once('='))
        .filter(|(k, _)| is_name(k) && !SKIP.contains(k))
        .filter(|(k, v)| std::env::var(k).ok().as_deref() != Some(*v))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let aliases = alias_part
        .lines()
        .filter_map(|l| l.strip_prefix("alias ").unwrap_or(l).split_once('='))
        .filter(|(k, _)| !k.is_empty() && !k.contains(char::is_whitespace))
        .map(|(k, v)| (unquote(k), unquote(v)))
        .collect();
    Some((env, aliases))
}

async fn capture(shell: &str) -> Option<Vec<u8>> {
    let script = format!("env -0; printf '%s' '{SEP}'; alias");
    let mut cmd = tokio::process::Command::new(shell);
    cmd.args(["-i", "-c", &script])
        .env("XSH_RC_IMPORT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .ok()?
        .ok()?;
    out.status.success().then_some(out.stdout)
}

fn to_map(v: &Value) -> BTreeMap<String, String> {
    v.as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// Loads env and aliases from the cache, or runs the rc file when its checksum changed.
pub async fn import(app: &mut App) {
    if std::env::var_os("XSH_RC_IMPORT").is_some() {
        return;
    }
    let Some((shell, files)) = target(&app.cfg.shell.import_rc) else {
        return;
    };
    let Some(sum) = checksum(shell, &files) else {
        return;
    };
    let path = app.paths.data_dir().join(CACHE);
    let cached = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(|v| v["sha256"].as_str() == Some(sum.as_str()));
    let (env, aliases) = if let Some(v) = cached {
        (to_map(&v["env"]), to_map(&v["aliases"]))
    } else {
        let Some((env, aliases)) = capture(shell).await.as_deref().and_then(parse) else {
            eprintln!("xsh: could not import ~/.{shell}rc");
            return;
        };
        let doc = json!({ "sha256": sum, "shell": shell, "env": env, "aliases": aliases });
        let tmp = path.with_extension("tmp");
        let saved = std::fs::create_dir_all(app.paths.data_dir())
            .and_then(|()| std::fs::write(&tmp, doc.to_string()))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = saved {
            eprintln!("xsh: {}: {e}", path.display());
        }
        (env, aliases)
    };
    let s = app.shell.state_mut();
    for (k, v) in &env {
        s.set_env(k, v);
    }
    for (k, v) in &aliases {
        s.set_alias(k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_env_and_both_alias_styles() {
        let out =
            format!("XSH_T_A=1\0PWD=/x\0{SEP}alias ll='ls -l'\ngs='git status'\nq='it'\\''s'\n");
        let (env, al) = parse(out.as_bytes()).unwrap_or_default();
        assert_eq!(env.get("XSH_T_A").map(String::as_str), Some("1"));
        assert!(!env.contains_key("PWD"));
        assert_eq!(al.get("ll").map(String::as_str), Some("ls -l"));
        assert_eq!(al.get("gs").map(String::as_str), Some("git status"));
        assert_eq!(al.get("q").map(String::as_str), Some("it's"));
    }

    /// An rc file that prints a banner must not turn it into a variable name.
    #[test]
    fn output_of_the_rc_file_is_not_a_variable() {
        let mut out = b"Welcome \xff \xc3\xa9 = x\nmore text\nXSH_T_B=2\0".to_vec();
        out.extend_from_slice(b"1BAD=x\0=y\0A B=z\0");
        out.extend_from_slice(SEP.as_bytes());
        let (env, _) = parse(&out).unwrap_or_default();
        assert_eq!(env.get("XSH_T_B").map(String::as_str), Some("2"));
        assert_eq!(env.len(), 1, "{env:?}");
        assert!(parse(b"no separator at all").is_none());
    }

    #[test]
    fn checksum_changes_with_content() -> std::io::Result<()> {
        let d = tempfile::tempdir()?;
        let f = d.path().join(".zshrc");
        assert!(checksum("zsh", std::slice::from_ref(&f)).is_none());
        std::fs::write(&f, "alias a=b")?;
        let one = checksum("zsh", std::slice::from_ref(&f));
        std::fs::write(&f, "alias a=c")?;
        assert_ne!(one, checksum("zsh", std::slice::from_ref(&f)));
        Ok(())
    }
}
