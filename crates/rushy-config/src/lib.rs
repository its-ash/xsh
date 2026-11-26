//! Layered configuration loading for xsh.
//!
//! Layers merge in this order, later layers winning:
//! built-in defaults → user `config.toml` → project `.xsh.toml` in the
//! working directory → `XSH_<SECTION>__<KEY>` environment variables → CLI flags.
//!
//! Every file is validated before merging, so a typo reports the file, line,
//! and column instead of being silently ignored. The project layer may only set
//! `[shell]`, `[ui]`, and `[mux]`: a cloned repository must not be able to
//! change the bundle registry or history settings.
//!
//! ```
//! # fn main() -> rushy_config::Result<()> {
//! let dir = tempfile::tempdir()?;
//! let paths = rushy_config::Paths::new(dir.path().join("cfg"), dir.path().join("data"));
//! let cfg = rushy_config::Loader::new(paths).load()?;
//! assert_eq!(cfg.mux.prefix, "C-b");
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::pedantic)]

mod error;
mod paths;

use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use error::{Error, Result};
pub use paths::Paths;

/// Default bundle registry index.
pub const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/its-ash/xsh/main/registry.json";

/// Hosts bundle downloads may come from unless the user config overrides them.
pub const DEFAULT_ALLOWED_HOSTS: &[&str] = &[
    "raw.githubusercontent.com",
    "github.com",
    "objects.githubusercontent.com",
];

/// Name of the per-project config file looked up in the working directory.
pub const PROJECT_FILE: &str = ".xsh.toml";

const ENV_SECTIONS: &[&str] = &[
    "shell", "ui", "mux", "bundles", "history", "window", "font", "colors", "terminal", "keys",
    "updates",
];

/// Fully merged configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Shell behaviour.
    pub shell: ShellConfig,
    /// Rendering and accessibility.
    pub ui: UiConfig,
    /// Multiplexer settings.
    pub mux: MuxConfig,
    /// Bundle registry and activation.
    pub bundles: BundlesConfig,
    /// Persistent history.
    pub history: HistoryConfig,
    /// macOS app: windows and tabs.
    pub window: WindowConfig,
    /// macOS app: terminal font.
    pub font: FontConfig,
    /// macOS app: color overrides on top of the theme.
    pub colors: ColorsConfig,
    /// macOS app: terminal behaviour.
    pub terminal: TerminalConfig,
    /// macOS app: keyboard shortcuts.
    pub keys: KeysConfig,
    /// macOS app: new versions.
    pub updates: UpdatesConfig,
    /// macOS app: extra session profiles.
    pub profiles: Vec<Profile>,
}

impl Config {
    /// Checks semantic and range constraints on configuration values.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Extract`] if any setting is outside its allowed range.
    pub fn validate(&self) -> Result<()> {
        if !(10..=100).contains(&self.window.opacity) {
            return Err(Error::Extract(format!(
                "window.opacity must be in 10..=100, got {}",
                self.window.opacity
            )));
        }
        if !(1..=21).contains(&self.font.min_contrast) {
            return Err(Error::Extract(format!(
                "font.min_contrast must be in 1..=21, got {}",
                self.font.min_contrast
            )));
        }
        if !(20..=500).contains(&self.window.columns) {
            return Err(Error::Extract(format!(
                "window.columns must be in 20..=500, got {}",
                self.window.columns
            )));
        }
        if !(5..=200).contains(&self.window.rows) {
            return Err(Error::Extract(format!(
                "window.rows must be in 5..=200, got {}",
                self.window.rows
            )));
        }
        if !(6..=96).contains(&self.font.size) {
            return Err(Error::Extract(format!(
                "font.size must be in 6..=96, got {}",
                self.font.size
            )));
        }
        Ok(())
    }
}

/// `[updates]` section of the macOS app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdatesConfig {
    /// Look for a new version once a day when the app starts.
    pub auto_check: bool,
}

impl Default for UpdatesConfig {
    fn default() -> Self {
        Self { auto_check: true }
    }
}

/// `[window]` section of the macOS app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools, reason = "independent TOML switches")]
pub struct WindowConfig {
    /// Columns of a new window.
    pub columns: u16,
    /// Rows of a new window.
    pub rows: u16,
    /// Background opacity in percent (10 to 100).
    pub opacity: u8,
    /// Blur what is behind a translucent window.
    pub blur: bool,
    /// Space around the terminal text, in points.
    pub padding: u16,
    /// App appearance: `system`, `light`, or `dark`.
    pub appearance: String,
    /// Theme for light mode; empty uses `ui.theme`.
    pub light_theme: String,
    /// Theme for dark mode; empty uses `ui.theme`.
    pub dark_theme: String,
    /// Title bar: `native`, `transparent`, or `hidden`.
    pub titlebar: String,
    /// Tab bar: `auto` (with two or more tabs) or `always`.
    pub tab_bar: String,
    /// Keep windows above other apps.
    pub always_on_top: bool,
    /// Quit the app when the last window closes.
    pub quit_when_last_closed: bool,
    /// Dim the panes that do not have focus.
    pub dim_inactive_panes: bool,
    /// Open the same tabs and folders again at launch.
    pub restore_session: bool,
    /// Ask before close: `processes` (when a program runs), `always`, or `never`.
    pub confirm_close: String,
    /// When the shell exits: `close`, `close_clean` (only with status 0), or `keep`.
    pub on_exit: String,
    /// Folder of a new tab: `previous`, `home`, or `custom`.
    pub new_tab_directory: String,
    /// Folder used when `new_tab_directory` is `custom`.
    pub custom_directory: String,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            columns: 120,
            rows: 34,
            opacity: 100,
            blur: false,
            padding: 6,
            appearance: "system".to_owned(),
            light_theme: String::new(),
            dark_theme: String::new(),
            titlebar: "native".to_owned(),
            tab_bar: "auto".to_owned(),
            always_on_top: false,
            quit_when_last_closed: false,
            dim_inactive_panes: false,
            restore_session: true,
            confirm_close: "processes".to_owned(),
            on_exit: "close".to_owned(),
            new_tab_directory: "previous".to_owned(),
            custom_directory: String::new(),
        }
    }
}

