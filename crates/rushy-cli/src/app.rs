//! Application state shared by the REPL and the subcommands.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context as _, Result};
use rushy_ai::{
    AiConfig, AiProvider, ChatMessage, Context, Transcript, TranscriptStore, check_command,
};
use rushy_bundles::{BundleStore, Effects, Installed, RegistryClient};
use rushy_config::{Config, Loader, Overrides, Paths};
use rushy_core::{BUILTINS, Shell};
use rushy_history::crypto::KeySource;
use rushy_history::{HistoryStore, KEYRING_SERVICE, NewEntry, now_ms};
use rushy_tui::Theme;

/// Commands handled by the `rushy` binary itself, with one-line help.
pub const EXTRA_BUILTINS: &[(&str, &str)] = &[
    (
        "ai",
        "AI helpers: ai explain|fix|suggest|agent [--auto]|status",
    ),
    (
        "bundle",
        "manage bundles: bundle install|remove|list|search|update|info",
    ),
    ("theme", "list or switch themes: theme [name]"),
    (
        "config",
        "show or change settings: config list|get|set|unset|edit|path|check",
    ),
    ("adb-find", "scan the network for ADB devices on Wi-Fi"),
    ("adb-connect", "connect to a device that adb-find found"),
];

/// Turns on colored output of `ls` and `grep` unless the user already set
/// them: `CLICOLOR` for BSD `ls` and a `--color=auto` alias for `grep`. User
/// aliases and variables win.
fn color_defaults(s: &mut rushy_core::ShellState) {
    for (k, v) in [
        ("CLICOLOR", "1"),
        ("LSCOLORS", "ExGxFxdaCxDaDahbadacec"),
        ("GREP_COLORS", "mt=01;31"),
    ] {
        if s.var(k).is_none() {
            s.set_env(k, v);
        }
    }
    if !s.aliases().contains_key("grep") {
        s.set_alias("grep", "grep --color=auto");
    }
}

/// Data the tab completer reads.
#[derive(Debug, Default, Clone)]
pub struct CompData {
    /// Names valid in command position.
    pub commands: Vec<String>,
    /// First-argument candidates per command (from bundles).
    pub subs: BTreeMap<String, Vec<String>>,
    /// Executables found on `PATH`.
    pub path_commands: Vec<String>,
    /// The `PATH` that `path_commands` was built from.
    pub path_key: String,
    /// The shell's working directory.
    pub cwd: PathBuf,
    /// Built-in command names, colored apart from programs.
    pub builtins: BTreeSet<String>,
    /// Shell variable names, for `$NAME` completion.
    pub vars: Vec<String>,
    /// Syntax colors for the command line.
    pub colors: crate::highlight::Colors,
    /// Host names from `~/.ssh/config` and `~/.ssh/known_hosts`.
    pub hosts: Vec<String>,
}

/// A configured AI provider.
#[derive(Clone)]
pub struct AiRuntime {
    /// Parsed `ai.toml`.
    pub cfg: AiConfig,
    /// The provider.
    pub provider: Arc<dyn AiProvider>,
}

/// Everything a running `rushy` needs.
pub struct App {
    pub paths: Paths,
    pub cfg: Config,
    overrides: Overrides,
    pub shell: Shell,
    pub theme: Theme,
    pub store: BundleStore,
    pub installed: Vec<Installed>,
    pub effects: Effects,
    pub history: Option<HistoryStore>,
    pub session_id: String,
    pub host: String,
    pub comp: Arc<RwLock<CompData>>,
    /// Options read from man pages, for flag completion.
    pub man: crate::man::ManCache,
    ai: Option<AiRuntime>,
    last_env: Option<(PathBuf, String)>,
    warned_history: bool,
}

/// The machine's host name.
pub fn hostname() -> String {
    if let Ok(h) = std::env::var("HOSTNAME")
        && !h.is_empty()
    {
        return h;
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "localhost".to_owned())
}

/// `cwd` with `home` shown as `~`.
pub fn display_path(cwd: &Path, home: Option<&str>) -> String {
    if let Some(rest) = home
        .filter(|h| !h.is_empty())
        .and_then(|h| cwd.strip_prefix(h).ok())
    {
        return if rest.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", rest.display())
        };
    }
    cwd.display().to_string()
}

