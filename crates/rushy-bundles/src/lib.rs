//! Bundle (plugin) system and registry client for rushy.
//!
//! # Security model
//!
//! rushy is not bash, and bundles cannot run arbitrary shell code. The
//! declarative `bundle.toml` is the whole interface:
//!
//! * `[env]` and `[aliases]` set variables and aliases while the bundle is active;
//! * `[prompt]` contributes one prompt segment with fixed placeholders;
//! * `[completion]` lists commands and first-argument candidates;
//! * `[[hooks]]` name commands to run on activation, and every hook passes the
//!   AI safety guard first (and `bundles.allow_hooks = false` turns them off).
//!
//! An optional `init.rushy` may hold only literal `alias` and `export` lines;
//! it is parsed, never executed. Downloads must be HTTPS from an allowlisted
//! host and match the SHA-256 in the registry index.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(clippy::pedantic)]

mod activate;
mod error;
mod init;
mod manifest;
mod registry;
mod store;

pub use activate::{Effects, detect, is_active, on_path};
pub use error::{Error, Result};
pub use init::{InitFile, parse_init};
pub use manifest::{
    Completion, Hook, HookEvent, Manifest, Package, RUSHY_VERSION, Rules, Segment, valid_name,
};
pub use registry::{
    Download, EXTRA_FILES, Index, IndexSource, RegistryClient, RegistryEntry, RegistryFile,
    sha256_hex,
};
pub use store::{BundleStore, Installed};
