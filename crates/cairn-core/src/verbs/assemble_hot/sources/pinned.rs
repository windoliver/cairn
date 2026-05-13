//! `PinnedFeedback` source — top 8 `user`/`feedback` records ranked by
//! `salience × recency_decay(now − updated_at)`.
//!
//! Pin semantics for v0.1: caller pre-filters to records with
//! `kind ∈ {user, feedback} ∧ is_static = 1`. Core re-checks the
//! `kind` half (signed payload) but trusts the caller for `is_static`
//! (store-side projection, not on `MemoryRecord`). See spec
//! "Pin semantics" + design brief §7.1.

use crate::domain::Rfc3339Timestamp;
use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use crate::verbs::assemble_hot::admissibility::admit;
use crate::verbs::assemble_hot::inclusion::{
    ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment,
};
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

use super::render::render_record_block;

/// Top-K cap from brief §7 ("top 8 by salience × recency").
const TOP_K: usize = 8;

/// Half-life for the recency decay term, in seconds.
/// 30 days × 86400 s/day. Matches the brief's "salience × recency"
/// shorthand by giving recent records a non-trivial multiplier without
/// fully cliff-sliding old high-salience records.
const RECENCY_HALF_LIFE_SECS: f64 = 30.0 * 86_400.0;

/// Select up to 8 pinned-feedback records for the hot prefix.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    let mut included_with_score: Vec<(InclusionTrace, &MemoryRecord)> = Vec::new();
    let mut excluded: Vec<ExclusionTrace> = Vec::new();

    for &record in inputs.pinned_candidates {
        if !is_pinned_kind(record.kind) {
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
        let score = pin_score(record, &inputs.now);
        included_with_score.push((
            InclusionTrace {
                record_id: record.id.clone(),
                score,
                note: "salience × recency",
            },
            record,
        ));
    }

    // Sort: score desc, then record_id desc as deterministic tiebreaker.
    included_with_score.sort_by(|a, b| {
        b.0.score
            .partial_cmp(&a.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.0.record_id.as_str().cmp(a.0.record_id.as_str()))
    });

    // Bucket overflow into BeyondTopK exclusions before truncation so
    // the debug trace explains why a candidate did not make the cut.
    let overflow = included_with_score.split_off(TOP_K.min(included_with_score.len()));
    for (trace, _) in overflow {
        excluded.push(ExclusionTrace {
            record_id: trace.record_id,
            reason: ExclusionReason::BeyondTopK,
        });
    }

    let mut body = String::new();
    let mut included: Vec<InclusionTrace> = Vec::with_capacity(included_with_score.len());
    for (trace, record) in included_with_score {
        body.push_str(&render_record_block(record));
        included.push(trace);
    }

    LoadedSegment {
        body,
        included,
        excluded,
    }
}

fn is_pinned_kind(kind: MemoryKind) -> bool {
    matches!(kind, MemoryKind::User | MemoryKind::Feedback)
}

fn pin_score(record: &MemoryRecord, now: &Rfc3339Timestamp) -> f64 {
    let age_secs = age_seconds(now, &record.updated_at);
    let decay = (-age_secs / RECENCY_HALF_LIFE_SECS).exp();
    f64::from(record.salience) * decay
}

