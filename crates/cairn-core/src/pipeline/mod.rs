//! Pure pipeline functions for the write path (brief §5.2).
//!
//! Capture → Tool-squash → Extract → **Filter** → Classify → Scope → Store.
//!
//! This module hosts the **Filter** stage: the gate that decides whether an
//! extracted draft becomes a record, what visibility it carries on entry,
//! and what redaction / fencing has been applied. Every function here is a
//! pure transform — no I/O, no async, no side effects beyond returning a
//! value or a typed error. The Filter stage runs **before** any
//! `MemoryStore` write, so it cannot leak raw bodies to disk.
//!
//! Brief sources:
//! - §5.2 Write path (`shouldMemorize` + `redact` + `fence`)
//! - §6.3 Visibility tiers (default = `private`/`session`)
//! - §14 Privacy and Consent (pre-persist redaction, deny-by-default,
//!   per-sensor opt-in, append-only audit)

pub mod filter;
