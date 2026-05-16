#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Cairn bench harness: `BrainBench` scorecard + release gates (latency, memory, privacy).

pub mod adapter;
pub mod cache;
pub mod cached_embedder;
pub mod fixture;
pub mod gates;
pub mod latency;
pub mod memory;
pub mod metrics;
pub mod report;
pub mod scorecard;
