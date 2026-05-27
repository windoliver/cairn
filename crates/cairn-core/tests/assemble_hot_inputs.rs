//! End-to-end integration: default-recipe assembly with mixed-kind
//! fixtures. Asserts the prefix bytes stay under budget and every
//! recipe step contributes exactly one segment.

use cairn_core::config::HotMemoryConfig;
use cairn_core::domain::Rfc3339Timestamp;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::scope::ScopeTuple;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::verbs::assemble_hot::{HotMemoryInputs, assemble_hot_with_inputs};
use proptest::prelude::*;

#[test]
fn default_recipe_with_mixed_records_stays_within_budget() {
    let mut user = sample_record();
    user.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000001").expect("valid");
    user.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000001").expect("valid");
    user.kind = MemoryKind::User;

    let mut project = sample_record();
    project.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000002").expect("valid");
    project.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000002").expect("valid");
    project.kind = MemoryKind::Project;

    let mut playbook = sample_record();
    playbook.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000003").expect("valid");
    playbook.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000003").expect("valid");
    playbook.kind = MemoryKind::Playbook;

    let mut signal = sample_record();
    signal.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000004").expect("valid");
    signal.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000004").expect("valid");
    signal.kind = MemoryKind::UserSignal;
    // user_signal needs a recent timestamp inside the 24h window.
    signal.updated_at = Rfc3339Timestamp::parse("2026-04-22T14:30:00Z").expect("valid");

    let pinned = [&user];
    let projects = [&project];
    let playbooks = [&playbook];
    let signals = [&signal];

    let inputs = HotMemoryInputs {
        purpose_md: "# Purpose\nact as a careful agent.\n",
        index_md: "# Index\n- a.md\n- b.md\n",
        pinned_candidates: &pinned,
        project_candidates: &projects,
        playbook_candidates: &playbooks,
        skill_graph_snapshot: None,
        rolling_summary_candidates: &[],
        user_signal_candidates: &signals,
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
        scope: ScopeTuple::default(),
        authorized_visibility: &[MemoryVisibility::Private],
        include_debug: false,
    };
    let cfg = HotMemoryConfig::default();
    let data = assemble_hot_with_inputs(&inputs, &cfg).expect("assemble");

    assert!(data.bytes <= u64::from(cfg.max_bytes));
    let segments = data.segments.expect("segments emitted");
    assert_eq!(segments.len(), cfg.recipe.len());
    assert!(data.prefix.contains("user prefers dark mode"));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn assemble_hot_is_deterministic_for_same_inputs(seed in 0u64..1024) {
        let mut r = sample_record();
        r.id = cairn_core::domain::RecordId::parse(format!(
            "01HQZX9F5N0000000000000{:03}",
            seed % 1000
        )).expect("valid id");
        r.target_id = cairn_core::domain::TargetId::parse(format!(
            "01HQZX9F5N0000000000000{:03}",
            seed % 1000
        )).expect("valid target");
        r.kind = MemoryKind::User;
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
            include_debug: false,
        };
        let cfg = HotMemoryConfig::default();
        let a = assemble_hot_with_inputs(&inputs, &cfg).expect("assemble");
        let b = assemble_hot_with_inputs(&inputs, &cfg).expect("assemble");
        prop_assert_eq!(a.prefix, b.prefix);
        prop_assert_eq!(a.bytes, b.bytes);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn assemble_hot_either_fits_budget_or_fails_typed(
        max_bytes in 0u32..1024,
        body_size in 0usize..512,
    ) {
        // Drive a non-empty body through the purpose source so byte
        // counts vary with the proptest-supplied size; the recipe is
        // the default 6-step shape so segments overhead is constant.
        let purpose: String = "p".repeat(body_size);
        let mut cfg = HotMemoryConfig {
            max_bytes,
            ..HotMemoryConfig::default()
        };
        if let Some(preset) = cfg.recipes.get_mut(&cfg.default_recipe.clone()) {
            preset.max_bytes = max_bytes;
        }
        let inputs = HotMemoryInputs {
            purpose_md: &purpose,
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
            include_debug: false,
        };
        match assemble_hot_with_inputs(&inputs, &cfg) {
            Ok(data) => prop_assert!(data.bytes <= u64::from(max_bytes)),
            Err(cairn_core::verbs::assemble_hot::AssembleHotError::BudgetExceeded {
                got,
                max,
            }) => {
                prop_assert!(got > max);
                prop_assert_eq!(max, u64::from(max_bytes));
            }
            Err(other) => prop_assert!(false, "unexpected error: {other:?}"),
        }
    }
}
