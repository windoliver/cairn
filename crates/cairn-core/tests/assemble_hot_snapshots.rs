//! Snapshot the canonical JSON shape of `AssembleHotData` for a
//! deterministic fixture. The `.snap` file is the byte-stability
//! acceptance criterion for issue #288.

use cairn_core::generated::verbs::assemble_hot::{AssembleHotData, HotRecipeStep::*};
use cairn_core::verbs::assemble_hot::build_segments;

#[test]
fn assemble_hot_data_canonical_json() {
    let recipe = [
        Purpose,
        Index,
        PinnedFeedback,
        TopSalienceProject,
        ActivePlaybook,
        RecentUserSignal,
    ];
    let bodies = [
        "purpose body\n",
        "index body\n",
        "pinned\n",
        "salience\n",
        "playbook\n",
        "signal\n",
    ];
    let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
    let data = AssembleHotData {
        bytes: prefix.len() as u64,
        prefix,
        segments: Some(segments),
        debug: None,
    };
    let json = serde_json::to_string_pretty(&data).unwrap();
    insta::assert_snapshot!(json);
}