/// `[font]` section of the macOS app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FontConfig {
    /// Font family; empty, or a family that is not installed, uses the
    /// system monospaced font.
    pub family: String,
    /// Size in points.
    pub size: u16,
    /// Line height in percent of the font height.
    pub line_spacing: u16,
    /// Smooth the edges of the glyphs.
    pub smoothing: bool,
    /// Show bold text in the bright colors.
    pub bright_bold: bool,
    /// Draw box and block characters with built-in shapes.
    pub custom_block_glyphs: bool,
    /// Lowest text contrast ratio (1 means off, 21 is the maximum).
    pub min_contrast: u8,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: "Consolas".to_owned(),
            size: 13,
            line_spacing: 100,
            smoothing: true,
            bright_bold: true,
            custom_block_glyphs: true,
            min_contrast: 1,
        }
    }
}

/// `[colors]` section of the macOS app. Empty values use the theme.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ColorsConfig {
    /// Background, `#rrggbb`.
    pub background: String,
    /// Default text, `#rrggbb`.
    pub foreground: String,
    /// Cursor, `#rrggbb`.
    pub cursor: String,
    /// Selection background, `#rrggbb`.
    pub selection: String,
    /// The 16 ANSI colors, `#rrggbb`; empty uses the theme.
    pub ansi: Vec<String>,
}

/// `[terminal]` section of the macOS app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools, reason = "independent TOML switches")]
pub struct TerminalConfig {
    /// Program each session runs; empty means the bundled `xsh`.
    pub shell: String,
    /// Arguments for the program.
    pub args: Vec<String>,
    /// Start the program as a login shell.
    pub login_shell: bool,
    /// Extra environment variables, each as `NAME=value`.
    pub env: Vec<String>,
    /// Value of `TERM`.
    pub term: String,
    /// Lines kept in the scrollback (0 turns it off).
    pub scrollback: u32,
    /// Cursor shape: `block`, `underline`, or `bar`.
    pub cursor_style: String,
    /// Make the cursor blink.
    pub cursor_blink: bool,
    /// The Option key sends Meta (`ESC` plus the key).
    pub option_as_meta: bool,
    /// Programs can read the mouse.
    pub mouse_reporting: bool,
    /// Copy text when it is selected.
    pub copy_on_select: bool,
    /// Ask before a paste with more than one line.
    pub paste_warning: bool,
    /// Programs can read the clipboard with OSC 52. A remote program over
    /// `ssh` can do it too, so this is off by default.
    pub clipboard_read: bool,
    /// Bell: `none`, `sound`, `visual`, or `both`.
    pub bell: String,
    /// Links open on: `command_click` or `click`.
    pub links: String,
    /// The Delete key sends `Ctrl-H`.
    pub backspace_ctrl_h: bool,
    /// Scroll speed in percent.
    pub scroll_speed: u16,
    /// Scroll bar: `overlay` or `legacy`.
    pub scroller: String,
    /// Draw with the GPU (Metal).
    pub gpu: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            shell: String::new(),
            args: Vec::new(),
            login_shell: true,
            env: Vec::new(),
            term: "xterm-256color".to_owned(),
            scrollback: 10_000,
            cursor_style: "block".to_owned(),
            cursor_blink: true,
            option_as_meta: true,
            mouse_reporting: true,
            copy_on_select: false,
            paste_warning: true,
            clipboard_read: false,
            bell: "visual".to_owned(),
            links: "command_click".to_owned(),
            backspace_ctrl_h: false,
            scroll_speed: 100,
            scroller: "overlay".to_owned(),
            gpu: true,
        }
    }
}

