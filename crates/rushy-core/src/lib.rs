//! Shell state, parser, executor, and jobs for rushy.
//!
//! ```no_run
//! # async fn demo() -> rushy_core::Result<()> {
//! let mut shell = rushy_core::Shell::from_env()?;
//! shell.run_line("echo hello | wc -c").await?;
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::pedantic)]

mod arith;
pub mod ast;
pub mod builtins;
mod error;
mod exec;
mod expand;
mod glob;
mod jobs;
pub mod parser;
mod regex;
mod shell;
mod state;
mod test;

pub use builtins::{BUILTINS, KEYWORDS, is_builtin};
pub use error::{Error, Result};
pub use jobs::{JobInfo, JobState};
pub use shell::{Flow, LastRun, Shell};
pub use state::{ForegroundHandle, Options, SET_OPTIONS, SHOPT_OPTIONS, ShellState};