fn load_theme(paths: &Paths, cfg: &Config) -> Theme {
    if cfg.ui.no_color
        && let Ok(t) = Theme::mono()
    {
        return t;
    }
    match Theme::load(&paths.themes_dir(), &cfg.ui.theme) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("xsh: theme `{}`: {e}; using tokyo-night", cfg.ui.theme);
            Theme::builtin("tokyo-night")
                .or_else(|_| Theme::mono())
                .unwrap_or_else(|_| plain_theme())
        }
    }
}

/// A last-resort theme, so loading a theme can never fail the shell.
fn plain_theme() -> Theme {
    Theme {
        name: "plain".into(),
        background: ratatui::style::Color::Reset,
        foreground: ratatui::style::Color::Reset,
        accent: ratatui::style::Color::Reset,
        error: ratatui::style::Color::Reset,
        success: ratatui::style::Color::Reset,
        prompt_user: ratatui::style::Color::Reset,
        prompt_path: ratatui::style::Color::Reset,
        prompt_format: "{path}> ".into(),
        plain: true,
        cursor: ratatui::style::Color::Reset,
        selection: ratatui::style::Color::Reset,
        ansi: Vec::new(),
    }
}

fn load_config(paths: &Paths, overrides: &Overrides) -> Config {
    let cwd = std::env::current_dir().unwrap_or_default();
    match Loader::new(paths.clone())
        .project(cwd)
        .overrides(overrides.clone())
        .load()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("xsh: {e}\nxsh: using default settings");
            let mut c = Config::default();
            if let Some(t) = &overrides.theme {
                c.ui.theme.clone_from(t);
            }
            c
        }
    }
}

impl App {
    /// Loads configuration, bundles, and (when `interactive`) history.
    pub fn load(overrides: Overrides, interactive: bool) -> Result<Self> {
        let paths = Paths::discover()?;
        let cfg = load_config(&paths, &overrides);
        let mut shell = Shell::from_env().context("failed to initialize shell state")?;
        shell
            .state_mut()
            .set_capture_stderr(interactive && cfg.shell.capture_stderr);
        shell.state_mut().set_cdpath(cfg.shell.cdpath.clone());
        if interactive && !cfg.ui.no_color {
            color_defaults(shell.state_mut());
        }
        let store = BundleStore::new(paths.bundles_dir());
        let (installed, broken) = store.list();
        for e in broken {
            eprintln!("xsh: skipping a broken bundle: {e}");
        }
        let theme = load_theme(&paths, &cfg);
        let mut app = Self {
            paths,
            cfg,
            overrides,
            shell,
            theme,
            store,
            installed,
            effects: Effects::default(),
            history: None,
            session_id: format!("{}-{}", std::process::id(), now_ms()),
            host: hostname(),
            comp: Arc::new(RwLock::new(CompData::default())),
            man: crate::man::ManCache::default(),
            ai: None,
            last_env: None,
            warned_history: false,
        };
        if interactive
            && app.cfg.history.enabled
            && let Err(e) = app.open_history()
        {
            eprintln!("xsh: history is off: {e:#}");
            app.warned_history = true;
        }
        Ok(app)
    }

    /// Reloads config, theme, and bundles (hot reload).
    pub fn reload(&mut self) {
        self.cfg = load_config(&self.paths, &self.overrides);
        self.theme = load_theme(&self.paths, &self.cfg);
        self.reload_bundles();
        let capture = self.shell.state().capture_stderr();
        self.shell
            .state_mut()
            .set_capture_stderr(capture && self.cfg.shell.capture_stderr);
        self.shell
            .state_mut()
            .set_cdpath(self.cfg.shell.cdpath.clone());
        self.ai = None;
    }

    /// Re-reads installed bundles and forces the next effects refresh.
    pub fn reload_bundles(&mut self) {
        let (installed, broken) = self.store.list();
        for e in broken {
            eprintln!("xsh: skipping a broken bundle: {e}");
        }
        self.installed = installed;
        self.last_env = None;
    }

    fn open_history(&mut self) -> Result<()> {
        let source = KeySource::from_mode(&self.cfg.history.key_source, KEYRING_SERVICE)?;
        self.history = Some(HistoryStore::open(&self.paths.history_db(), &source)?);
        Ok(())
    }

