//! Bitemporal knowledge-graph storage layer (issue #186).
//!
//! All entity-graph mutations write through the generic `wal_ops` +
//! `wal_steps` schema (migration 0002, widened in 0031) using the helpers
//! in [`wal`]. Spec: `docs/superpowers/specs/2026-05-02-issue-186-bitemporal-kg-schema-design.md`.

pub mod wal;

mod edge;
mod episode;
mod node;
mod query;
