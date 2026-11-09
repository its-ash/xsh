//! Shell context injected into every AI request.

use std::fmt::Write as _;
use std::path::Path;

use crate::config::ContextConfig;

const MAX_ENTRIES: usize = 200;

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
                while let Ok(Some(e)) = rd.next_entry().await {
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
            let _ = writeln!(out, "Recent commands:\n  {}", self.history.join("\n  "));
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
