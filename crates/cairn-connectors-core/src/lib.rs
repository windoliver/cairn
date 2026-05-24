//! Cairn connector framework — substrate that lets adapter crates
//! emit `CaptureEvent`s through the cairn-core pipeline behind a
//! validated, redacted, consent-gated boundary.
//!
//! Issue #130, brief §9.1 source sensors, §19 v0.3.
//!
//! # Status
//! Scaffold only. Modules and re-exports are added task-by-task (T5–T16).
//! `CONTRACT_VERSION` is a placeholder; T8 moves it into `connector.rs`
//! and replaces this with `pub use connector::CONTRACT_VERSION`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cairn_core::contract::version::ContractVersion;

/// Contract version for the `Connector` trait surface.
/// Placeholder for this scaffold; replaced by `pub use connector::CONTRACT_VERSION`
/// when T8 lands `cairn-connectors-core/src/connector.rs`.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);
