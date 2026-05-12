//! `AutoUserProfile` synthesis (brief §7.1, issue #81).
//!
//! Pure functions that turn a slim projection of stored memory records
//! into the typed `DataProfile` document returned by
//! `retrieve --profile` (§8.0.c) and consumed by `assemble_hot` for the
//! hot prefix (§7).
//!
//! The synthesizer never touches the store. Callers (CLI / SDK / MCP
//! adapters wired in #82) project active, non-tombstoned records into
//! [`ProfileSourceRecord`] values, hand them to [`synthesize()`], and
//! emit the typed result.
//!
//! ## Why pure
//!
//! - **Repeatable** — the same inputs always produce the same profile.
//!   The forget-propagation contract (§7.1, "Profile lines can be removed
//!   by record-level forget of their source evidence") relies on
//!   re-synthesizing from a smaller record set after a forget.
//! - **Adapter-free** — `cairn-core` has no `SQLite` / vault dependency, so
//!   this module sits inside the dep-boundary enforced by
//!   `scripts/check-core-boundary.sh`.
//!
//! ## P0 vs P1
//!
//! Per brief §7.1, only the structural split (the `static` / `dynamic`
//! halves) and the `key_facts` aggregation are P0; the rolling `summary`
//! / `historical_summary` narratives are produced by `DreamWorkflow` and
//! remain empty strings here at P0.

mod source;
mod synthesize;

#[cfg(test)]
mod tests;

pub use source::{KeyFactFacet, ProfileSourceRecord, ProfileSubject};
pub use synthesize::{SynthesizeError, synthesize};
