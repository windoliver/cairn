#![allow(missing_docs)]

#[test]
fn lint_schema_exposes_skillify_flags_and_findings() {
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schema/verbs/lint.json")).expect("schema");
    let flags = schema["x-cairn-cli"]["flags"].as_array().expect("flags");
    assert!(flags.iter().any(|f| f["name"] == "skill"));
    assert!(flags.iter().any(|f| f["name"] == "fix_skill_plan"));

    let kinds = schema["$defs"]["Kind"]["enum"].as_array().expect("kinds");
    for expected in [
        "skill_missing_artifact",
        "skill_unreachable",
        "skill_duplicate_lane",
        "skill_gate_failed",
        "skill_rollback_broken",
    ] {
        assert!(kinds.iter().any(|kind| kind == expected), "{expected}");
    }
}
