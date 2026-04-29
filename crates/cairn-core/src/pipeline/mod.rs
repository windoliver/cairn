//! Pure pipeline functions for the write path (brief §5.2).
//!
//! Capture → Tool-squash → Extract → **Filter** → Classify → Scope → Store.
//!
//! Stages between sensor capture and store upsert that operate as
//! pure transformations: no I/O, no async, no side effects beyond
//! returning a value or a typed error.
//!
//! Modules:
//! - [`squash`] — tool-output compactor (issue #72). Crate-internal
//!   until the dispatch driver (#217) and persisted `TerminalContext`
//!   (#218) land: the squash gate's eligibility must be derivable from
//!   persisted capture data, not a caller-supplied side input — keeping
//!   the surface inside the crate prevents misuse.
//! - [`filter`] — visibility / redaction / fencing gate that runs
//!   **before** any `MemoryStore` write, so it cannot leak raw bodies
//!   to disk.
//!
//! Brief sources:
//! - §5.2 Write path (`shouldMemorize` + `redact` + `fence`)
//! - §6.3 Visibility tiers (default = `private`/`session`)
//! - §14 Privacy and Consent (pre-persist redaction, deny-by-default,
//!   per-sensor opt-in, append-only audit)

pub mod filter;
pub(crate) mod squash;