/// `[keys]` section of the macOS app: shortcuts such as `cmd+shift+d`.
/// An empty value turns a shortcut off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeysConfig {
    /// Show or hide the app from any other app; empty turns it off.
    pub global_hotkey: String,
    /// New window.
    pub new_window: String,
    /// New tab.
    pub new_tab: String,
    /// Close the pane, then the tab.
    pub close: String,
    /// Split to the right.
    pub split_right: String,
    /// Split downward.
    pub split_down: String,
    /// Make all panes in the tab the same size.
    pub equalize_panes: String,
    /// Focus the next pane.
    pub next_pane: String,
    /// Focus the previous pane.
    pub previous_pane: String,
    /// Focus the pane to the left.
    pub focus_left: String,
    /// Focus the pane to the right.
    pub focus_right: String,
    /// Focus the pane above.
    pub focus_up: String,
    /// Focus the pane below.
    pub focus_down: String,
    /// Next tab.
    pub next_tab: String,
    /// Previous tab.
    pub previous_tab: String,
    /// Find.
    pub find: String,
    /// Find the next match.
    pub find_next: String,
    /// Find the previous match.
    pub find_previous: String,
    /// Clear the scrollback.
    pub clear_scrollback: String,
    /// Reset the terminal.
    pub reset: String,
    /// Bigger text.
    pub font_bigger: String,
    /// Smaller text.
    pub font_smaller: String,
    /// Default text size.
    pub font_reset: String,
    /// Full screen.
    pub fullscreen: String,
    /// Open the settings.
    pub settings: String,
}

impl Default for KeysConfig {
    fn default() -> Self {
        let k = str::to_owned;
        Self {
            global_hotkey: String::new(),
            new_window: k("cmd+n"),
            new_tab: k("cmd+t"),
            close: k("cmd+w"),
            split_right: k("cmd+d"),
            split_down: k("cmd+shift+d"),
            equalize_panes: k("cmd+ctrl+e"),
            next_pane: k("cmd+]"),
            previous_pane: k("cmd+["),
            focus_left: k("cmd+alt+left"),
            focus_right: k("cmd+alt+right"),
            focus_up: k("cmd+alt+up"),
            focus_down: k("cmd+alt+down"),
            next_tab: k("cmd+shift+]"),
            previous_tab: k("cmd+shift+["),
            find: k("cmd+f"),
            find_next: k("cmd+g"),
            find_previous: k("cmd+shift+g"),
            clear_scrollback: k("cmd+k"),
            reset: k("cmd+shift+r"),
            font_bigger: k("cmd+="),
            font_smaller: k("cmd+-"),
            font_reset: k("cmd+0"),
            fullscreen: k("cmd+ctrl+f"),
            settings: k("cmd+,"),
        }
    }
}

/// `[[profiles]]`: a named way to start a session in the macOS app. Empty
/// fields use the normal settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    /// Name in the menu.
    pub name: String,
    /// Program to run.
    pub shell: String,
    /// Arguments for the program.
    pub args: Vec<String>,
    /// Working folder.
    pub directory: String,
    /// Theme.
    pub theme: String,
    /// Font family.
    pub font_family: String,
    /// Font size in points; 0 uses the normal size.
    pub font_size: u16,
    /// Extra environment variables, each as `NAME=value`.
    pub env: Vec<String>,
}

/// `[shell]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools, reason = "independent TOML switches")]
pub struct ShellConfig {
    /// Tee foreground stderr so `ai fix` can see it.
    pub capture_stderr: bool,
    /// Line editor key mode: `emacs` or `vi`.
    pub edit_mode: String,
    /// Show a gray inline suggestion from history while typing.
    pub autosuggest: bool,
    /// A line that names only a directory changes to it.
    pub auto_cd: bool,
    /// Directories `cd NAME` also searches.
    pub cdpath: Vec<String>,
    /// Ask before `exit` or `Ctrl-D` when background jobs run.
    pub confirm_exit_with_jobs: bool,
    /// Print a notice when a command runs longer than this many seconds (0 = off).
    pub notify_after_secs: u64,
    /// Set the terminal title to the command and directory, and send OSC 7 and OSC 133 marks.
    pub terminal_integration: bool,
    /// Preserve theme colors, prompt integration, and live syntax highlighting over interactive SSH.
    pub ssh_integration: bool,
    /// Suggest close command names when a command is not found.
    pub suggest_typos: bool,
    /// Script run at start-up, relative to the config directory.
    pub rc_file: String,
    /// Import env and aliases from the login shell rc file: `auto`, `zsh`, `bash`, or `off`.
    pub import_rc: String,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            capture_stderr: true,
            edit_mode: "emacs".to_owned(),
            autosuggest: true,
            auto_cd: true,
            cdpath: Vec::new(),
            confirm_exit_with_jobs: true,
            notify_after_secs: 10,
            terminal_integration: true,
            ssh_integration: true,
            suggest_typos: true,
            rc_file: "init.xsh".to_owned(),
            import_rc: "auto".to_owned(),
        }
    }
}

/// `[ui]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools, reason = "independent TOML switches")]
pub struct UiConfig {
    /// Theme name: a built-in or a file in the themes directory.
    pub theme: String,
    /// Plain ASCII borders, no animations, no decorative glyphs.
    pub screen_reader: bool,
    /// Disable all colors (also enabled by the `NO_COLOR` variable).
    pub no_color: bool,
    /// Show the multiplexer status bar.
    pub status_bar: bool,
    /// Reload theme and bundles when the config directory changes.
    pub hot_reload: bool,
    /// Color the command line as it is typed.
    pub highlight: bool,
    /// Show the exit status and duration of the last command on the right.
    pub right_prompt: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "neon".to_owned(),
            screen_reader: false,
            no_color: false,
            status_bar: true,
            hot_reload: true,
            highlight: true,
            right_prompt: true,
        }
    }
}

