//! `Ctrl-K` command palette: a fuzzy-searchable list of everything runnable.

use std::collections::BTreeMap;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use rushy_history::fuzzy;

use crate::theme::Theme;

/// What an item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A shell built-in.
    Builtin,
    /// An installed bundle.
    Bundle,
    /// An alias.
    Alias,
    /// A theme.
    Theme,
    /// An AI action.
    Ai,
    /// A recent command.
    History,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Bundle => "bundle",
            Self::Alias => "alias",
            Self::Theme => "theme",
            Self::Ai => "ai",
            Self::History => "history",
        }
    }
}

/// One palette entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Category.
    pub kind: Kind,
    /// Text shown and searched.
    pub label: String,
    /// Secondary text, also searched.
    pub detail: String,
    /// Text to run or insert on selection.
    pub insert: String,
}

/// AI actions always offered by the palette.
pub const AI_ACTIONS: &[(&str, &str, &str)] = &[
    ("ai: command from description", "Ctrl-Space", "ai suggest "),
    ("ai: agent for a goal", "Ctrl-Shift-Space", "ai agent "),
    ("ai: agent on autopilot", "Alt-A", "ai agent --auto "),
    ("ai: explain a command", "xsh ai explain", "ai explain "),
    ("ai: fix the last failed command", "xsh ai fix", "ai fix"),
];

/// Builds the item list from the shell's current state.
#[must_use]
pub fn items(
    builtins: &[(&str, &str)],
    bundles: &[String],
    aliases: &BTreeMap<String, String>,
    themes: &[String],
    history: &[String],
) -> Vec<Item> {
    let mut out = Vec::new();
    out.extend(builtins.iter().map(|(n, d)| Item {
        kind: Kind::Builtin,
        label: (*n).to_owned(),
        detail: (*d).to_owned(),
        insert: (*n).to_owned(),
    }));
    out.extend(bundles.iter().map(|b| Item {
        kind: Kind::Bundle,
        label: b.clone(),
        detail: "bundle info".to_owned(),
        insert: format!("bundle info {b}"),
    }));
    out.extend(aliases.iter().map(|(k, v)| Item {
        kind: Kind::Alias,
        label: k.clone(),
        detail: v.clone(),
        insert: k.clone(),
    }));
    out.extend(themes.iter().map(|t| Item {
        kind: Kind::Theme,
        label: t.clone(),
        detail: "switch theme".to_owned(),
        insert: format!("theme {t}"),
    }));
    out.extend(AI_ACTIONS.iter().map(|(l, d, i)| Item {
        kind: Kind::Ai,
        label: (*l).to_owned(),
        detail: (*d).to_owned(),
        insert: (*i).to_owned(),
    }));
    out.extend(history.iter().rev().map(|h| Item {
        kind: Kind::History,
        label: h.clone(),
        detail: String::new(),
        insert: h.clone(),
    }));
    out
}

/// What a key press did to the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing to report.
    None,
    /// Close without choosing (`Esc`, `Ctrl-C`, `Ctrl-K`).
    Close,
    /// An item was chosen (`Enter`).
    Choose(Item),
}

/// Palette state.
#[derive(Debug, Clone)]
pub struct Palette {
    items: Vec<Item>,
    haystack: Vec<String>,
    query: String,
    matches: Vec<usize>,
    selected: usize,
    title: String,
}

impl Palette {
    /// Sets the title shown on the frame.
    #[must_use]
    pub fn with_title(mut self, title: &str) -> Self {
        self.title = format!(" {title} ");
        self
    }

    /// A palette over `items`.
    #[must_use]
    pub fn new(items: Vec<Item>) -> Self {
        let haystack = items
            .iter()
            .map(|i| format!("{} {}", i.label, i.detail))
            .collect();
        let mut p = Self {
            items,
            haystack,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            title: " Command palette ".to_owned(),
        };
        p.refresh();
        p
    }

    fn refresh(&mut self) {
        self.matches = fuzzy::rank(&self.query, &self.haystack);
        self.selected = 0;
    }

    /// Current query.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Matching items, best first.
    #[must_use]
    pub fn visible(&self) -> Vec<&Item> {
        self.matches
            .iter()
            .filter_map(|&i| self.items.get(i))
            .collect()
    }

    /// The highlighted item.
    #[must_use]
    pub fn current(&self) -> Option<&Item> {
        self.matches
            .get(self.selected)
            .and_then(|&i| self.items.get(i))
    }

    /// Handles one key press.
    pub fn on_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => Action::Close,
            KeyCode::Char('c' | 'k') if ctrl => Action::Close,
            KeyCode::Enter => self
                .current()
                .cloned()
                .map_or(Action::Close, Action::Choose),
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                Action::None
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.matches.len().saturating_sub(1));
                Action::None
            }
            KeyCode::Char('p') if ctrl => self.on_key(KeyEvent::from(KeyCode::Up)),
            KeyCode::Char('n') if ctrl => self.on_key(KeyEvent::from(KeyCode::Down)),
            KeyCode::Backspace => {
                self.query.pop();
                self.refresh();
                Action::None
            }
            KeyCode::Char(c) if !ctrl => {
                self.query.push(c);
                self.refresh();
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Draws the palette as a centered overlay inside `area`.
    pub fn render(&self, f: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let w = area.width.saturating_sub(4).clamp(20, 72).min(area.width);
        let h = area.height.saturating_sub(2).clamp(5, 16).min(area.height);
        let rect = Rect::new(
            area.x + area.width.saturating_sub(w) / 2,
            area.y + area.height.saturating_sub(h) / 3,
            w,
            h,
        );
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(self.title.as_str())
            .border_style(theme.focus())
            .style(theme.text());
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        if inner.height == 0 {
            return;
        }
        let prompt = Rect::new(inner.x, inner.y, inner.width, 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", theme.focus()),
                Span::styled(self.query.clone(), theme.text()),
            ])),
            prompt,
        );
        let list_area = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(1),
        );
        let rows: Vec<ListItem<'_>> = self
            .visible()
            .into_iter()
            .map(|i| {
                let mut spans = vec![Span::styled(
                    format!("{:<8}", i.kind.tag()),
                    theme.style(theme.accent),
                )];
                spans.push(Span::styled(i.label.clone(), theme.text()));
                if !i.detail.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", i.detail),
                        theme.style(theme.foreground),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let mut state = ListState::default().with_selected(Some(self.selected));
        f.render_stateful_widget(
            List::new(rows)
                .highlight_style(theme.focus())
                .highlight_symbol("> "),
            list_area,
            &mut state,
        );
    }
}
