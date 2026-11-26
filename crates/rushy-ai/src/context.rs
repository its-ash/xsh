//! Shell context injected into every AI request.

use std::fmt::Write as _;
use std::path::Path;

use crate::config::ContextConfig;

const MAX_ENTRIES: usize = 200;
const MAX_SCAN_ENTRIES: usize = MAX_ENTRIES * 10;

/// What the model is told about the shell.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    /// Working directory.
    pub cwd: String,
    /// Operating system.
    pub os: String,
    /// `$SHELL`, if enabled.
    pub shell: Option<String>,
    /// Depth-1 directory listing (directories end in `/`), if enabled.
    pub listing: Vec<String>,
    /// Active bundle names, if enabled.
    pub bundles: Vec<String>,
    /// Recent commands, oldest first, if enabled.
    pub history: Vec<String>,
}

impl Context {
    /// Gathers context as `cfg` allows.
    pub async fn gather(
        cfg: &ContextConfig,
        cwd: &Path,
        shell: Option<&str>,
        bundles: &[String],
        history: &[String],
    ) -> Self {
        let mut listing = Vec::new();
        if cfg.listing {
            if let Ok(mut rd) = tokio::fs::read_dir(cwd).await {
                while listing.len() < MAX_SCAN_ENTRIES
                    && let Ok(Some(e)) = rd.next_entry().await
                {
                    let mut name = e.file_name().to_string_lossy().into_owned();
                    if e.file_type().await.is_ok_and(|t| t.is_dir()) {
                        name.push('/');
                    }
                    listing.push(name);
                }
            }
            listing.sort();
            listing.truncate(MAX_ENTRIES);
        }
        Self {
            cwd: cwd.display().to_string(),
            os: std::env::consts::OS.to_owned(),
            shell: shell.filter(|_| cfg.shell).map(str::to_owned),
            listing,
            bundles: if cfg.bundles {
                bundles.to_vec()
            } else {
                Vec::new()
            },
            history: history
                .iter()
                .skip(history.len().saturating_sub(cfg.history))
                .cloned()
                .collect(),
        }
    }

    /// Plain-text rendering, cut to about `max_tokens` tokens.
    #[must_use]
    pub fn render(&self, max_tokens: u32) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "OS: {}", self.os);
        let _ = writeln!(out, "Working directory: {}", self.cwd);
        if let Some(s) = &self.shell {
            let _ = writeln!(
                out,
                "$SHELL: {s} (commands run in xsh, a POSIX-like shell without loops or subshells)"
            );
        }
        if !self.bundles.is_empty() {
            let _ = writeln!(out, "Active bundles: {}", self.bundles.join(", "));
        }
        if !self.listing.is_empty() {
            let _ = writeln!(out, "Directory listing:\n  {}", self.listing.join("\n  "));
        }
        if !self.history.is_empty() {
            let redacted: Vec<String> = self.history.iter().map(|h| redact_secrets(h)).collect();
            let _ = writeln!(out, "Recent commands:\n  {}", redacted.join("\n  "));
        }
        let budget = usize::try_from(max_tokens)
            .unwrap_or(usize::MAX)
            .saturating_mul(4);
        if out.len() > budget {
            let mut end = budget;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
            out.push_str("\n[context truncated]\n");
        }
        out
    }
}

const REDACTED: &str = "[REDACTED]";

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn token_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && is_token_char(bytes[i]) {
        i += 1;
    }
    i
}

fn value_end(bytes: &[u8], start: usize) -> usize {
    if start >= bytes.len() {
        return start;
    }
    let quote = bytes[start];
    if quote == b'"' || quote == b'\'' {
        let mut i = start + 1;
        while i < bytes.len() {
            if bytes[i] == quote {
                return i + 1;
            }
            i += 1;
        }
        return bytes.len();
    }
    let mut i = start;
    while i < bytes.len()
        && !bytes[i].is_ascii_whitespace()
        && !matches!(bytes[i], b';' | b',' | b')' | b']' | b'}' | b'"' | b'\'')
    {
        i += 1;
    }
    i
}