/// `[mux]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MuxConfig {
    /// Prefix key, such as `C-b` or `C-a`.
    pub prefix: String,
    /// Program that each new pane runs; empty means the `rushy` binary.
    pub shell: String,
}

impl Default for MuxConfig {
    fn default() -> Self {
        Self {
            prefix: "C-b".to_owned(),
            shell: String::new(),
        }
    }
}

/// `[bundles]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BundlesConfig {
    /// Registry index URL (`https://` or, for local registries, `file://`).
    pub registry_url: String,
    /// Hosts bundle files may be downloaded from.
    pub allowed_hosts: Vec<String>,
    /// Run `[[hooks]]` commands on activation (after the safety guard).
    pub allow_hooks: bool,
    /// Installed bundles that never activate.
    pub disabled: Vec<String>,
}

impl Default for BundlesConfig {
    fn default() -> Self {
        Self {
            registry_url: DEFAULT_REGISTRY_URL.to_owned(),
            allowed_hosts: DEFAULT_ALLOWED_HOSTS
                .iter()
                .map(|h| (*h).to_owned())
                .collect(),
            allow_hooks: true,
            disabled: Vec::new(),
        }
    }
}

/// `[history]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    /// Record commands to the encrypted store.
    pub enabled: bool,
    /// Entries kept in the line editor for `Ctrl-R`.
    pub editor_entries: usize,
    /// Where the encryption key comes from: `auto` (OS keychain, then the
    /// `XSH_HISTORY_PASSPHRASE` variable), `keyring`, or `passphrase`.
    pub key_source: String,
    /// Do not record a command that repeats the one before it.
    pub ignore_dups: bool,
    /// Do not record a command that starts with a space.
    pub ignore_space: bool,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            editor_entries: 5_000,
            key_source: "auto".to_owned(),
            ignore_dups: true,
            ignore_space: true,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(dead_code, clippy::struct_excessive_bools)]
struct ProjectShellConfig {
    capture_stderr: Option<bool>,
    edit_mode: Option<String>,
    autosuggest: Option<bool>,
    auto_cd: Option<bool>,
    confirm_exit_with_jobs: Option<bool>,
    notify_after_secs: Option<u64>,
    terminal_integration: Option<bool>,
    ssh_integration: Option<bool>,
    suggest_typos: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(dead_code)]
struct ProjectMuxConfig {
    prefix: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct ProjectConfig {
    #[serde(default)]
    shell: Option<ProjectShellConfig>,
    #[serde(default)]
    ui: Option<UiConfig>,
    #[serde(default)]
    mux: Option<ProjectMuxConfig>,
}

/// Values from command-line flags, the highest-priority layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `--theme NAME`.
    pub theme: Option<String>,
    /// `--screen-reader`.
    pub screen_reader: Option<bool>,
    /// `--no-color`.
    pub no_color: Option<bool>,
}

#[derive(Serialize)]
struct UiLayer<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    theme: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    screen_reader: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    no_color: Option<bool>,
}

#[derive(Serialize)]
struct CliLayer<'a> {
    ui: UiLayer<'a>,
}

/// Builds a [`Config`] from all layers.
#[derive(Debug, Clone)]
pub struct Loader {
    paths: Paths,
    cwd: Option<PathBuf>,
    overrides: Overrides,
    use_env: bool,
}

