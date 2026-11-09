//! PTY, pane, and session management for rushy.
//!
//! # Design
//!
//! rushy uses [`portable_pty`] plus a custom pane manager instead of an
//! external multiplexer SDK:
//!
//! * `portable-pty` is the PTY layer of WezTerm. It is maintained, and its safe
//!   API lets this crate keep `#![forbid(unsafe_code)]`.
//! * Owning the layout tree ([`Layout`]) and the session model
//!   ([`SessionManager`]) keeps sessions, the protocol, and the TUI in one
//!   codebase, with no second binary to install or version-match.
//!
//! Each [`Pane`] owns a PTY master, a reader thread that feeds a `vt100`
//! emulator and the event channel, a writer thread that drains input, and a
//! waiter thread that reports exit. The threads keep blocking PTY I/O off the
//! async runtime.
//!
//! Detach and re-attach need the PTYs to outlive the client, so
//! [`server::serve`] runs them in a background process that listens on a local
//! socket (`<data>/mux/default.sock`, mode `0700` directory). Clients exchange
//! [`proto`] frames: length-prefixed JSON. One client is attached at a time; a
//! new attach detaches the old client.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::pedantic)]

mod error;
mod layout;
mod pane;
pub mod proto;
pub mod server;
mod session;

pub use error::{Error, Result};
pub use layout::{Axis, Dir, Layout, Rect, neighbor};
pub use pane::{Pane, PaneCommand, PaneEvent, PaneId};
pub use session::{MuxCommand, PaneView, SessionId, SessionInfo, SessionManager, View, WindowInfo};
