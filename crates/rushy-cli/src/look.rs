//! Colors for the line editor: syntax highlighting, completion menu, hints.
//!
//! The colors come from the active theme. With the `mono` theme or `NO_COLOR`
//! there are no colors at all, only bold and reverse video.

use nu_ansi_term::{Color, Style};
use ratatui::style::Color as Ui;
use rushy_tui::Theme;

/// Styles for every part of the prompt line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Look {
    /// Use ASCII only (no box-drawing characters).
    pub ascii: bool,
    /// A command that exists.
    pub command: Style,
    /// A command that does not exist.
    pub invalid: Style,
    /// A subcommand of the command.
    pub sub: Style,
    /// An option such as `-v` or `--verbose`.
    pub flag: Style,
    /// A quoted string.
    pub string: Style,
    /// A variable such as `$HOME` or `FOO=bar`.
    pub var: Style,
    /// `|`, `;`, `&`, `<`, `>`.
    pub op: Style,
    /// A directory in completions.
    pub dir: Style,
    /// The grey suggestion that follows the cursor.
    pub hint: Style,
    /// The line that separates subcommands from files.
    pub sep: Style,
    /// Completion menu entries.
    pub menu_text: Style,
    /// The selected menu entry.
    pub menu_selected: Style,
    /// Menu descriptions.
    pub menu_desc: Style,
    /// The typed part inside menu entries.
    pub menu_match: Style,
}

fn nu(c: Ui) -> Option<Color> {
    Some(match c {
        Ui::Reset => return None,
        Ui::Black => Color::Black,
        Ui::Red => Color::Red,
        Ui::Green => Color::Green,
        Ui::Yellow => Color::Yellow,
        Ui::Blue => Color::Blue,
        Ui::Magenta => Color::Magenta,
        Ui::Cyan => Color::Cyan,
        Ui::Gray => Color::LightGray,
        Ui::DarkGray => Color::DarkGray,
        Ui::LightRed => Color::LightRed,
        Ui::LightGreen => Color::LightGreen,
        Ui::LightYellow => Color::LightYellow,
        Ui::LightBlue => Color::LightBlue,
        Ui::LightMagenta => Color::LightMagenta,
        Ui::LightCyan => Color::LightCyan,
        Ui::White => Color::White,
        Ui::Rgb(r, g, b) => Color::Rgb(r, g, b),
        Ui::Indexed(i) => Color::Fixed(i),
    })
}

fn fg(c: Ui) -> Style {
    nu(c).map_or_else(Style::new, |c| Style::new().fg(c))
}

impl Look {
    /// No colors. Commands and subcommands are still bold, which needs no
    /// color support.
    #[must_use]
    pub fn plain(ascii: bool) -> Self {
        let s = Style::new();
        Self {
            ascii,
            command: s.bold(),
            invalid: s,
            sub: s.bold(),
            flag: s,
            string: s,
            var: s,
            op: s,
            dir: s.bold(),
            hint: s.dimmed(),
            sep: s.dimmed(),
            menu_text: s,
            menu_selected: s.reverse(),
            menu_desc: s.dimmed(),
            menu_match: s.underline(),
        }
    }

    /// Styles derived from `theme`.
    #[must_use]
    pub fn from_theme(theme: &Theme, ascii: bool) -> Self {
        if theme.plain {
            return Self::plain(ascii);
        }
        let dim = fg(theme.foreground).dimmed();
        Self {
            ascii,
            command: fg(theme.success).bold(),
            invalid: fg(theme.error),
            sub: fg(theme.accent).bold(),
            flag: fg(theme.prompt_path),
            string: fg(theme.success),
            var: fg(theme.prompt_path),
            op: dim,
            dir: fg(theme.accent),
            hint: dim,
            sep: dim,
            menu_text: fg(theme.foreground),
            menu_selected: Style::new().reverse().bold(),
            menu_desc: dim,
            menu_match: fg(theme.success).underline(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_map_to_colors_and_mono_has_none() {
        let tokyo = Theme::builtin("tokyo-night").map(|t| Look::from_theme(&t, false));
        let tokyo = tokyo.unwrap_or_else(|_| Look::plain(false));
        assert_eq!(tokyo.command, Style::new().fg(Color::Rgb(0x9e, 0xce, 0x6a)).bold());
        assert_eq!(tokyo.invalid, Style::new().fg(Color::Rgb(0xf7, 0x76, 0x8e)));
        assert_eq!(tokyo.sub, Style::new().fg(Color::Rgb(0x7a, 0xa2, 0xf7)).bold());
        assert_ne!(tokyo.command, tokyo.sub, "command and subcommand must differ");

        let mono = Theme::mono().map(|t| Look::from_theme(&t, true));
        let mono = mono.unwrap_or_else(|_| Look::plain(true));
        assert_eq!(mono, Look::plain(true));
        assert!(mono.command.is_bold && mono.command.foreground.is_none());
        assert!(mono.invalid.foreground.is_none());
    }
}
