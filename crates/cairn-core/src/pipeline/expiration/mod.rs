//! Pure pipeline functions for the `ExpirationWorkflow` (issue #91,
//! brief §10.0).
//!
//! Soft-retirement decisions belong in `cairn-core` because brief §4
//! requires "seven contracts, pure functions otherwise" and CLAUDE.md
//! §4 forbids hidden I/O in core. The handler in
//! `cairn-workflows::expiration` calls [`decide`] per record and
//! lifts the verdict into a `MemoryStore::tombstone` call.

pub mod decision;

pub use decision::decide;
