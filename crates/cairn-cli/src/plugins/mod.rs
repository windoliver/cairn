//! `cairn plugins` subcommand implementation.
//!
//! - [`host`] — wires bundled adapter crates into a `PluginRegistry`.
//! - [`list`] — `cairn plugins list` (Task 11).
//! - [`verify`] — `cairn plugins verify` (Task 12).

pub mod host;
pub mod list;
pub mod verify;
