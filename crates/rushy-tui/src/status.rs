//! The bottom status bar.

use ratatui::text::{Line, Span};

use crate::prompt::truncate;
use crate::theme::Theme;

/// State of the AI layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiState {
    /// No provider is configured.
    Off,
    /// A provider is configured.
    Ready(String),
    /// A request is running.
    Busy(String),
    /// The last request failed.
    Failed(String),
}

impl AiState {
    fn text(&self) -> String {
        match self {
            Self::Off => "ai: off".into(),
            Self::Ready(p) => format!("ai: {p}"),
            Self::Busy(p) => format!("ai: {p} ..."),
            Self::Failed(p) => format!("ai: {p} error"),
        }
    }
}

/// Everything the status bar shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInfo {
    /// Session name.
    pub session: String,
    /// 1-based active window.
    pub window: usize,
    /// Window count.
    pub windows: usize,
    /// Focused pane label, such as `%3`.
    pub pane: String,
    /// Number of active bundles.
    pub bundles: usize,
    /// AI state.
    pub ai: AiState,
    /// Clock text such as `14:05`.
    pub clock: String,
}

/// The local time as `HH:MM`.
#[must_use]
pub fn clock_now() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// Renders the bar into one line of exactly `width` columns.
///
/// The clock is always kept; the left side is truncated first.
#[must_use]
pub fn render_status(theme: &Theme, info: &StatusInfo, width: u16, ascii: bool) -> Line<'static> {
    let width = usize::from(width);
    let sep = if ascii { " | " } else { " │ " };
    let left = format!(
        "[{}] win {}/{} pane {}{sep}bundles {}{sep}{}",
        info.session,
        info.window,
        info.windows,
        info.pane,
        info.bundles,
        info.ai.text()
    );
    let right = format!(" {} ", info.clock);
    let room = width.saturating_sub(right.chars().count());
    let mut left = truncate(&left, room, ascii);
    let pad = room.saturating_sub(left.chars().count());
    left.push_str(&" ".repeat(pad));
    let style = if theme.plain {
        theme.focus()
    } else {
        theme.style(theme.background).bg(theme.accent)
    };
    Line::from(vec![Span::styled(left, style), Span::styled(right, style)])
}
