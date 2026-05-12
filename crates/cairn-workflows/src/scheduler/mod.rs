//! Tokio scheduler loop over [`cairn_core::contract::JobStore`].
//! Built incrementally across Tasks 5–9 of the #90 plan.

pub mod clock;

pub use clock::{Clock, MockClock, SystemClock};