impl Loader {
    /// Loader reading the user config under `paths`.
    #[must_use]
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            cwd: None,
            overrides: Overrides::default(),
            use_env: true,
        }
    }

    /// Also reads `.xsh.toml` from `cwd`.
    #[must_use]
    pub fn project(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Applies CLI flag overrides.
    #[must_use]
    pub fn overrides(mut self, overrides: Overrides) -> Self {
        self.overrides = overrides;
        self
    }

    /// Enables or disables the `XSH_*` environment layer.
    #[must_use]
    pub fn env(mut self, enabled: bool) -> Self {
        self.use_env = enabled;
        self
    }

    /// Paths this loader reads from.
    #[must_use]
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Validates and merges every layer.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Invalid`] with a line and column for a bad file, and
    /// [`Error::Extract`] when the merged result is invalid.
    pub fn load(&self) -> Result<Config> {
        let user = self.paths.config_file();
        let user_text = read_optional(&user)?;
        let project = self.cwd.as_ref().map(|cwd| cwd.join(PROJECT_FILE));
        let project_text = match &project {
            Some(p) => read_optional(p)?,
            None => None,
        };
        let env_hit = self.use_env && has_env_override();
        // Skip Figment's default-serialization roundtrip when no project file
        // or `XSH_*` env override needs merging.
        if project_text.is_none() && !env_hit {
            let mut cfg = match &user_text {
                Some(text) => parse_toml::<Config>(&user, text)?,
                None => Config::default(),
            };
            if let Some(t) = &self.overrides.theme {
                cfg.ui.theme.clone_from(t);
            }
            if let Some(sr) = self.overrides.screen_reader {
                cfg.ui.screen_reader = sr;
            }
            if let Some(nc) = self.overrides.no_color {
                cfg.ui.no_color = nc;
            }
            cfg.validate()?;
            if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
                cfg.ui.no_color = true;
            }
            return Ok(cfg);
        }
        let mut fig = Figment::from(Serialized::defaults(Config::default()));
        if let Some(text) = &user_text {
            parse_toml::<Config>(&user, text)?;
            fig = fig.merge(Toml::string(text));
        }
        if let (Some(project), Some(text)) = (&project, &project_text) {
            parse_toml::<ProjectConfig>(project, text)?;
            fig = fig.merge(Toml::string(text));
        }
        if env_hit {
            fig = fig.merge(
                Env::prefixed("XSH_")
                    .filter(|k| {
                        let k = k.as_str().to_ascii_lowercase();
                        ENV_SECTIONS
                            .iter()
                            .any(|s| k.strip_prefix(s).is_some_and(|r| r.starts_with("__")))
                    })
                    .split("__"),
            );
        }
        let o = &self.overrides;
        fig = fig.merge(Serialized::defaults(CliLayer {
            ui: UiLayer {
                theme: o.theme.as_deref(),
                screen_reader: o.screen_reader,
                no_color: o.no_color,
            },
        }));
        let mut cfg: Config = fig.extract().map_err(|e| Error::Extract(e.to_string()))?;
        cfg.validate()?;
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            cfg.ui.no_color = true;
        }
        Ok(cfg)
    }
}

fn has_env_override() -> bool {
    std::env::vars_os().any(|(k, _)| {
        k.to_str().is_some_and(|s| {
            let lower = s.to_ascii_lowercase();
            lower.strip_prefix("xsh_").is_some_and(|rest| {
                ENV_SECTIONS
                    .iter()
                    .any(|sec| rest.strip_prefix(sec).is_some_and(|r| r.starts_with("__")))
            })
        })
    })
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Parses TOML into `T`, reporting errors with the file's line and column.
///
/// ```
/// #[derive(serde::Deserialize)]
/// struct T { n: u8 }
/// let err = rushy_config::parse_toml::<T>("x.toml".as_ref(), "\nn = 'a'").err();
/// assert!(err.is_some_and(|e| e.to_string().starts_with("x.toml:2:")));
/// ```
///
/// # Errors
///
/// Returns [`Error::Invalid`] when the text is not valid TOML for `T`.
pub fn parse_toml<T: DeserializeOwned>(path: &Path, text: &str) -> Result<T> {
    toml_edit::de::from_str(text).map_err(|e| {
        let start = e.span().map_or(0, |s| s.start).min(text.len());
        let before = text.get(..start).unwrap_or_default();
        let line = before.matches('\n').count() + 1;
        // Columns count characters, not bytes, so non-ASCII text before the
        // error does not push the column past where an editor shows it.
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let column = before.get(line_start..).map_or(0, |s| s.chars().count()) + 1;
        Error::Invalid {
            path: path.to_path_buf(),
            line,
            column,
            message: e.message().trim().to_owned(),
        }
    })
}

fn write_atomic(path: &Path, content: &str) -> Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let pid = std::process::id();
    let base = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("config.toml");

    let (tmp_path, mut file) = loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = dir.join(format!(".{base}.tmp.{pid}.{n}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(f) => break (candidate, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    };

    let write_res = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    Ok(())
}

/// Sets `ui.theme` in the user's `config.toml`, keeping all other content and
/// comments, and creating the file if needed.
///
/// # Errors
///
/// Fails if the existing file is invalid or cannot be written.
pub fn set_theme(paths: &Paths, name: &str) -> Result<()> {
    let file = paths.config_file();
    let text = read_optional(&file)?.unwrap_or_default();
    parse_toml::<Config>(&file, &text)?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Extract(e.to_string()))?;
    ensure_section(&mut doc, "ui");
    doc["ui"]["theme"] = toml_edit::value(name);
    write_atomic(&file, &doc.to_string())
}

/// Replaces the `[[profiles]]` of the user's `config.toml`, keeping all other
/// content and comments.
///
/// # Errors
///
/// Fails if the file is invalid, the result does not validate, or a write fails.
pub fn set_profiles(paths: &Paths, profiles: &[Profile]) -> Result<()> {
    #[derive(Serialize)]
    struct Wrap<'a> {
        profiles: &'a [Profile],
    }
    let file = paths.config_file();
    let text = read_optional(&file)?.unwrap_or_default();
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Extract(e.to_string()))?;
    doc.remove("profiles");
    if !profiles.is_empty() {
        let new = toml_edit::ser::to_document(&Wrap { profiles })
            .map_err(|e| Error::Extract(e.to_string()))?;
        if let Some(item) = new.get("profiles") {
            doc["profiles"] = item.clone();
        }
    }
    let new = doc.to_string();
    parse_toml::<Config>(&file, &new)?.validate()?;
    write_atomic(&file, &new)
}

/// Every `section.key` name that [`get_value`] and [`set_value`] accept.
///
/// # Errors
///
/// Fails only if the defaults cannot be serialized.
pub fn keys() -> Result<Vec<String>> {
    let doc = to_doc(&Config::default())?;
    let mut out = Vec::new();
    for (section, item) in doc.iter() {
        if let Some(t) = item.as_table_like() {
            out.extend(t.iter().map(|(k, _)| format!("{section}.{k}")));
        }
    }
    Ok(out)
}