fn match_bearer_header(text: &str) -> Option<(usize, usize)> {
    const AUTH: &[u8] = b"authorization:";
    const BEARER: &[u8] = b"bearer";
    let bytes = text.as_bytes();
    if bytes.len() < AUTH.len() || !bytes[..AUTH.len()].eq_ignore_ascii_case(AUTH) {
        return None;
    }
    let mut i = AUTH.len();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if bytes.len() < i + BEARER.len() || !bytes[i..i + BEARER.len()].eq_ignore_ascii_case(BEARER) {
        return None;
    }
    i += BEARER.len();
    let ws_start = i;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i == ws_start {
        return None;
    }
    let val_start = i;
    let val_end = token_end(bytes, val_start);
    (val_end > val_start).then_some((val_start, val_end))
}

fn match_prefixed_secret(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    for (prefix, min_rest) in [
        ("sk-ant-", 4usize),
        ("sk-", 8usize),
        ("ghp_", 8usize),
        ("gho_", 8usize),
        ("ghu_", 8usize),
        ("ghs_", 8usize),
        ("ghr_", 8usize),
    ] {
        if text.starts_with(prefix) {
            let end = token_end(bytes, prefix.len());
            if end - prefix.len() >= min_rest {
                return Some(end);
            }
        }
    }
    None
}

fn match_env_secret(text: &str) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && is_ident_char(bytes[i]) {
        i += 1;
    }
    if i == 0 || bytes.get(i) != Some(&b'=') {
        return None;
    }
    let name = &bytes[..i];
    let is_secret = name
        .len()
        .checked_sub(4)
        .is_some_and(|s| name[s..].eq_ignore_ascii_case(b"_key"))
        || name
            .len()
            .checked_sub(6)
            .is_some_and(|s| name[s..].eq_ignore_ascii_case(b"_token"))
        || name
            .len()
            .checked_sub(7)
            .is_some_and(|s| name[s..].eq_ignore_ascii_case(b"_secret"));
    if !is_secret {
        return None;
    }
    let val_start = i + 1;
    let val_end = value_end(bytes, val_start);
    (val_end > val_start).then_some((val_start, val_end))
}

/// Redacts common API keys, bearer tokens, and `*_KEY`/`*_TOKEN`/`*_SECRET`
/// assignments before text is sent to an AI provider.
#[must_use]
pub fn redact_secrets(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = text.get(i..).unwrap_or_default();
        let prev_is_ident = i > 0 && is_ident_char(bytes[i - 1]);
        if !prev_is_ident && let Some((val_start, val_end)) = match_bearer_header(rest) {
            out.push_str(rest.get(..val_start).unwrap_or_default());
            out.push_str(REDACTED);
            i += val_end;
            continue;
        }
        if !prev_is_ident && let Some(end) = match_prefixed_secret(rest) {
            out.push_str(REDACTED);
            i += end;
            continue;
        }
        if !prev_is_ident && let Some((val_start, val_end)) = match_env_secret(rest) {
            out.push_str(rest.get(..val_start).unwrap_or_default());
            out.push_str(REDACTED);
            i += val_end;
            continue;
        }
        if let Some(ch) = rest.chars().next() {
            out.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_tokens_headers_and_env_assignments() {
        assert_eq!(
            redact_secrets("OPENAI_API_KEY=sk-proj-1234567890abcdef"),
            "OPENAI_API_KEY=[REDACTED]"
        );
        assert_eq!(
            redact_secrets("key is sk-ant-api03-abcdef123456"),
            "key is [REDACTED]"
        );
        assert_eq!(
            redact_secrets("token ghp_1234567890abcdef done"),
            "token [REDACTED] done"
        );
        assert_eq!(
            redact_secrets("Authorization: Bearer eyJhbGciOi.payload.sig"),
            "Authorization: Bearer [REDACTED]"
        );
        assert_eq!(
            redact_secrets("export MY_SECRET='hunter2' OTHER=ok"),
            "export MY_SECRET=[REDACTED] OTHER=ok"
        );
        assert_eq!(
            redact_secrets("API_TOKEN=\"abc123\" cargo test"),
            "API_TOKEN=[REDACTED] cargo test"
        );
        assert_eq!(
            redact_secrets("cargo build --release"),
            "cargo build --release"
        );
    }

    #[test]
    fn redacts_bearer_with_multibyte_utf8_at_64_byte_boundary() {
        let input = format!("Authorization: Bearer {}érest", "a".repeat(41));
        assert_eq!(
            redact_secrets(&input),
            "Authorization: Bearer [REDACTED]érest"
        );
    }
}
