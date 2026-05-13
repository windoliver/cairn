//! Pure-projection inputs for `assemble_hot` (issue #82).
//!
//! Adapters (`cairn-cli`, `cairn-sdk`) materialize this struct from a
//! `MemoryStore` query + filesystem reads. Core never touches the
//! store or the filesystem.

use crate::domain::Rfc3339Timestamp;
use crate::domain::record::MemoryRecord;
use crate::domain::scope::ScopeTuple;
use crate::domain::taxonomy::MemoryVisibility;

/// Pre-filtered record + filesystem inputs for [`super::assemble_hot`].
///
/// Slot contracts (caller-side responsibility, all enforced by the
/// adapter, double-checked by core):
///
/// * `pinned_candidates`: `kind ∈ {user, feedback} ∧ is_static = 1`,
///   already narrowed to the authorized scope/visibility. `is_static`
///   is a store-side projection (no field on `MemoryRecord`); the
///   caller is the only authority for it.
/// * `project_candidates`: `kind = project`.
/// * `playbook_candidates`: `kind = playbook`.
/// * `user_signal_candidates`: `kind = user_signal`.
/// * `purpose_md` / `index_md`: pre-read file content.
///
/// Each record-bearing slot may include records the source-side ranker
/// later excludes (low confidence, scope mismatch, etc.). Core's
/// admissibility check is the trust boundary, not the slot membership.
#[derive(Debug, Clone)]
pub struct HotMemoryInputs<'a> {
    /// Body of `purpose.md`, already read from disk by the adapter.
    pub purpose_md: &'a str,
    /// Body of `index.md`, already read from disk by the adapter.
    pub index_md: &'a str,
    /// Caller-narrowed pinned-feedback candidates.
    pub pinned_candidates: &'a [&'a MemoryRecord],
    /// `project`-kind candidates for the salience source.
    pub project_candidates: &'a [&'a MemoryRecord],
    /// `playbook`-kind candidates.
    pub playbook_candidates: &'a [&'a MemoryRecord],
    /// `user_signal`-kind candidates.
    pub user_signal_candidates: &'a [&'a MemoryRecord],
    /// Reference instant. Recency windows are computed against this.
    pub now: Rfc3339Timestamp,
    /// Authorized scope for this assembly call (re-checked per record).
    pub scope: ScopeTuple,
    /// Visibility tiers the caller is authorized to see.
    pub authorized_visibility: &'a [MemoryVisibility],
    /// When `true`, the assembler populates `AssembleHotData.debug`.
    pub include_debug: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::taxonomy::MemoryVisibility;

    #[test]
    fn inputs_is_constructible() {
        let r = sample_record();
        let recs = [&r];
        let now = Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid");
        let scope = ScopeTuple::default();
        let allow = [MemoryVisibility::Private];
        let inputs = HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &recs,
            project_candidates: &[],
            playbook_candidates: &[],
            user_signal_candidates: &[],
            now,
            scope,
            authorized_visibility: &allow,
            include_debug: false,
        };
        assert_eq!(inputs.pinned_candidates.len(), 1);
        assert!(!inputs.include_debug);
    }
}
