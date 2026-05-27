//! Rolling-summary source — turns `reasoning`-kind episodic summaries
//! into hot-prefix segments (issue #90, brief §5.3).
//!
//! This module is not yet wired into a `HotRecipeStep`. Adding
//! `RollingSummary` to the `HotRecipeStep` IDL enum requires a protocol
//! version bump to `cairn.mcp.v2` (per the IDL schema comment). The
//! selection logic here is complete; the render integration deferred to
//! the IDL extension PR.
//!
//! # Selection algorithm
//!
//! Recency-biased: newest admissible `reasoning` record per session wins,
//! up to `MAX_PER_SESSION` summaries per session (currently 1).

use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use crate::verbs::assemble_hot::admissibility::admit;
use crate::verbs::assemble_hot::inclusion::{
    ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment,
};
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

use super::render::render_record_block;

/// Maximum number of rolling summaries selected per session.
pub const MAX_PER_SESSION: usize = 1;

/// Select at most [`MAX_PER_SESSION`] admissible `reasoning` records per
/// session, newest-first, from `inputs.rolling_summary_candidates`.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    // Group admissible candidates by session_id, collecting traces.
    // We need two passes: first collect per-session admissible records,
    // then apply the per-session TOP-K cap.
    let mut by_session: std::collections::BTreeMap<String, Vec<(InclusionTrace, &MemoryRecord)>> =
        std::collections::BTreeMap::new();
    let mut excluded: Vec<ExclusionTrace> = Vec::new();

    for &record in inputs.rolling_summary_candidates {
        if record.kind != MemoryKind::Reasoning {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                // NotPinned is the conventional "wrong kind for this source"
                // reason (see playbook.rs and user_signal.rs for precedent).
                reason: ExclusionReason::NotPinned,
            });
            continue;
        }
        // Rolling summaries without a session_id are not session-scoped
        // episodic summaries — skip them.
        let Some(session) = record.scope.session_id.clone() else {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason: ExclusionReason::OutOfScope,
            });
            continue;
        };
        if let Err(reason) = admit(record, &inputs.scope, inputs.authorized_visibility) {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason,
            });
            continue;
        }
        by_session.entry(session).or_default().push((
            InclusionTrace {
                record_id: record.id.clone(),
                score: 0.0,
                note: "newest rolling summary per session",
            },
            record,
        ));
    }

    // For each session: sort newest-first, take up to MAX_PER_SESSION,
    // push the overflow into `excluded`.
    let mut all_admissible: Vec<(InclusionTrace, &MemoryRecord)> = Vec::new();
    for (_session, mut group) in by_session {
        group.sort_by(|a, b| {
            b.1.updated_at
                .cmp_chronological(&a.1.updated_at)
                .then_with(|| b.0.record_id.as_str().cmp(a.0.record_id.as_str()))
        });
        let overflow = group.split_off(MAX_PER_SESSION.min(group.len()));
        for (trace, _) in overflow {
            excluded.push(ExclusionTrace {
                record_id: trace.record_id,
                reason: ExclusionReason::BeyondTopK,
            });
        }
        all_admissible.extend(group);
    }

    // Emit bodies in BTreeMap session-key order (deterministic).
    let mut body = String::new();
    let mut included: Vec<InclusionTrace> = Vec::with_capacity(all_admissible.len());
    for (trace, record) in all_admissible {
        body.push_str(&render_record_block(record));
        included.push(trace);
    }

    LoadedSegment {
        body,
        included,
        excluded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Rfc3339Timestamp;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn summary(session: &str, updated_at: &str) -> MemoryRecord {
        let mut r = sample_record();
        r.kind = MemoryKind::Reasoning;
        r.scope.session_id = Some(session.to_owned());
        r.updated_at = Rfc3339Timestamp::parse(updated_at).expect("valid timestamp");
        r
    }

    fn input_with<'a>(records: &'a [&'a MemoryRecord]) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &[],
            project_candidates: &[],
            playbook_candidates: &[],
            skill_graph_snapshot: None,
            rolling_summary_candidates: records,
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn empty_input_emits_empty_segment() {
        let seg = select(&input_with(&[]));
        assert!(seg.body.is_empty());
        assert!(seg.included.is_empty());
        assert!(seg.excluded.is_empty());
    }

    #[test]
    fn newest_summary_per_session_wins() {
        let old = summary("sess-a", "2026-04-20T12:00:00Z");
        let new_s = summary("sess-a", "2026-04-22T14:00:00Z");
        let recs = [&old, &new_s];
        let seg = select(&input_with(&recs));
        assert_eq!(seg.included.len(), 1);
        // The newer record should be included.
        assert_eq!(seg.included[0].record_id, new_s.id);
        // The older one is pushed to excluded with BeyondTopK.
        assert_eq!(seg.excluded.len(), 1);
        assert_eq!(seg.excluded[0].reason, ExclusionReason::BeyondTopK);
    }

    #[test]
    fn one_winner_per_session() {
        let a1 = summary("sess-a", "2026-04-22T14:00:00Z");
        let b1 = summary("sess-b", "2026-04-22T13:00:00Z");
        let recs = [&a1, &b1];
        let seg = select(&input_with(&recs));
        assert_eq!(seg.included.len(), 2);
        assert!(seg.excluded.is_empty());
    }

    #[test]
    fn skips_non_reasoning_kind() {
        let mut r = sample_record();
        r.kind = MemoryKind::UserSignal;
        r.scope.session_id = Some("sess-z".to_owned());
        let recs = [&r];
        let seg = select(&input_with(&recs));
        assert!(seg.included.is_empty());
        assert_eq!(seg.excluded.len(), 1);
        assert_eq!(seg.excluded[0].reason, ExclusionReason::NotPinned);
    }

    #[test]
    fn skips_reasoning_without_session_id() {
        let mut r = sample_record();
        r.kind = MemoryKind::Reasoning;
        r.scope.session_id = None;
        let recs = [&r];
        let seg = select(&input_with(&recs));
        assert!(seg.included.is_empty());
        assert_eq!(seg.excluded.len(), 1);
        assert_eq!(seg.excluded[0].reason, ExclusionReason::OutOfScope);
    }
}