fn to_doc(cfg: &Config) -> Result<toml_edit::DocumentMut> {
    toml_edit::ser::to_document(cfg).map_err(|e| Error::Extract(e.to_string()))
}

/// Makes `section` a table unless it already is one. An inline table
/// (`ui = { theme = "x" }`) counts as one, so its other keys are kept.
fn ensure_section(doc: &mut toml_edit::DocumentMut, section: &str) {
    if !doc.get(section).is_some_and(toml_edit::Item::is_table_like) {
        doc[section] = toml_edit::table();
    }
}

fn split_key(key: &str) -> Result<(&str, &str)> {
    key.split_once('.')
        .filter(|(s, k)| !s.is_empty() && !k.is_empty() && !k.contains('.'))
        .ok_or_else(|| Error::Extract(format!("`{key}`: use the form section.key")))
}

/// The value of `section.key` in `cfg`, as TOML text.
///
/// # Errors
///
/// Returns [`Error::Extract`] for an unknown key.
pub fn get_value(cfg: &Config, key: &str) -> Result<String> {
    let (section, name) = split_key(key)?;
    let doc = to_doc(cfg)?;
    doc.get(section)
        .and_then(|t| t.get(name))
        .and_then(toml_edit::Item::as_value)
        .map(|v| v.to_string().trim().to_owned())
        .ok_or_else(|| Error::Extract(format!("unknown setting `{key}`")))
}

/// Sets `section.key` in the user's `config.toml`, keeping comments. `raw` is
/// TOML (`true`, `10`, `["a"]`); anything else is stored as a string. The
/// result is validated before the file is written.
///
/// # Errors
///
/// Fails for an unknown key, a value of the wrong type, or a write error.
pub fn set_value(paths: &Paths, key: &str, raw: &str) -> Result<()> {
    let (section, name) = split_key(key)?;
    let file = paths.config_file();
    let text = read_optional(&file)?.unwrap_or_default();
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Extract(e.to_string()))?;
    let value = raw
        .parse::<toml_edit::Value>()
        .unwrap_or_else(|_| toml_edit::Value::from(raw));
    let is_str = value.is_str();
    ensure_section(&mut doc, section);
    doc[section][name] = toml_edit::Item::Value(value);
    let mut new = doc.to_string();
    if let Err(e) = parse_toml::<Config>(&file, &new).and_then(|c| c.validate()) {
        // `1984` or `true` for a text setting: keep the text as typed.
        if is_str {
            return Err(e);
        }
        doc[section][name] = toml_edit::value(raw);
        new = doc.to_string();
        if parse_toml::<Config>(&file, &new)
            .and_then(|c| c.validate())
            .is_err()
        {
            return Err(e);
        }
    }
    write_atomic(&file, &new)
}

/// Removes `section.key` from the user's `config.toml`, so the default applies.
///
/// # Errors
///
/// Fails if the file is invalid or cannot be written.
pub fn unset_value(paths: &Paths, key: &str) -> Result<()> {
    let (section, name) = split_key(key)?;
    let file = paths.config_file();
    let Some(text) = read_optional(&file)? else {
        return Ok(());
    };
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| Error::Extract(e.to_string()))?;
    if let Some(t) = doc
        .get_mut(section)
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        t.remove(name);
    }
    write_atomic(&file, &doc.to_string())
}

