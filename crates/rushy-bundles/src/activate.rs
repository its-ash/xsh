//! Deciding which bundles are active and applying their effects to the shell.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::Path;

use globset::{Glob, GlobMatcher};
use rushy_core::ShellState;

use crate::manifest::{HookEvent, Segment};
use crate::store::Installed;

/// Whether `name` is an executable on `path` (a `PATH`-style list).
#[must_use]
pub fn on_path(name: &str, path: &OsStr) -> bool {
    if name.is_empty() || name.contains(['/', '\\']) {
        return false;
    }
    std::env::split_paths(path).any(|dir| is_executable(&dir.join(name)))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn matcher(pattern: &str) -> Option<GlobMatcher> {
    Glob::new(pattern).ok().map(|g| g.compile_matcher())
}

fn entry_matches(dir: &Path, m: &GlobMatcher) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|e| m.is_match(Path::new(&e.file_name())))
    })
}

/// Whether one `detect` pattern matches for `cwd`.
#[must_use]
pub fn detect(pattern: &str, cwd: &Path) -> bool {
    if let Some(rest) = pattern.strip_prefix("**/") {
        let Some(m) = matcher(rest) else {
            return false;
        };
        return cwd.ancestors().any(|d| entry_matches(d, &m));
    }
    matcher(pattern).is_some_and(|m| entry_matches(cwd, &m))
}

/// Whether `bundle` should be active in `cwd` with the given `PATH`.
#[must_use]
pub fn is_active(bundle: &Installed, cwd: &Path, path: &OsStr) -> bool {
    let rules = &bundle.manifest.bundle;
    rules.detect.iter().all(|p| detect(p, cwd))
        && rules.requires_commands.iter().all(|c| on_path(c, path))
}

/// Everything the active bundles contribute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effects {
    /// Names of active bundles, sorted.
    pub active: BTreeSet<String>,
    /// Merged aliases (later bundles win).
    pub aliases: BTreeMap<String, String>,
    /// Merged variables.
    pub env: BTreeMap<String, String>,
    /// Prompt segments by name.
    pub segments: BTreeMap<String, Segment>,
    /// Commands with bundle completions.
    pub completions: BTreeMap<String, Vec<String>>,
}

impl Effects {
    /// Computes the effects of `bundles` for `cwd`, skipping `disabled`.
    #[must_use]
    pub fn compute(bundles: &[Installed], cwd: &Path, path: &OsStr, disabled: &[String]) -> Self {
        let mut fx = Self::default();
        for b in bundles
            .iter()
            .filter(|b| !disabled.iter().any(|d| d == b.name()))
            .filter(|b| is_active(b, cwd, path))
        {
            let m = &b.manifest;
            fx.active.insert(b.name().to_owned());
            fx.env.extend(m.env.clone());
            fx.env.extend(b.init.env.clone());
            fx.aliases.extend(m.aliases.clone());
            fx.aliases.extend(b.init.aliases.clone());
            if let Some(s) = &m.prompt {
                fx.segments.insert(s.segment.clone(), s.clone());
            }
            for c in &m.completion.commands {
                let subs = m.completion.subcommands.get(c).cloned().unwrap_or_default();
                fx.completions.entry(c.clone()).or_default().extend(subs);
            }
        }
        fx
    }

    /// Moves `state` from `prev` effects to these, leaving user-set values
    /// alone: an alias or variable is removed only if it still has the value a
    /// bundle gave it.
    pub fn apply(&self, prev: &Self, state: &mut ShellState) {
        for (k, v) in &prev.aliases {
            if !self.aliases.contains_key(k) && state.aliases().get(k) == Some(v) {
                state.remove_alias(k);
            }
        }
        for (k, v) in &prev.env {
            if !self.env.contains_key(k) && state.var(k) == Some(v.as_str()) {
                state.unset_var(k);
            }
        }
        for (k, v) in &self.aliases {
            state.set_alias(k.clone(), v.clone());
        }
        for (k, v) in &self.env {
            state.set_env(k.clone(), v.clone());
        }
    }

    /// Hook commands to run for bundles that started (`Activate`) or stopped
    /// (`Deactivate`) being active between `prev` and `self`, as
    /// `(bundle, command)` pairs.
    #[must_use]
    pub fn hooks(&self, prev: &Self, bundles: &[Installed]) -> Vec<(String, String)> {
        bundles
            .iter()
            .flat_map(|b| {
                let name = b.name();
                let event = match (prev.active.contains(name), self.active.contains(name)) {
                    (false, true) => Some(HookEvent::Activate),
                    (true, false) => Some(HookEvent::Deactivate),
                    _ => None,
                };
                b.manifest
                    .hooks
                    .iter()
                    .filter(move |h| Some(h.on) == event)
                    .map(move |h| (name.to_owned(), h.run.clone()))
            })
            .collect()
    }
}