    /// The history store, opened on demand.
    pub fn history_store(&mut self) -> Result<&mut HistoryStore> {
        if self.history.is_none() {
            self.open_history()
                .context("cannot open the history store")?;
        }
        self.history
            .as_mut()
            .context("history store is unavailable")
    }

    /// The start-up script: `shell.rc_file` inside the config directory. A
    /// name that leaves the directory falls back to `init.xsh`.
    pub fn rc_path(&self) -> PathBuf {
        let name = Path::new(&self.cfg.shell.rc_file);
        let safe = name
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
        self.paths
            .config_dir()
            .join(if safe { name } else { Path::new("init.xsh") })
    }

    /// Records a finished command in the encrypted history.
    pub fn record_history(&mut self, line: &str, status: i32, took: Duration) {
        let h = &self.cfg.history;
        if (h.ignore_space && line.starts_with(' ')) || !h.enabled || line.trim().is_empty() {
            return;
        }
        if h.ignore_dups
            && self
                .recent_commands(1)
                .first()
                .is_some_and(|c| c == line.trim())
        {
            return;
        }
        let entry = NewEntry {
            command: line.trim().to_owned(),
            cwd: self.shell.state().cwd().display().to_string(),
            exit_code: Some(status),
            duration_ms: i64::try_from(took.as_millis()).ok(),
            host: self.host.clone(),
            session_id: self.session_id.clone(),
            ts: None,
        };
        let Some(h) = self.history.as_mut() else {
            return;
        };
        if let Err(e) = h.record(&entry)
            && !self.warned_history
        {
            eprintln!("xsh: could not save history: {e}");
            self.warned_history = true;
        }
    }

    /// Recomputes which bundles are active for the current directory and
    /// `PATH`. Returns the `(bundle, command)` hooks to run; empty when
    /// nothing changed.
    pub fn refresh_effects(&mut self) -> Vec<(String, String)> {
        let cwd = self.shell.state().cwd().to_path_buf();
        let path = self
            .shell
            .state()
            .var("PATH")
            .unwrap_or_default()
            .to_owned();
        if self.last_env.as_ref() == Some(&(cwd.clone(), path.clone())) {
            self.update_completion(&path);
            return Vec::new();
        }
        let prev = std::mem::take(&mut self.effects);
        let fx = Effects::compute(
            &self.installed,
            &cwd,
            OsStr::new(&path),
            &self.cfg.bundles.disabled,
        );
        fx.apply(&prev, self.shell.state_mut());
        let hooks = if self.cfg.bundles.allow_hooks {
            fx.hooks(&prev, &self.installed)
        } else {
            Vec::new()
        };
        self.effects = fx;
        self.last_env = Some((cwd, path.clone()));
        self.update_completion(&path);
        hooks
    }

    /// Runs bundle hooks after the safety guard checks each one.
    pub async fn run_hooks(&mut self, hooks: Vec<(String, String)>) {
        for (bundle, command) in hooks {
            let verdict = check_command(&command);
            if !verdict.is_allowed() {
                eprintln!("xsh: bundle `{bundle}`: hook not run: {verdict}");
                continue;
            }
            let status = self.shell.last_status();
            let _ = self.shell.run_line(&format!(" {command}")).await;
            self.shell.state_mut().set_last_status(status);
        }
    }

    fn update_completion(&self, path: &str) {
        let Ok(mut c) = self.comp.write() else {
            return;
        };
        let mut names: BTreeSet<String> = BUILTINS
            .iter()
            .chain(EXTRA_BUILTINS)
            .map(|(n, _)| (*n).to_owned())
            .collect();
        names.extend(self.shell.state().aliases().keys().cloned());
        names.extend(self.shell.function_names());
        names.extend(self.effects.completions.keys().cloned());
        c.builtins = BUILTINS
            .iter()
            .chain(EXTRA_BUILTINS)
            .map(|(n, _)| (*n).to_owned())
            .collect();
        c.commands = names.into_iter().collect();
        c.vars = self.shell.state().vars().keys().cloned().collect();
        c.colors = crate::highlight::Colors::new(
            &self.theme,
            self.cfg.ui.no_color || !self.cfg.ui.highlight,
        );
        if c.hosts.is_empty() {
            c.hosts = crate::highlight::ssh_hosts(self.shell.state().home().as_deref());
        }
        let mut subs = self.effects.completions.clone();
        for (cmd, list) in crate::subcmds::TABLE {
            let e = subs.entry((*cmd).to_owned()).or_default();
            for s in *list {
                if !e.iter().any(|x| x == s) {
                    e.push((*s).to_owned());
                }
            }
            e.sort();
        }
        c.subs = subs;
        c.cwd = self.shell.state().cwd().to_path_buf();
        if c.path_key != path {
            c.path_commands = scan_path(path);
            path.clone_into(&mut c.path_key);
        }
    }

