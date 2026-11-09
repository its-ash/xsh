//! ratatui rendering, themes, and widgets for rushy.
//!
//! * [`Theme`]: TOML themes with `NO_COLOR`/`mono` support.
//! * [`render_prompt`]: the prompt template plus bundle segments, with
//!   priorities and truncation.
//! * [`Palette`]: the `Ctrl-K` fuzzy command palette.
//! * [`render_status`]: the status bar.
//! * [`mux::draw`]: panes, borders, and focus for a multiplexer session.
//! * [`ConfigWatcher`]: hot-reload of themes and bundles.
//!
//! Accessibility: pass `ascii: true` for plain ASCII borders and no decorative
//! glyphs; use the `mono` theme (or set `NO_COLOR`) to drop all colors. No
//! widget animates.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::pedantic)]

mod error;
pub mod mux;
pub mod palette;
mod prompt;
mod status;
mod theme;
mod watch;

pub use error::{Error, Result};
pub use palette::Palette;
pub use prompt::{
    Piece, PromptInput, RenderOpts, Values, plain, render_prompt, segment_values, to_ansi, to_line,
    truncate,
};
pub use status::{AiState, StatusInfo, clock_now, render_status};
pub use theme::{BUILTIN, Theme, parse_color};
pub use watch::{ConfigWatcher, is_config_path};
