//! GitHub connector adapter (issues, PRs, commits) for `cairn-connectors-core`.
//!
//! Issue #131, brief §19 v0.3 connector set, §9.1 source sensors.
//!
//! ## Live smoke tests
//!
//! The `tests/smoke_live_github.rs` file contains `#[ignore]`-gated tests that
//! hit real `api.github.com`. Run them with a PAT:
//!
//! ```bash
//! GITHUB_TOKEN=ghp_… \
//!   cargo nextest run -p cairn-connectors-github \
//!   --run-ignored only smoke_live_github --locked
//! ```
//!
//! These tests verify that our DTOs and HTTP headers work against the real
//! GitHub wire shape, not just our fixture JSON.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod auth;
mod client;
mod connector;
mod cursor;
mod error;
mod event_id;
mod resources;
mod webhook;

pub use error::GhError;

pub use connector::GitHubConnector;

/// Embedded `connector.toml` bytes, parsed at `GitHubConnector::new` time.
///
/// Exposed as a `pub` constant so integration tests can derive the expected
/// `manifest_hash` when constructing a [`ConsentGrant`] for the registry
/// end-to-end test (issue #131).
pub const MANIFEST_TOML: &str = include_str!("../connector.toml");

/// Test-only helpers exposed for integration tests. Cfg-gated; not part of
/// the public API.
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
