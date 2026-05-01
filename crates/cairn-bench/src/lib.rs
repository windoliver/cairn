#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! `BrainBench` retrieval scorecard runner library.
//!
//! Hosts the fixture loader, IR metric helpers, the adapter trait + four
//! cairn-side implementations (`bm25-only`, `vector-bge`, `hybrid-bge-rrf`,
//! `hybrid-openai-rrf`), and the disk-backed embedding cache that lets
//! re-runs skip inference and HTTP cost. The report writer (Task 12)
//! lives behind a stub today; the binary in `src/main.rs` calls every
//! module here.

pub mod adapter;
pub mod cache;
pub mod fixture;
pub mod metrics;
pub mod report;
