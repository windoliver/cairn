//! Snapshot the canonical JSON shape of `AssembleHotData` for a
//! deterministic fixture. The `.snap` file is the byte-stability
//! acceptance criterion for issues #288 and #293.
//!
//! One snapshot per built-in recipe (chat / wake-up / debug / handoff)
//! pins the recipe-step ordering, default stability, and content hashes
//! against drift. Bodies are deterministic placeholders keyed by step
//! name so snapshots stay stable when recipe membership changes —
//! adding a step bumps the snapshot for the recipes it appears in,
//! never the unrelated ones.

use cairn_core::config::HotMemoryConfig;
use cairn_core::generated::verbs::assemble_hot::{AssembleHotData, HotRecipeStep};
use cairn_core::verbs::assemble_hot::build_segments;

fn body_for(step: HotRecipeStep) -> &'static str {
    match step {
        HotRecipeStep::Purpose => "purpose body\n",
        HotRecipeStep::Index => "index body\n",
        HotRecipeStep::PinnedFeedback => "pinned\n",
        HotRecipeStep::TopSalienceProject => "salience\n",
        HotRecipeStep::ActivePlaybook => "playbook\n",
        HotRecipeStep::RecentUserSignal => "signal\n",
        _ => "",
    }
}

fn snapshot_for_recipe(name: &str) -> String {
    let cfg = HotMemoryConfig::default();
    let preset = cfg
        .recipes
        .get(name)
        .unwrap_or_else(|| panic!("recipe {name:?} must exist in defaults"));
    let recipe: Vec<HotRecipeStep> = preset
        .steps
        .iter()
        .copied()
        .map(HotRecipeStep::from)
        .collect();
    let bodies: Vec<&str> = recipe.iter().copied().map(body_for).collect();
    let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
    let data = AssembleHotData {
        bytes: prefix.len() as u64,
        prefix,
        recipe: Some(name.to_owned()),
        segments: Some(segments),
        debug: None,
    };
    serde_json::to_string_pretty(&data).unwrap()
}

#[test]
fn assemble_hot_data_canonical_json() {
    insta::assert_snapshot!(snapshot_for_recipe("chat"));
}

#[test]
fn assemble_hot_data_canonical_json_wake_up() {
    insta::assert_snapshot!(snapshot_for_recipe("wake-up"));
}

#[test]
fn assemble_hot_data_canonical_json_debug() {
    insta::assert_snapshot!(snapshot_for_recipe("debug"));
}

#[test]
fn assemble_hot_data_canonical_json_handoff() {
    insta::assert_snapshot!(snapshot_for_recipe("handoff"));
}
