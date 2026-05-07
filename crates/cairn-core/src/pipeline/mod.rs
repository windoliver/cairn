//! Pure pipeline functions for the write path (brief §5.2).
//!
//! Capture → Tool-squash → Extract → **Filter** → Classify → Scope → Store.
//!
//! Stages between sensor capture and store upsert that operate as
//! pure transformations: no I/O, no async, no side effects beyond
//! returning a value or a typed error.
//!
//! Modules:
//! - [`dispatch`] — pure routing function deciding per
//!   `CaptureEvent` whether the bytes flow through `squash` (lossy
//!   text compaction) or **bypass** (raw bytes to Extract). Reads
//!   only `event.payload`, never the bytes; replay reproduces the
//!   same decision the dispatch driver took at capture time
//!   (issue #217).
//! - `squash` — tool-output compactor (issue #72). Crate-private:
//!   the squash entry point only proves internal self-consistency
//!   (event validates, payload is `Terminal`, context is
//!   `InteractiveTty`, hash matches the supplied bytes), so exposing
//!   it publicly would let an external caller fabricate a terminal
//!   envelope around arbitrary bytes and drive lossy compaction. The
//!   pipeline driver in `cairn-core` composes [`dispatch::dispatch`]
//!   then squash internally so the trust chain stays owned by this
//!   crate.
//! - [`filter`] — visibility / redaction / fencing gate that runs
//!   **before** any `MemoryStore` write, so it cannot leak raw bodies
//!   to disk.
//!
//! Brief sources:
//! - §5.2 Write path (`shouldMemorize` + `redact` + `fence`)
//! - §6.3 Visibility tiers (default = `private`/`session`)
//! - §14 Privacy and Consent (pre-persist redaction, deny-by-default,
//!   per-sensor opt-in, append-only audit)

pub mod capture_trace;
pub mod dispatch;
pub mod entity_resolve;
pub mod explain;
pub mod extract;
pub mod filter;
pub mod lint;
pub(crate) mod squash;
pub mod turn;

/// Fuzz-only re-export of the squash module's internal entrypoint.
/// Gated behind the `fuzz` crate feature; production builds keep
/// the `squash` module crate-private. Issue #217 keeps it that way
/// deliberately: although `dispatch()` makes squash eligibility
/// derivable from persisted capture data, exposing the squash
/// constructor publicly would let external callers fabricate a
/// `Terminal + InteractiveTty` envelope around arbitrary bytes and
/// drive lossy compaction — `try_from_terminal_event` only proves
/// internal self-consistency, not that the bytes came from a trusted
/// capture path. The next issue's pipeline driver will compose
/// dispatch + squash inside `cairn-core` so the trust chain stays
/// owned by this crate.
#[cfg(feature = "fuzz")]
pub mod squash_fuzz {
    pub use super::squash::{SquashConfig, SquashOutput, fuzz_entrypoint};
}
