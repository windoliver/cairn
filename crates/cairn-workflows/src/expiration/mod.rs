//! Minimum-path `ExpirationWorkflow` (issue #91, brief §10.0, §6).
//!
//! See [`handler::ExpirationHandler`] for the entry point.

pub mod handler;
pub mod payload;

pub use handler::{EXPIRATION_KIND, ExpirationHandler, ExpirationSweepReport};
pub use payload::ExpirationPayload;
