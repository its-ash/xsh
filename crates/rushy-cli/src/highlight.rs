//! Syntax colors for the command line, taken from the active theme.

use nu_ansi_term::{Color, Style};
use ratatui::style::Color as TuiColor;

use rushy_tui::Theme;

/// Styles for each kind of token.
#[derive(Debug, Clone, Default)]
pub struct Colors {
    pub command: Style,
    pub builtin: Style,
    pub unknown: Style,
    pub flag: Style,
    pub string: Style,
    pub var: Style,
    pub operator: Style,
    pub keyword: Style,
    pub comment: Style,
    pub path: Style,
}

fn conv(c: TuiColor) -> Color {
    match c {
        TuiColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
        TuiColor::Black => Color::Black,
        TuiColor::Red | TuiColor::LightRed => Color::Red,
        TuiColor::Green | TuiColor::LightGreen => Color::Green,
        TuiColor::Yellow | TuiColor::LightYellow => Color::Yellow,
        TuiColor::Blue | TuiColor::LightBlue => Color::Blue,
        TuiColor::Magenta | TuiColor::LightMagenta => Color::Purple,
        TuiColor::Cyan | TuiColor::LightCyan => Color::Cyan,
        TuiColor::Gray | TuiColor::DarkGray => Color::DarkGray,
        _ => Color::Default,
    }
}

impl Colors {
    /// Colors from `theme`; all plain when `plain` is set.
    pub fn new(theme: &Theme, plain: bool) -> Self {
        if plain || theme.plain {
            return Self {
                command: Style::new().bold(),
                builtin: Style::new().bold(),
                ..Self::default()
            };
        }
        let fg = |c| Style::new().fg(conv(c));
        Self {
            command: fg(theme.success).bold(),
            builtin: fg(theme.accent).bold(),
            unknown: fg(theme.error).bold(),
            flag: fg(theme.prompt_user),
            string: Style::new().fg(Color::Yellow),
            var: fg(theme.prompt_path),
            operator: fg(theme.accent),
            keyword: fg(theme.prompt_path).bold(),
            comment: Style::new().fg(Color::DarkGray).italic(),
            path: Style::new().underline(),
        }
    }
}

/// Reserved words that start or end a compound command.
pub const KEYWORDS: &[&str] = &[
    "if", "then", "elif", "else", "fi", "for", "in", "do", "done", "while", "until", "case",
    "esac", "function", "!", "{", "}", "[[", "]]", "select", "time",
];

/// Host names from `~/.ssh/config` (`Host` lines without wildcards) and
/// `~/.ssh/known_hosts` (plain, unhashed entries).
pub fn ssh_hosts(home: Option<&str>) -> Vec<String> {
    let Some(home) = home else {
        return Vec::new();
    };
    let dir = std::path::Path::new(home).join(".ssh");
    let mut out = std::collections::BTreeSet::new();
    if let Ok(text) = std::fs::read_to_string(dir.join("config")) {
        for line in text.lines() {
            let mut it = line.split_whitespace();
            if it.next().is_some_and(|k| k.eq_ignore_ascii_case("host")) {
                out.extend(
                    it.filter(|h| !h.contains(['*', '?', '!']))
                        .map(str::to_owned),
                );
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("known_hosts")) {
        for line in text.lines().filter(|l| !l.starts_with(['|', '#', '@'])) {
            if let Some(field) = line.split_whitespace().next() {
                out.extend(
                    field
                        .split(',')
                        .map(|h| h.trim_start_matches('[').split(']').next().unwrap_or(h))
                        .filter(|h| !h.is_empty())
                        .map(str::to_owned),
                );
            }
        }
    }
    out.into_iter().collect()
}

/// Whether `word` is a reserved word.
pub fn is_keyword(word: &str) -> bool {
    KEYWORDS.contains(&word)
}
