#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Cairn bench harness: `BrainBench` scorecard + release gates (latency, memory, privacy, SRE).

pub mod adapter;
pub mod all;
pub mod cache;
pub mod cached_embedder;
pub mod fixture;
pub mod gates;
pub mod latency;
pub mod memory;
pub mod metrics;
pub mod privacy;
pub mod report;
pub mod scorecard;
pub mod sre;
