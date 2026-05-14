//! Smoke test: build a fixture-driven `HotMemoryInputs` and call
//! `assemble_hot` end-to-end. Full `SQLite` + filesystem adapter wiring
//! is owned by issue #80; this test only proves the pure path is
//! reachable from outside `cairn-core`.

use cairn_core::config::HotMemoryConfig;
use cairn_core::domain::Rfc3339Timestamp;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::scope::ScopeTuple;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::verbs::assemble_hot::{HotMemoryInputs, assemble_hot_with_inputs};

#[test]
fn assemble_hot_runs_with_minimal_inputs() {
    let mut r = sample_record();
    r.kind = MemoryKind::User;
    let pinned = [&r];
    let inputs = HotMemoryInputs {
        purpose_md: "# Purpose\nact carefully.\n",
        index_md: "# Index\n",
        pinned_candidates: &pinned,
        project_candidates: &[],
        playbook_candidates: &[],
        user_signal_candidates: &[],
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
        scope: ScopeTuple::default(),
        authorized_visibility: &[MemoryVisibility::Private],
        include_debug: false,
    };
    let cfg = HotMemoryConfig::default();
    let data = assemble_hot_with_inputs(&inputs, &cfg).expect("assemble");
    assert!(data.bytes > 0);
    let segments = data.segments.expect("segments emitted");
    assert_eq!(segments.len(), cfg.recipe.len());
}
