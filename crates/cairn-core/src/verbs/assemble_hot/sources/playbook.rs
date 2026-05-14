//! `ActivePlaybook` source — single most-recently-updated `playbook` record.

use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use crate::verbs::assemble_hot::admissibility::admit;
use crate::verbs::assemble_hot::inclusion::{
    ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment,
};
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

use super::render::render_record_block;

const TOP_K: usize = 1;

/// Select the single most-recently-updated admissible `playbook` record.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    let mut admissible: Vec<(InclusionTrace, &MemoryRecord)> = Vec::new();
    let mut excluded: Vec<ExclusionTrace> = Vec::new();

    for &record in inputs.playbook_candidates {
        // NotPinned is reused as the wire-form "wrong kind for this
        // source" reason. Spec §"Selection rules per source" calls this
        // a NotPinned-equivalent. A future WrongKind variant could
        // disambiguate; revisit if downstream tooling needs it.
        if record.kind != MemoryKind::Playbook {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason: ExclusionReason::NotPinned,
            });
            continue;
        }
        if let Err(reason) = admit(record, &inputs.scope, inputs.authorized_visibility) {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason,
            });
            continue;
        }
        admissible.push((
            InclusionTrace {
                record_id: record.id.clone(),
                score: 0.0,
                note: "most recent updated_at",
            },
            record,
        ));
    }

    admissible.sort_by(|a, b| {
        b.1.updated_at
            .cmp_chronological(&a.1.updated_at)
            .then_with(|| b.0.record_id.as_str().cmp(a.0.record_id.as_str()))
    });

    let overflow = admissible.split_off(TOP_K.min(admissible.len()));
    for (trace, _) in overflow {
        excluded.push(ExclusionTrace {
            record_id: trace.record_id,
            reason: ExclusionReason::BeyondTopK,
        });
    }

    let mut body = String::new();
    let mut included: Vec<InclusionTrace> = Vec::with_capacity(admissible.len());
    for (trace, record) in admissible {
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

    fn playbook_record(id: &str, updated_at: &str) -> MemoryRecord {
        let mut r = sample_record();
        r.id = crate::domain::RecordId::parse(id).expect("valid");
        r.target_id = crate::domain::TargetId::parse(id).expect("valid");
        r.kind = MemoryKind::Playbook;
        r.updated_at = Rfc3339Timestamp::parse(updated_at).expect("valid");
        r
    }

    fn input_with<'a>(records: &'a [&'a MemoryRecord]) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &[],
            project_candidates: &[],
            playbook_candidates: records,
            rolling_summary_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn most_recent_wins() {
        let old = playbook_record("01HQZX9F5N0000000000000001", "2026-04-20T12:00:00Z");
        let new_p = playbook_record("01HQZX9F5N0000000000000002", "2026-04-22T14:00:00Z");
        let recs = [&old, &new_p];
        let seg = select(&input_with(&recs));
        assert_eq!(seg.included.len(), 1);
        assert_eq!(seg.included[0].record_id, new_p.id);
        assert_eq!(seg.excluded.len(), 1);
        assert_eq!(seg.excluded[0].reason, ExclusionReason::BeyondTopK);
    }

    #[test]
    fn empty_input_emits_empty_body() {
        let seg = select(&input_with(&[]));
        assert!(seg.body.is_empty());
        assert!(seg.included.is_empty());
        assert!(seg.excluded.is_empty());
    }

    #[test]
    fn rejects_non_playbook_kind() {
        let mut r = playbook_record("01HQZX9F5N0000000000000001", "2026-04-22T14:00:00Z");
        r.kind = MemoryKind::Project;
        let recs = [&r];
        let seg = select(&input_with(&recs));
        assert!(seg.included.is_empty());
        assert_eq!(seg.excluded[0].reason, ExclusionReason::NotPinned);
    }
}