/// Reads and parses a TOML file with [`parse_toml`].
///
/// # Errors
///
/// Fails if the file cannot be read or does not parse.
pub fn load_toml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path)?;
    parse_toml(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> std::io::Result<(tempfile::TempDir, Paths)> {
        let dir = tempfile::tempdir()?;
        let paths = Paths::new(dir.path().join("cfg"), dir.path().join("data"));
        std::fs::create_dir_all(paths.config_dir())?;
        Ok((dir, paths))
    }

    #[test]
    fn layers_merge_in_order() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (dir, paths) = setup()?;
        std::fs::write(
            paths.config_file(),
            "[ui]\ntheme = \"user\"\nstatus_bar = false\n[mux]\nprefix = \"C-a\"\n",
        )?;
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj)?;
        std::fs::write(proj.join(PROJECT_FILE), "[ui]\ntheme = \"project\"\n")?;
        let base = Loader::new(paths).env(false).project(&proj);
        let cfg = base.load()?;
        assert_eq!(cfg.ui.theme, "project");
        assert!(!cfg.ui.status_bar);
        assert_eq!(cfg.mux.prefix, "C-a");
        let cfg = base
            .overrides(Overrides {
                theme: Some("mono".into()),
                ..Overrides::default()
            })
            .load()?;
        assert_eq!(cfg.ui.theme, "mono");
        Ok(())
    }

    #[test]
    fn errors_report_line_and_column() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_dir, paths) = setup()?;
        std::fs::write(paths.config_file(), "[ui]\nthme = \"x\"\n")?;
        match Loader::new(paths).env(false).load() {
            Err(Error::Invalid { line, column, .. }) => {
                assert_eq!((line, column), (2, 1));
                Ok(())
            }
            other => Err(format!("expected an Invalid error, got {other:?}").into()),
        }
    }

    #[test]
    fn set_theme_keeps_other_content() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_dir, paths) = setup()?;
        std::fs::write(paths.config_file(), "# mine\n[mux]\nprefix = \"C-a\"\n")?;
        set_theme(&paths, "mono")?;
        let text = std::fs::read_to_string(paths.config_file())?;
        assert!(
            text.contains("# mine") && text.contains("prefix = \"C-a\""),
            "{text}"
        );
        let cfg = Loader::new(paths.clone()).env(false).load()?;
        assert_eq!(
            (cfg.ui.theme.as_str(), cfg.mux.prefix.as_str()),
            ("mono", "C-a")
        );
        set_theme(&paths, "tokyo-night")?;
        assert_eq!(
            Loader::new(paths).env(false).load()?.ui.theme,
            "tokyo-night"
        );
        Ok(())
    }

    #[test]
    fn get_and_set_values() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_dir, paths) = setup()?;
        set_value(&paths, "shell.edit_mode", "vi")?;
        set_value(&paths, "shell.notify_after_secs", "30")?;
        let cfg = Loader::new(paths.clone()).env(false).load()?;
        assert_eq!(get_value(&cfg, "shell.edit_mode")?, "\"vi\"");
        assert_eq!(cfg.shell.notify_after_secs, 30);
        assert!(set_value(&paths, "shell.notify_after_secs", "\"x\"").is_err());
        assert!(set_value(&paths, "shell.nope", "1").is_err());
        assert!(get_value(&cfg, "nope").is_err());
        unset_value(&paths, "shell.edit_mode")?;
        let cfg = Loader::new(paths).env(false).load()?;
        assert_eq!(cfg.shell.edit_mode, "emacs");
        assert!(keys()?.iter().any(|k| k == "ui.highlight"));
        Ok(())
    }

    /// Regression: `set_value` and `set_theme` replaced an inline-table
    /// section with an empty table, dropping the user's other settings.
    #[test]
    fn set_value_keeps_inline_table_section() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let (_dir, paths) = setup()?;
        std::fs::write(
            paths.config_file(),
            "ui = { theme = \"nord\", status_bar = false }\n",
        )?;
        set_value(&paths, "ui.highlight", "false")?;
        let cfg = Loader::new(paths.clone()).env(false).load()?;
        assert_eq!(cfg.ui.theme, "nord");
        assert!(!cfg.ui.status_bar);
        assert!(!cfg.ui.highlight);
        set_theme(&paths, "mono")?;
        let cfg = Loader::new(paths.clone()).env(false).load()?;
        assert_eq!(cfg.ui.theme, "mono");
        assert!(!cfg.ui.status_bar);
        unset_value(&paths, "ui.status_bar")?;
        let cfg = Loader::new(paths).env(false).load()?;
        assert!(
            cfg.ui.status_bar,
            "unset must remove keys from inline tables too"
        );
        assert_eq!(cfg.ui.theme, "mono");
        Ok(())
    }

    /// Regression: a text setting whose value looks like a number or a
    /// boolean was rejected as the wrong type.
    #[test]
    fn set_value_keeps_number_like_text() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_dir, paths) = setup()?;
        set_value(&paths, "ui.theme", "1984")?;
        set_value(&paths, "mux.shell", "true")?;
        let cfg = Loader::new(paths.clone()).env(false).load()?;
        assert_eq!(cfg.ui.theme, "1984");
        assert_eq!(cfg.mux.shell, "true");
        // Typed settings still reject text and out-of-range numbers.
        assert!(set_value(&paths, "window.opacity", "300").is_err());
        assert!(set_value(&paths, "ui.highlight", "maybe").is_err());
        assert!(set_value(&paths, "font.size", "-1").is_err());
        let cfg = Loader::new(paths).env(false).load()?;
        assert_eq!(cfg.window.opacity, 100);
        Ok(())
    }

    /// Regression: the column counted bytes, so text such as `é` before the
    /// error moved it to the right.
    #[test]
    fn error_column_counts_characters() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("x.toml");
        match parse_toml::<Config>(path, "[ui]\ntheme = \"é\" x\n") {
            Err(Error::Invalid { line, column, .. }) => {
                assert_eq!((line, column), (2, 13));
                Ok(())
            }
            other => Err(format!("expected an Invalid error, got {other:?}").into()),
        }
    }

    #[test]
    fn malformed_files_are_errors_not_panics() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let path = Path::new("x.toml");
        for text in [
            "[ui",
            "[ui]\ntheme = ",
            "[ui]\ntheme = \"unterminated",
            "[ui]\n[ui]\n",
            "[ui]\nopacity = 1\n",
            "[window]\nopacity = 1000\n",
            "[window]\ncolumns = -1\n",
            "\u{feff}[ui]\ntheme = 3\n",
            "profiles = 3\n",
            "[[ui]]\n",
        ] {
            assert!(
                matches!(parse_toml::<Config>(path, text), Err(Error::Invalid { .. })),
                "{text:?} should be rejected"
            );
        }
        // Empty and comment-only files are the defaults.
        assert_eq!(parse_toml::<Config>(path, "")?, Config::default());
        assert_eq!(parse_toml::<Config>(path, "# hi\n\n")?, Config::default());
        Ok(())
    }

    #[test]
    fn bad_keys_are_rejected() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_dir, paths) = setup()?;
        for key in [
            "",
            ".",
            "ui",
            "ui.",
            ".theme",
            "ui.theme.x",
            "profiles.name",
        ] {
            assert!(set_value(&paths, key, "x").is_err(), "{key:?}");
        }
        assert!(!paths.config_file().exists() || load_toml::<Config>(&paths.config_file()).is_ok());
        let cfg = Config::default();
        assert!(get_value(&cfg, "ui.theme.x").is_err());
        assert!(get_value(&cfg, "profiles.name").is_err());
        Ok(())
    }

    #[test]
    fn profiles_round_trip() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_dir, paths) = setup()?;
        std::fs::write(paths.config_file(), "# keep\n[ui]\ntheme = \"nord\"\n")?;
        let p = Profile {
            name: "Ünïcode ✓".into(),
            args: vec!["-l".into()],
            ..Profile::default()
        };
        set_profiles(&paths, std::slice::from_ref(&p))?;
        let cfg = Loader::new(paths.clone()).env(false).load()?;
        assert_eq!(cfg.profiles, vec![p]);
        assert_eq!(cfg.ui.theme, "nord");
        set_profiles(&paths, &[])?;
        let cfg = Loader::new(paths.clone()).env(false).load()?;
        assert!(cfg.profiles.is_empty());
        assert!(std::fs::read_to_string(paths.config_file())?.contains("# keep"));
        Ok(())
    }

    #[test]
    fn project_may_set_ui_but_not_history() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (dir, paths) = setup()?;
        std::fs::write(
            dir.path().join(PROJECT_FILE),
            "[history]\nenabled = false\n",
        )?;
        let res = Loader::new(paths.clone())
            .env(false)
            .project(dir.path())
            .load();
        assert!(matches!(res, Err(Error::Invalid { .. })));
        // A missing project file is fine.
        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty)?;
        let cfg = Loader::new(paths).env(false).project(&empty).load()?;
        assert_eq!(cfg, Config::default());
        Ok(())
    }

    #[test]
    fn project_cannot_change_registry() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (dir, paths) = setup()?;
        std::fs::write(
            dir.path().join(PROJECT_FILE),
            "[bundles]\nregistry_url = \"https://evil.example\"\n",
        )?;
        let res = Loader::new(paths).env(false).project(dir.path()).load();
        assert!(matches!(res, Err(Error::Invalid { line: 1, .. })));
        Ok(())
    }

    #[test]
    fn project_cannot_set_rce_fields() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (dir, paths) = setup()?;
        for forbidden in [
            "[mux]\nshell = \"/tmp/evil\"\n",
            "[shell]\nrc_file = \"/tmp/evil.xsh\"\n",
            "[shell]\nimport_rc = \"bash\"\n",
            "[shell]\ncdpath = [\"/tmp\"]\n",
        ] {
            std::fs::write(dir.path().join(PROJECT_FILE), forbidden)?;
            let res = Loader::new(paths.clone())
                .env(false)
                .project(dir.path())
                .load();
            assert!(
                matches!(res, Err(Error::Invalid { .. })),
                "{forbidden:?} must be rejected"
            );
        }
        // Safe fields in [mux] and [shell] are still allowed.
        std::fs::write(
            dir.path().join(PROJECT_FILE),
            "[mux]\nprefix = \"C-a\"\n[shell]\nedit_mode = \"vi\"\n",
        )?;
        let cfg = Loader::new(paths).env(false).project(dir.path()).load()?;
        assert_eq!(cfg.mux.prefix, "C-a");
        assert_eq!(cfg.shell.edit_mode, "vi");
        Ok(())
    }

    #[test]
    fn range_validation_rejects_out_of_bounds()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let (_dir, paths) = setup()?;
        for (key, bad) in [
            ("window.opacity", "9"),
            ("window.opacity", "101"),
            ("font.min_contrast", "0"),
            ("font.min_contrast", "22"),
            ("window.columns", "19"),
            ("window.columns", "501"),
            ("window.rows", "4"),
            ("window.rows", "201"),
            ("font.size", "5"),
            ("font.size", "97"),
        ] {
            assert!(
                set_value(&paths, key, bad).is_err(),
                "{key}={bad} should be rejected"
            );
        }
        // Boundary values succeed.
        set_value(&paths, "window.opacity", "10")?;
        set_value(&paths, "font.min_contrast", "21")?;
        set_value(&paths, "window.columns", "20")?;
        set_value(&paths, "window.rows", "5")?;
        set_value(&paths, "font.size", "96")?;
        let cfg = Loader::new(paths).env(false).load()?;
        assert_eq!(cfg.window.opacity, 10);
        assert_eq!(cfg.font.min_contrast, 21);
        assert_eq!(cfg.window.columns, 20);
        assert_eq!(cfg.window.rows, 5);
        assert_eq!(cfg.font.size, 96);
        Ok(())
    }
}
