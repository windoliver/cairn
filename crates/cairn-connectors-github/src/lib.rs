//! GitHub connector adapter (issues, PRs, commits) for `cairn-connectors-core`.
//!
//! Issue #131, brief §19 v0.3 connector set, §9.1 source sensors.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Allowed during scaffold; removed by Task 17.
#![allow(dead_code)]

mod auth;
mod client;
mod connector;
mod cursor;
mod error;
mod resources;
mod webhook;

// Re-exports added in Tasks 2/12.
// pub use connector::GitHubConnector;
// pub use error::GhError;

/// Embedded `connector.toml` bytes, parsed at `GitHubConnector::new` time.
pub(crate) const MANIFEST_TOML: &str = include_str!("../connector.toml");
