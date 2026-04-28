//! Folder sidecars — `_index.md`, `_policy.yaml`, `_summary.md` (brief §3.4).
//!
//! Pure functions only — zero I/O, zero async. Caller (CLI / future hooks)
//! supplies records and policy bytes; module returns projected files,
//! parsed policies, and resolved effective policies.

pub mod index;
pub mod links;
pub mod policy;