    /// Names for the command palette.
    pub fn palette_items(&self) -> Vec<rushy_tui::palette::Item> {
        let builtins: Vec<(&str, &str)> = BUILTINS.iter().chain(EXTRA_BUILTINS).copied().collect();
        let bundles: Vec<String> = self.effects.active.iter().cloned().collect();
        let mut themes: Vec<String> = rushy_tui::BUILTIN.iter().map(|s| (*s).to_owned()).collect();
        themes.extend(user_themes(&self.paths.themes_dir()));
        themes.sort();
        themes.dedup();
        rushy_tui::palette::items(
            &builtins,
            &bundles,
            self.shell.state().aliases(),
            &themes,
            &self.recent_commands(200),
        )
    }

    /// The newest `n` commands, oldest first, from the store or the session.
    pub fn recent_commands(&self, n: usize) -> Vec<String> {
        if let Some(h) = &self.history {
            h.recent(n)
                .map(|v| v.into_iter().map(|e| e.command).collect())
                .unwrap_or_default()
        } else {
            let all = self.shell.state().history();
            all[all.len().saturating_sub(n)..].to_vec()
        }
    }

    /// A registry client for the configured URL.
    pub fn registry(&self) -> Result<RegistryClient> {
        Ok(RegistryClient::new(
            &self.cfg.bundles.registry_url,
            &self.cfg.bundles.allowed_hosts,
            self.paths.registry_cache(),
        )?)
    }

    /// The configured AI provider, created on first use.
    pub fn ai(&mut self) -> Result<AiRuntime> {
        if let Some(a) = &self.ai {
            return Ok(a.clone());
        }
        let cfg = AiConfig::load(&self.paths.ai_file())?;
        let provider: Arc<dyn AiProvider> = Arc::from(rushy_ai::from_config(&cfg)?);
        let rt = AiRuntime { cfg, provider };
        self.ai = Some(rt.clone());
        Ok(rt)
    }

    /// The context sent with AI requests.
    pub async fn ai_context(&self, cfg: &AiConfig) -> Context {
        let shell = self.shell.state().var("SHELL").map(str::to_owned);
        let bundles: Vec<String> = self.effects.active.iter().cloned().collect();
        let history = self.recent_commands(cfg.context.history);
        Context::gather(
            &cfg.context,
            self.shell.state().cwd(),
            shell.as_deref(),
            &bundles,
            &history,
        )
        .await
    }

    /// Saves an encrypted transcript; warns and skips when it cannot be
    /// encrypted (it is never written in plain text).
    pub fn save_transcript(&self, kind: &str, provider: &str, messages: Vec<ChatMessage>) {
        let source =
            match KeySource::from_mode(&self.cfg.history.key_source, rushy_ai::KEYRING_SERVICE) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("xsh: transcript not saved: {e}");
                    return;
                }
            };
        let saved = TranscriptStore::open(self.paths.ai_dir(), &source).and_then(|s| {
            s.save(&Transcript {
                kind: kind.to_owned(),
                ts: now_ms(),
                provider: provider.to_owned(),
                messages,
            })
        });
        if let Err(e) = saved {
            eprintln!("xsh: transcript not saved: {e}");
        }
    }
}

/// Names of `*.toml` files in the themes directory.
pub fn user_themes(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension().is_some_and(|x| x == "toml"))
                        .then(|| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                        .flatten()
                })
                .collect()
        })
        .unwrap_or_default()
}

fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn scan_path(path: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    for dir in std::env::split_paths(path) {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            if is_exec(&e.path()) {
                out.insert(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    out.into_iter().collect()
}
