//! `include_debug` round-trip: every recipe step emits a `HotStepTrace`,
//! and an excluded record carries a typed reason.

use cairn_core::config::HotMemoryConfig;
use cairn_core::domain::Rfc3339Timestamp;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::scope::ScopeTuple;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::generated::verbs::assemble_hot::{HotExclusionReason, HotRecipeStep};
use cairn_core::verbs::assemble_hot::{HotMemoryInputs, assemble_hot_with_inputs};

#[test]
fn debug_field_present_when_requested() {
    let cfg = HotMemoryConfig::default();
    let inputs = HotMemoryInputs {
        purpose_md: "",
        index_md: "",
        pinned_candidates: &[],
        project_candidates: &[],
        playbook_candidates: &[],
        skill_graph_snapshot: None,
        rolling_summary_candidates: &[],
        user_signal_candidates: &[],
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
        scope: ScopeTuple::default(),
        authorized_visibility: &[MemoryVisibility::Private],
        include_debug: true,
    };
    let data = assemble_hot_with_inputs(&inputs, &cfg).expect("assemble");
    let debug = data.debug.expect("debug present");
    assert_eq!(debug.steps.len(), cfg.recipe.len());
}

#[test]
fn auth_failed_exclusions_are_redacted_from_debug() {
    // OutOfScope / VisibilityDenied / ForgottenScope must NOT appear in
    // the wire trace: the candidate slot can contain records the caller
    // is not authorized to see, and emitting their record_id on the
    // debug surface would turn --explain into an enumeration channel.
    let mut visible = sample_record();
    visible.kind = MemoryKind::User;
    let mut hidden = sample_record();
    hidden.kind = MemoryKind::User;
    hidden.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000099").unwrap();
    hidden.target_id = cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000099").unwrap();
    hidden.visibility = MemoryVisibility::Org; // not in allowlist
    let pinned = [&visible, &hidden];

    let inputs = HotMemoryInputs {
        purpose_md: "",
        index_md: "",
        pinned_candidates: &pinned,
        project_candidates: &[],
        playbook_candidates: &[],
        skill_graph_snapshot: None,
        rolling_summary_candidates: &[],
        user_signal_candidates: &[],
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
        scope: ScopeTuple::default(),
        authorized_visibility: &[MemoryVisibility::Private],
        include_debug: true,
    };
    let cfg = HotMemoryConfig::default();
    let data = assemble_hot_with_inputs(&inputs, &cfg).expect("assemble");
    let debug = data.debug.expect("debug present");
    let pinned_step = debug
        .steps
        .iter()
        .find(|t| t.step == HotRecipeStep::PinnedFeedback)
        .expect("pinned step trace");
    assert!(
        !pinned_step
            .excluded
            .iter()
            .any(|e| e.record_id == hidden.id.as_str()),
        "auth-failed record_id leaked into debug trace: {:?}",
        pinned_step.excluded
    );
    assert_eq!(
        pinned_step.included.len(),
        1,
        "the visible record must be included"
    );
    assert_eq!(pinned_step.included[0].record_id, visible.id.as_str());
}

#[test]
fn excluded_record_carries_typed_reason() {
    let mut r = sample_record();
    r.kind = MemoryKind::User;
    r.confidence = 0.1; // below floor
    let pinned = [&r];
    let inputs = HotMemoryInputs {
        purpose_md: "",
        index_md: "",
        pinned_candidates: &pinned,
        project_candidates: &[],
        playbook_candidates: &[],
        skill_graph_snapshot: None,
        rolling_summary_candidates: &[],
        user_signal_candidates: &[],
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
        scope: ScopeTuple::default(),
        authorized_visibility: &[MemoryVisibility::Private],
        include_debug: true,
    };
    let cfg = HotMemoryConfig::default();
    let data = assemble_hot_with_inputs(&inputs, &cfg).expect("assemble");
    let debug = data.debug.expect("debug present");
    let pinned_step = debug
        .steps
        .iter()
        .find(|t| t.step == HotRecipeStep::PinnedFeedback)
        .expect("pinned step trace");
    assert_eq!(pinned_step.excluded.len(), 1);
    assert_eq!(
        pinned_step.excluded[0].reason,
        HotExclusionReason::BelowConfidenceFloor
    );
}
