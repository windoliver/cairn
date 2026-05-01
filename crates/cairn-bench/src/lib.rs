#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! `BrainBench` retrieval scorecard runner library.
//!
//! Hosts the fixture loader and IR metric helpers shared by the
//! `cairn-bench` binary. Adapter dispatch and report writing land in
//! follow-up tasks; this crate is intentionally minimal at scaffold time.

pub mod fixture;
pub mod metrics;
