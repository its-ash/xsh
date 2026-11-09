//! Command-line definitions, shared by the process arguments and the
//! built-in commands typed at the prompt.

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand};

/// AI-native shell and terminal multiplexer.
#[derive(Debug, Parser)]
#[command(
    name = "xsh",
    version,
    about,
    propagate_version = true,
    args_conflicts_with_subcommands = true
)]
#[allow(clippy::struct_excessive_bools, reason = "independent CLI switches")]
pub struct Cli {
    /// Run COMMAND and exit with its status.
    #[arg(short = 'c', value_name = "COMMAND")]
    pub command: Option<String>,

    /// Print each command before it runs (`set -x`).
    #[arg(short = 'x')]
    pub xtrace: bool,

    /// Exit when a command fails (`set -e`).
    #[arg(short = 'e')]
    pub errexit: bool,

    /// Read commands and check syntax, but do not run them.
    #[arg(short = 'n')]
    pub noexec: bool,

    /// Do not run the start-up file (`init.xsh`).
    #[arg(long)]
    pub norc: bool,

    /// Script file to run, then its arguments (`$1`, `$2`, ...).
    #[arg(value_name = "SCRIPT")]
    pub script: Option<PathBuf>,

    /// Arguments for SCRIPT or for `-c`.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        requires = "script"
    )]
    pub args: Vec<String>,

    /// Theme: a built-in name, or a file in the themes directory. `mono` has no colors.
    #[arg(long, global = true, value_name = "NAME")]
    pub theme: Option<String>,

    /// Screen-reader friendly mode: plain ASCII borders and symbols, no animations.
    #[arg(long, global = true)]
    pub screen_reader: bool,

    /// Turn colors off (the `NO_COLOR` variable does the same).
    #[arg(long, global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub sub: Option<Sub>,
}

/// Subcommands.
#[derive(Debug, Subcommand)]
pub enum Sub {
    /// Install, remove, and inspect bundles.
    #[command(subcommand)]
    Bundle(BundleCmd),
    /// AI helpers: explain, fix, suggest, agent.
    #[command(subcommand)]
    Ai(AiCmd),
    /// Show, search, export, and manage the encrypted command history.
    History(HistoryArgs),
    /// List themes, or switch to one and save the choice.
    Theme {
        /// Theme to switch to.
        name: Option<String>,
    },
    /// Attach to a running multiplexer session (default: `main`).
    Attach {
        /// Session name.
        session: Option<String>,
    },
    /// Create a multiplexer session and attach to it (default name: `main`).
    New {
        /// Session name.
        name: Option<String>,
    },
    /// Scan the local network for ADB devices on Wi-Fi.
    AdbFind,
    /// Connect to an ADB device found by `adb-find`.
    AdbConnect,
    /// List multiplexer sessions.
    Ls,
    /// Kill a multiplexer session and its panes.
    KillSession {
        /// Session name.
        name: String,
    },
    /// Show and change settings in `config.toml`.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Run the multiplexer server (started automatically).
    #[command(hide = true)]
    Server,
    /// Bridge for the macOS app.
    #[command(hide = true, subcommand)]
    App(AppCmd),
}

/// `xsh app ...`: what the macOS app uses to read and change settings.
#[derive(Debug, Subcommand)]
pub enum AppCmd {
    /// Print all settings, themes, and bundles as JSON.
    State,
    /// Print the bundle registry as JSON.
    Registry,
    /// Replace the profiles with a JSON list from standard input.
    SetProfiles,
    /// Change a value in `ai.toml`, such as `default.provider`.
    AiSet {
        /// Dotted setting name.
        key: String,
        /// New value as TOML or plain text.
        value: String,
    },
    /// Remove a value from `ai.toml`.
    AiUnset {
        /// Dotted setting name.
        key: String,
    },
    /// Store the API key from standard input in the Keychain (empty removes it).
    AiKey {
        /// Provider name.
        provider: String,
    },
    /// Print the models of a custom provider endpoint as JSON.
    AiModels {
        /// Endpoint, such as `http://127.0.0.1:9694/v1`.
        endpoint: String,
    },
    /// Write a commit subject for the diff on standard input with a model.
    CommitMessage {
        /// OpenAI-compatible endpoint.
        #[arg(long)]
        endpoint: String,
        /// Model name.
        #[arg(long)]
        model: String,
    },
    /// Print whether GitHub has a newer release, as JSON.
    UpdateCheck,
    /// Download and check a release, and print the path of the new app as JSON.
    UpdateDownload {
        /// Version, such as `1.2.3`.
        #[arg(value_name = "VERSION")]
        release: String,
    },
    /// Run the commands of an agent in this pane, then a normal shell.
    AgentRunner {
        /// Socket of the agent.
        socket: PathBuf,
    },
}

