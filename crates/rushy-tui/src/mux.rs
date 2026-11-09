//! Drawing multiplexer sessions: pane borders, focus, screen contents.

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect as UiRect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::widgets::{Block, Borders};
use rushy_pty::{PaneId, PaneView, Rect, View};

use crate::status::{StatusInfo, render_status};
use crate::theme::Theme;

const ASCII_BORDER: border::Set = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// How to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Plain ASCII borders, no decorative glyphs (screen-reader mode).
    pub ascii: bool,
    /// Reserve and draw the status bar.
    pub status_bar: bool,
}

/// The pane area `(cols, rows)` for a terminal of `term`, leaving a row for the status bar.
#[must_use]
pub fn pane_area(term: (u16, u16), status_bar: bool) -> (u16, u16) {
    (term.0, term.1.saturating_sub(u16::from(status_bar)).max(1))
}

/// Client-side copies of every visible pane's screen.
#[derive(Default)]
pub struct Screens {
    parsers: HashMap<PaneId, vt100::Parser>,
}

impl std::fmt::Debug for Screens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screens")
            .field("panes", &self.parsers.len())
            .finish()
    }
}

impl Screens {
    /// Empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the set match `view`: new panes get a screen sized to their
    /// border-inner area, existing ones resize, and missing ones are dropped.
    pub fn sync(&mut self, view: &View) {
        self.parsers
            .retain(|id, _| view.panes.iter().any(|p| p.id == *id));
        for p in &view.panes {
            let inner = p.rect.inner();
            let (rows, cols) = (inner.height, inner.width);
            self.parsers
                .entry(p.id)
                .and_modify(|s| {
                    if s.screen().size() != (rows, cols) {
                        s.set_size(rows, cols);
                    }
                })
                .or_insert_with(|| vt100::Parser::new(rows, cols, 0));
        }
    }

    /// Replaces a pane's screen with a server snapshot (escape sequences).
    pub fn snapshot(&mut self, id: PaneId, bytes: &[u8]) {
        if let Some(s) = self.parsers.get_mut(&id) {
            let (rows, cols) = s.screen().size();
            *s = vt100::Parser::new(rows, cols, 0);
            s.process(bytes);
        }
    }

    /// Feeds incremental output.
    pub fn output(&mut self, id: PaneId, bytes: &[u8]) {
        if let Some(s) = self.parsers.get_mut(&id) {
            s.process(bytes);
        }
    }

    /// Plain text of a pane's screen.
    #[must_use]
    pub fn contents(&self, id: PaneId) -> Option<String> {
        self.parsers.get(&id).map(|s| s.screen().contents())
    }
}

fn convert(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn ui_rect(r: Rect) -> UiRect {
    UiRect::new(r.x, r.y, r.width, r.height)
}

fn blit(buf: &mut Buffer, area: UiRect, screen: &vt100::Screen, plain: bool) {
    for row in 0..area.height {
        for col in 0..area.width {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let Some(dst) = buf.cell_mut((area.x + col, area.y + row)) else {
                continue;
            };
            let text = cell.contents();
            dst.set_symbol(if text.is_empty() { " " } else { &text });
            let mut style = Style::default();
            if !plain {
                style = style
                    .fg(convert(cell.fgcolor()))
                    .bg(convert(cell.bgcolor()));
            }
            for (on, m) in [
                (cell.bold(), Modifier::BOLD),
                (cell.italic(), Modifier::ITALIC),
                (cell.underline(), Modifier::UNDERLINED),
                (cell.inverse(), Modifier::REVERSED),
            ] {
                if on {
                    style = style.add_modifier(m);
                }
            }
            dst.set_style(style);
        }
    }
}

fn draw_pane(f: &mut Frame<'_>, p: &PaneView, screens: &Screens, theme: &Theme, opts: Options) {
    let area = ui_rect(p.rect);
    let title = if p.focused {
        format!(" {} {} ", p.id, if opts.ascii { "[active]" } else { "●" })
    } else {
        format!(" {} ", p.id)
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(theme.text())
        .border_style(if p.focused {
            theme.focus()
        } else {
            theme.style(theme.foreground)
        });
    if opts.ascii {
        block = block.border_set(ASCII_BORDER);
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if let Some(parser) = screens.parsers.get(&p.id) {
        blit(f.buffer_mut(), inner, parser.screen(), theme.plain);
        if p.focused && !parser.screen().hide_cursor() {
            let (row, col) = parser.screen().cursor_position();
            if col < inner.width && row < inner.height {
                f.set_cursor_position((inner.x + col, inner.y + row));
            }
        }
    }
}

/// Draws all panes of `view`, then the status bar on the last row.
pub fn draw(
    f: &mut Frame<'_>,
    view: &View,
    screens: &Screens,
    theme: &Theme,
    status: Option<&StatusInfo>,
    opts: Options,
) {
    let area = f.area();
    f.render_widget(Block::default().style(theme.text()), area);
    for p in &view.panes {
        draw_pane(f, p, screens, theme, opts);
    }
    if let (true, Some(info)) = (opts.status_bar, status) {
        let bar = UiRect::new(
            area.x,
            area.y + area.height.saturating_sub(1),
            area.width,
            1,
        );
        f.render_widget(
            ratatui::widgets::Paragraph::new(render_status(theme, info, area.width, opts.ascii)),
            bar,
        );
    }
}
