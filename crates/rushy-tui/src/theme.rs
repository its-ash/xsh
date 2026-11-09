//! Themes: TOML files under `<config>/themes/`, plus built-ins.
//!
//! ```toml
//! name = "tokyo-night"
//! [colors]
//! background = "#1a1b26"
//! foreground = "#c0caf5"
//! accent = "#7aa2f7"
//! error = "#f7768e"
//! success = "#9ece6a"
//! prompt_user = "#7aa2f7"
//! prompt_path = "#bb9af7"
//! [prompt]
//! format = "{user} {path} {git}{node}{python}❯ "
//! ```

use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use crate::{Error, Result};

const TOKYO_NIGHT: &str = include_str!("../themes/tokyo-night.toml");
const MONO: &str = include_str!("../themes/mono.toml");
const DRACULA: &str = include_str!("../themes/dracula.toml");
const NORD: &str = include_str!("../themes/nord.toml");
const SOLARIZED_DARK: &str = include_str!("../themes/solarized-dark.toml");
const SOLARIZED_LIGHT: &str = include_str!("../themes/solarized-light.toml");

/// Built-in theme names.
pub const BUILTIN: &[&str] = &[
    "tokyo-night",
    "mono",
    "dracula",
    "nord",
    "solarized-dark",
    "solarized-light",
];

/// Parses `#rrggbb` or a basic color name.
///
/// ```
/// use ratatui::style::Color;
/// assert_eq!(rushy_tui::parse_color("#7aa2f7").ok(), Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
/// assert_eq!(rushy_tui::parse_color("cyan").ok(), Some(Color::Cyan));
/// assert!(rushy_tui::parse_color("#12").is_err());
/// ```
///
/// # Errors
///
/// Returns [`Error::Color`] for anything else.
pub fn parse_color(s: &str) -> Result<Color> {
    let bad = || Error::Color(s.to_owned());
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() != 6 || !hex.is_ascii() {
            return Err(bad());
        }
        let n = |i: usize| {
            u8::from_str_radix(hex.get(i..i + 2).unwrap_or_default(), 16).map_err(|_| bad())
        };
        return Ok(Color::Rgb(n(0)?, n(2)?, n(4)?));
    }
    Ok(match s.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "gray" | "grey" => Color::Gray,
        "reset" | "" => Color::Reset,
        _ => return Err(bad()),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawColors {
    background: String,
    foreground: String,
    accent: String,
    error: String,
    success: String,
    prompt_user: String,
    prompt_path: String,
    #[serde(default)]
    cursor: String,
    #[serde(default)]
    selection: String,
    #[serde(default)]
    ansi: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPrompt {
    format: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    name: String,
    colors: RawColors,
    prompt: RawPrompt,
}

/// A resolved theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Theme name.
    pub name: String,
    /// Window background.
    pub background: Color,
    /// Default text.
    pub foreground: Color,
    /// Focus and highlights.
    pub accent: Color,
    /// Errors.
    pub error: Color,
    /// Success.
    pub success: Color,
    /// User name in the prompt.
    pub prompt_user: Color,
    /// Path in the prompt.
    pub prompt_path: Color,
    /// Prompt template: `{user}`, `{host}`, `{path}`, `{status}`, and bundle segments.
    pub prompt_format: String,
    /// Whether colors are disabled (`mono`, `NO_COLOR`).
    pub plain: bool,
    /// Cursor color in the macOS app; `Reset` uses the foreground.
    pub cursor: Color,
    /// Selection color in the macOS app; `Reset` uses the system color.
    pub selection: Color,
    /// The 16 ANSI colors for the macOS app; empty uses the default palette.
    pub ansi: Vec<Color>,
}

impl Theme {
    /// Parses theme text.
    ///
    /// # Errors
    ///
    /// Fails with a located error for bad TOML, or [`Error::Color`] for a bad
    /// color or an `ansi` list that does not have 16 colors.
    pub fn parse(path: &Path, text: &str) -> Result<Self> {
        let raw: Raw = rushy_config::parse_toml(path, text)?;
        let c = &raw.colors;
        if !c.ansi.is_empty() && c.ansi.len() != 16 {
            return Err(Error::Color(format!(
                "ansi has {} colors, it needs 16",
                c.ansi.len()
            )));
        }
        Ok(Self {
            name: raw.name,
            background: parse_color(&c.background)?,
            foreground: parse_color(&c.foreground)?,
            accent: parse_color(&c.accent)?,
            error: parse_color(&c.error)?,
            success: parse_color(&c.success)?,
            prompt_user: parse_color(&c.prompt_user)?,
            prompt_path: parse_color(&c.prompt_path)?,
            prompt_format: raw.prompt.format,
            plain: false,
            cursor: parse_color(&c.cursor)?,
            selection: parse_color(&c.selection)?,
            ansi: c
                .ansi
                .iter()
                .map(|s| parse_color(s))
                .collect::<Result<_>>()?,
        })
    }

    /// A built-in theme.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoTheme`] for unknown names.
    pub fn builtin(name: &str) -> Result<Self> {
        let (text, plain) = match name {
            "tokyo-night" => (TOKYO_NIGHT, false),
            "mono" => (MONO, true),
            "dracula" => (DRACULA, false),
            "nord" => (NORD, false),
            "solarized-dark" => (SOLARIZED_DARK, false),
            "solarized-light" => (SOLARIZED_LIGHT, false),
            _ => return Err(Error::NoTheme(name.to_owned())),
        };
        let mut t = Self::parse(Path::new(name), text)?;
        t.plain = plain;
        Ok(t)
    }

    /// Looks `name` up as `<dir>/<name>.toml`, then among the built-ins.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoTheme`] when neither exists, or a parse error.
    pub fn load(dir: &Path, name: &str) -> Result<Self> {
        let safe = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if safe {
            let path = dir.join(format!("{name}.toml"));
            if path.is_file() {
                return Self::parse(&path, &std::fs::read_to_string(&path)?);
            }
        }
        Self::builtin(name)
    }

    /// The `mono` theme, used for `NO_COLOR`.
    ///
    /// # Errors
    ///
    /// Never fails in practice; the theme is compiled in.
    pub fn mono() -> Result<Self> {
        Self::builtin("mono")
    }

    /// A style with foreground `fg` on the theme background, or the terminal
    /// default when plain.
    #[must_use]
    pub fn style(&self, fg: Color) -> Style {
        if self.plain {
            Style::default()
        } else {
            Style::default().fg(fg).bg(self.background)
        }
    }

    /// Text style.
    #[must_use]
    pub fn text(&self) -> Style {
        self.style(self.foreground)
    }

    /// Focus style: accent color, or reverse video when plain.
    #[must_use]
    pub fn focus(&self) -> Style {
        if self.plain {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            self.style(self.accent).add_modifier(Modifier::BOLD)
        }
    }
}
