//! `RecentUserSignal` source — `user_signal` records inside the last 24h.

use crate::domain::Rfc3339Timestamp;
use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use crate::verbs::assemble_hot::admissibility::admit;
use crate::verbs::assemble_hot::inclusion::{
    ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment,
};
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

use super::render::render_record_block;

const RECENCY_WINDOW_SECS: i64 = 24 * 60 * 60;

/// Select all admissible `user_signal` records updated within the last 24 h,
/// sorted by `updated_at` descending.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    let now_dt = inputs.now.as_chrono();
    let mut admissible: Vec<(InclusionTrace, &MemoryRecord)> = Vec::new();
    let mut excluded: Vec<ExclusionTrace> = Vec::new();

    for &record in inputs.user_signal_candidates {
        // NotPinned is reused as the wire-form "wrong kind for this
        // source" reason. Spec §"Selection rules per source" calls this
        // a NotPinned-equivalent. A future WrongKind variant could
        // disambiguate; revisit if downstream tooling needs it.
        if record.kind != MemoryKind::UserSignal {
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
        if !within_window(&record.updated_at, &now_dt) {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason: ExclusionReason::OutsideRecencyWindow,
            });
            continue;
        }
        admissible.push((
            InclusionTrace {
                record_id: record.id.clone(),
                score: 0.0,
                note: "last 24h, updated_at desc",
            },
            record,
        ));
    }

    admissible.sort_by(|a, b| {
        b.1.updated_at
            .cmp_chronological(&a.1.updated_at)
            .then_with(|| b.0.record_id.as_str().cmp(a.0.record_id.as_str()))
    });

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

fn within_window(updated_at: &Rfc3339Timestamp, now_dt: &chrono::DateTime<chrono::Utc>) -> bool {
    let upd_dt = updated_at.as_chrono();
    let age = (*now_dt - upd_dt).num_seconds();
    (0..=RECENCY_WINDOW_SECS).contains(&age)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn signal(id: &str, updated_at: &str) -> MemoryRecord {
        let mut r = sample_record();
        r.id = crate::domain::RecordId::parse(id).expect("valid");
        r.target_id = crate::domain::TargetId::parse(id).expect("valid");
        r.kind = MemoryKind::UserSignal;
        r.updated_at = Rfc3339Timestamp::parse(updated_at).expect("valid");
        r
    }

    fn input_with<'a>(records: &'a [&'a MemoryRecord], now: &str) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &[],
            project_candidates: &[],
            playbook_candidates: &[],
            skill_graph_snapshot: None,
            rolling_summary_candidates: &[],
            user_signal_candidates: records,
            now: Rfc3339Timestamp::parse(now).expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn includes_record_at_window_boundary() {
        // 2026-04-21T15:00:00Z → 2026-04-22T15:00:00Z is exactly 86_400 s.
        let r = signal("01HQZX9F5N0000000000000001", "2026-04-21T15:00:00Z");
        let recs = [&r];
        let seg = select(&input_with(&recs, "2026-04-22T15:00:00Z"));
        assert_eq!(seg.included.len(), 1);
    }

    #[test]
    fn excludes_record_one_second_past_window() {
        let r = signal("01HQZX9F5N0000000000000001", "2026-04-21T14:59:59Z");
        let recs = [&r];
        let seg = select(&input_with(&recs, "2026-04-22T15:00:00Z"));
        assert!(seg.included.is_empty());
        assert_eq!(
            seg.excluded[0].reason,
            ExclusionReason::OutsideRecencyWindow
        );
    }

    #[test]
    fn rejects_non_signal_kind() {
        let mut r = signal("01HQZX9F5N0000000000000001", "2026-04-22T14:59:59Z");
        r.kind = MemoryKind::Project;
        let recs = [&r];
        let seg = select(&input_with(&recs, "2026-04-22T15:00:00Z"));
        assert!(seg.included.is_empty());
        assert_eq!(seg.excluded[0].reason, ExclusionReason::NotPinned);
    }
}
