//! `TopSalienceProject` source — top 6 `project`-kind records by salience.
//!
//! Sort key matches the lint canary regression
//! (`tied_salience_top_k_picks_largest_records_conservatively`):
//! salience desc, then byte size desc as tiebreaker, then `record_id` desc
//! to keep the assembled prefix bytewise-deterministic.

use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use crate::verbs::assemble_hot::admissibility::admit;
use crate::verbs::assemble_hot::inclusion::{
    ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment,
};
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

use super::render::render_record_block;

const TOP_K: usize = 6;

/// Select up to 6 project records ranked by salience.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    let mut admissible: Vec<(InclusionTrace, &MemoryRecord)> = Vec::new();
    let mut excluded: Vec<ExclusionTrace> = Vec::new();

    for &record in inputs.project_candidates {
        // NotPinned is reused as the wire-form "wrong kind for this
        // source" reason. Spec §"Selection rules per source" calls this
        // a NotPinned-equivalent. A future WrongKind variant could
        // disambiguate; revisit if downstream tooling needs it.
        if record.kind != MemoryKind::Project {
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
                score: f64::from(record.salience),
                note: "salience desc",
            },
            record,
        ));
    }

    admissible.sort_by(|a, b| {
        b.0.score
            .partial_cmp(&a.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.body.len().cmp(&a.1.body.len()))
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

    fn project_record(id: &str, salience: f32, body: &str) -> MemoryRecord {
        let mut r = sample_record();
        r.id = crate::domain::RecordId::parse(id).expect("valid");
        r.target_id = crate::domain::TargetId::parse(id).expect("valid");
        r.kind = MemoryKind::Project;
        r.salience = salience;
        r.body = body.to_owned();
        r
    }

    fn input_with<'a>(records: &'a [&'a MemoryRecord]) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &[],
            project_candidates: records,
            playbook_candidates: &[],
            skill_graph_snapshot: None,
            rolling_summary_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn caps_at_top_6() {
        let recs: Vec<MemoryRecord> = (1..=10)
            .map(|i| {
                #[allow(clippy::cast_precision_loss, reason = "i in 1..=10, loss negligible")]
                let salience = i as f32 / 10.0;
                project_record(&format!("01HQZX9F5N00000000000000{i:02}"), salience, "body")
            })
            .collect();
        let record_refs: Vec<&MemoryRecord> = recs.iter().collect();
        let seg = select(&input_with(&record_refs));
        assert_eq!(seg.included.len(), 6);
    }

    #[test]
    fn rejects_non_project_kind_with_not_pinned() {
        let mut rec = project_record("01HQZX9F5N0000000000000001", 0.9, "x");
        rec.kind = MemoryKind::Feedback;
        let recs = [&rec];
        let seg = select(&input_with(&recs));
        assert!(seg.included.is_empty());
        assert_eq!(seg.excluded[0].reason, ExclusionReason::NotPinned);
    }

    #[test]
    fn ties_break_by_body_size_then_record_id() {
        // Six records with identical salience, two of them larger.
        // Top-6 must include the larger ones; the seventh tied record
        // is BeyondTopK.
        let small1 = project_record("01HQZX9F5N0000000000000001", 0.5, "x");
        let small2 = project_record("01HQZX9F5N0000000000000002", 0.5, "x");
        let small3 = project_record("01HQZX9F5N0000000000000003", 0.5, "x");
        let small4 = project_record("01HQZX9F5N0000000000000004", 0.5, "x");
        let small5 = project_record("01HQZX9F5N0000000000000005", 0.5, "x");
        let large1 = project_record("01HQZX9F5N0000000000000006", 0.5, &"L".repeat(100));
        let large2 = project_record("01HQZX9F5N0000000000000007", 0.5, &"L".repeat(100));
        let all_recs = [
            &small1, &small2, &small3, &small4, &small5, &large1, &large2,
        ];
        let seg = select(&input_with(&all_recs));
        let included_ids: Vec<&str> = seg.included.iter().map(|i| i.record_id.as_str()).collect();
        assert!(included_ids.contains(&large1.id.as_str()));
        assert!(included_ids.contains(&large2.id.as_str()));
    }
}