fn age_seconds(now: &Rfc3339Timestamp, updated_at: &Rfc3339Timestamp) -> f64 {
    // Negative ages (record stamped slightly in the future relative
    // to `now`) clamp to zero so decay never blows up past 1.0.
    let now_dt = now.as_chrono();
    let upd_dt = updated_at.as_chrono();
    let secs = (now_dt - upd_dt).num_seconds().max(0);
    // i64 → f64 widens; clippy::cast_precision_loss is acceptable here
    // because `secs` is bounded by realistic timestamps (≤ ~10⁹ s for
    // the next century) and the decay exp() saturates well before f64
    // precision becomes a problem.
    #[allow(
        clippy::cast_precision_loss,
        reason = "bounded by realistic timestamps"
    )]
    (secs as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn fresh_input<'a>(records: &'a [&'a MemoryRecord], now: &str) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: records,
            project_candidates: &[],
            playbook_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse(now).expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    fn user_record(id: &str, salience: f32, updated_at: &str) -> MemoryRecord {
        let mut r = sample_record();
        r.id = crate::domain::RecordId::parse(id).expect("valid id");
        r.target_id = crate::domain::TargetId::parse(id).expect("valid target");
        r.kind = MemoryKind::User;
        r.salience = salience;
        r.updated_at = Rfc3339Timestamp::parse(updated_at).expect("valid");
        r
    }

    #[test]
    fn ranks_by_salience_times_recency() {
        let recent_low = user_record("01HQZX9F5N0000000000000001", 0.4, "2026-04-22T14:00:00Z");
        let old_high = user_record("01HQZX9F5N0000000000000002", 0.9, "2025-04-22T14:00:00Z");
        let now = "2026-04-22T15:00:00Z";
        let candidates = [&recent_low, &old_high];
        let inputs = fresh_input(&candidates, now);
        let s = select(&inputs);
        // 0.4 × ~1.0 = 0.4 ; 0.9 × exp(-365/30) ≈ 0.9 × 5.7e-6 ≈ 5e-6.
        // Recent low-salience must win.
        assert_eq!(s.included[0].record_id, recent_low.id);
        assert_eq!(s.included[1].record_id, old_high.id);
    }

    #[test]
    fn caps_at_top_8_and_emits_beyond_top_k() {
        let recs: Vec<MemoryRecord> = (1_i32..=12)
            .map(|i| {
                user_record(
                    &format!("01HQZX9F5N00000000000000{i:02}"),
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "small bounded integer, test only"
                    )]
                    (i as f32 / 12.0),
                    "2026-04-22T14:00:00Z",
                )
            })
            .collect();
        let record_refs: Vec<&MemoryRecord> = recs.iter().collect();
        let s = select(&fresh_input(&record_refs, "2026-04-22T15:00:00Z"));
        assert_eq!(s.included.len(), 8);
        let beyond = s
            .excluded
            .iter()
            .filter(|e| e.reason == ExclusionReason::BeyondTopK)
            .count();
        assert_eq!(beyond, 4);
    }

    #[test]
    fn excludes_non_user_feedback_kind_with_not_pinned() {
        let mut r = user_record("01HQZX9F5N0000000000000001", 0.9, "2026-04-22T14:00:00Z");
        r.kind = MemoryKind::Project;
        let s = select(&fresh_input(&[&r], "2026-04-22T15:00:00Z"));
        assert!(s.included.is_empty());
        assert_eq!(s.excluded.len(), 1);
        assert_eq!(s.excluded[0].reason, ExclusionReason::NotPinned);
    }

    #[test]
    fn excludes_low_confidence() {
        let mut r = user_record("01HQZX9F5N0000000000000001", 0.9, "2026-04-22T14:00:00Z");
        r.confidence = 0.2;
        let s = select(&fresh_input(&[&r], "2026-04-22T15:00:00Z"));
        assert_eq!(s.excluded[0].reason, ExclusionReason::BelowConfidenceFloor);
    }

    #[test]
    fn deterministic_tiebreaker_on_equal_score() {
        let a = user_record("01HQZX9F5N0000000000000001", 0.5, "2026-04-22T14:00:00Z");
        let b = user_record("01HQZX9F5N0000000000000002", 0.5, "2026-04-22T14:00:00Z");
        let s = select(&fresh_input(&[&a, &b], "2026-04-22T15:00:00Z"));
        // Tiebreaker: record_id desc → id ending …02 first, then …01.
        assert_eq!(s.included[0].record_id, b.id);
        assert_eq!(s.included[1].record_id, a.id);
    }

    #[test]
    fn future_timestamp_clamps_age_to_zero() {
        let r = user_record(
            "01HQZX9F5N0000000000000001",
            0.5,
            "2026-04-22T15:00:01Z", // 1s after now
        );
        let s = select(&fresh_input(&[&r], "2026-04-22T15:00:00Z"));
        // No panic, no NaN — the record is included with score == salience.
        assert_eq!(s.included.len(), 1);
        assert!((s.included[0].score - 0.5).abs() < 1e-9);
    }
}