/// `xsh config ...`
#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// List every setting with its current value.
    List,
    /// Print one setting, such as `shell.edit_mode`.
    Get {
        /// Setting name: `section.key`.
        key: String,
    },
    /// Change one setting and save it.
    Set {
        /// Setting name: `section.key`.
        key: String,
        /// New value as TOML (`true`, `10`, `["a"]`) or plain text.
        value: String,
    },
    /// Remove a setting so that the default applies again.
    Unset {
        /// Setting name: `section.key`.
        key: String,
    },
    /// Open `config.toml` in `$VISUAL` or `$EDITOR`.
    Edit,
    /// Print the paths of the config file, the start-up file, and the data directory.
    Path,
    /// Check the config files and report errors.
    Check,
}

/// `xsh bundle ...`
#[derive(Debug, Subcommand)]
pub enum BundleCmd {
    /// Install a bundle from the registry (or from a local directory path).
    Install {
        /// Bundle name, or a path that starts with `.` or `/`.
        name: String,
    },
    /// Remove an installed bundle.
    Remove {
        /// Bundle name.
        name: String,
    },
    /// List installed bundles.
    List,
    /// Search the registry.
    Search {
        /// Text to look for in names and descriptions.
        query: String,
    },
    /// Update one bundle or all of them.
    Update(UpdateArgs),
    /// Show details of a bundle.
    Info {
        /// Bundle name.
        name: String,
    },
}

/// Arguments of `bundle update`.
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("target").required(true).args(["name", "all"])))]
pub struct UpdateArgs {
    /// Bundle to update.
    pub name: Option<String>,
    /// Update every installed bundle.
    #[arg(long)]
    pub all: bool,
}

/// `xsh ai ...`
#[derive(Debug, Subcommand)]
pub enum AiCmd {
    /// Explain a command in plain English.
    Explain {
        /// The command to explain.
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Suggest a fix for the last failed command (use it at the xsh prompt).
    Fix,
    /// Turn a description into a command, then ask before running it.
    Suggest {
        /// What you want to do.
        #[arg(required = true, trailing_var_arg = true)]
        description: Vec<String>,
    },
    /// Let the agent work towards a goal, one approved command at a time.
    Agent {
        /// Autopilot: run each command without a question. The safety checks stay on.
        #[arg(long)]
        auto: bool,
        /// The goal.
        #[arg(required = true, trailing_var_arg = true)]
        goal: Vec<String>,
    },
    /// Show which provider and model are configured.
    Status,
}

/// `xsh history ...`
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct HistoryArgs {
    #[command(subcommand)]
    pub cmd: Option<HistoryCmd>,
    /// Show only the last COUNT entries.
    pub count: Option<usize>,
    /// Delete all history.
    #[arg(short = 'c')]
    pub clear: bool,
}

/// History subcommands.
#[derive(Debug, Subcommand)]
pub enum HistoryCmd {
    /// Fuzzy-search the history.
    Search {
        /// Search text.
        #[arg(required = true, trailing_var_arg = true)]
        query: Vec<String>,
        /// Maximum results.
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
    },
    /// Write the history as JSON.
    Export {
        /// Output format.
        #[arg(long, default_value = "json")]
        format: String,
        /// Output file (default: standard output).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Read history from a JSON export (`-` for standard input).
    Import {
        /// File to read.
        file: PathBuf,
    },
    /// Stop recording until `resume` (privacy mode).
    Pause,
    /// Start recording again.
    Resume,
    /// Delete all history.
    Clear,
    /// Show whether recording is on.
    Status,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_spec_examples() {
        for args in [
            &["xsh", "bundle", "install", "git"][..],
            &["xsh", "bundle", "remove", "git"],
            &["xsh", "bundle", "list"],
            &["xsh", "bundle", "search", "docker"],
            &["xsh", "bundle", "update", "git"],
            &["xsh", "bundle", "update", "--all"],
            &["xsh", "bundle", "info", "git"],
            &["xsh", "ai", "explain", "tar", "-xzf", "a.tgz"],
            &["xsh", "ai", "fix"],
            &["xsh", "history", "search", "cargo", "test"],
            &["xsh", "history", "export", "--format", "json"],
            &["xsh", "history", "30"],
            &["xsh", "history", "-c"],
            &["xsh", "attach", "work"],
            &["xsh", "--theme", "mono", "-c", "ls"],
            &["xsh", "config", "set", "shell.edit_mode", "vi"],
            &["xsh", "config", "list"],
            &["xsh", "-x", "script.xsh", "a", "-b"],
        ] {
            assert!(Cli::try_parse_from(args).is_ok(), "{args:?}");
        }
        assert!(Cli::try_parse_from(["xsh", "bundle", "update"]).is_err());
    }
}
