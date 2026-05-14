//! Privacy regressions: low-confidence, scope mismatch, and visibility
//! denials must exclude records across every source.

use cairn_core::config::HotMemoryConfig;
use cairn_core::domain::Rfc3339Timestamp;
use cairn_core::domain::record::MemoryRecord;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::scope::ScopeTuple;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::verbs::assemble_hot::{HotMemoryInputs, assemble_hot_with_inputs};

fn make_input<'a>(
    pinned: &'a [&'a MemoryRecord],
    visibility: &'a [MemoryVisibility],
    scope: ScopeTuple,
) -> HotMemoryInputs<'a> {
    HotMemoryInputs {
        purpose_md: "",
        index_md: "",
        pinned_candidates: pinned,
        project_candidates: &[],
        playbook_candidates: &[],
        rolling_summary_candidates: &[],
        user_signal_candidates: &[],
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
        scope,
        authorized_visibility: visibility,
        include_debug: true,
    }
}

#[test]
fn pinned_record_with_low_confidence_does_not_appear() {
    let mut r = sample_record();
    r.kind = MemoryKind::User;
    r.confidence = 0.1;
    let pinned = [&r];
    let inputs = make_input(&pinned, &[MemoryVisibility::Private], ScopeTuple::default());
    let cfg = HotMemoryConfig::default();
    let data = assemble_hot_with_inputs(&inputs, &cfg).expect("assemble");
    assert!(!data.prefix.contains(&r.body));
}

#[test]
fn pinned_record_with_visibility_denial_does_not_appear() {
    let mut r = sample_record();
    r.kind = MemoryKind::User;
    r.visibility = MemoryVisibility::Org;
    let pinned = [&r];
    let inputs = make_input(&pinned, &[MemoryVisibility::Private], ScopeTuple::default());
    let cfg = HotMemoryConfig::default();
    let data = assemble_hot_with_inputs(&inputs, &cfg).expect("assemble");
    assert!(!data.prefix.contains(&r.body));
}

#[test]
fn pinned_record_with_scope_mismatch_does_not_appear() {
    let mut r = sample_record();
    r.kind = MemoryKind::User;
    let pinned = [&r];
    let other_user = ScopeTuple {
        user: Some("hmn:other".to_owned()),
        ..ScopeTuple::default()
    };
    let inputs = make_input(&pinned, &[MemoryVisibility::Private], other_user);
    let cfg = HotMemoryConfig::default();
    let data = assemble_hot_with_inputs(&inputs, &cfg).expect("assemble");
    assert!(!data.prefix.contains(&r.body));
}
