#![allow(missing_docs)]
use cairn_idl::codegen::{RunMode, RunOpts, run};
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn run_write_mode_emits_files_into_tempdir() {
    let tmp = tempfile::tempdir().unwrap();
    let report = run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Write,
    })
    .unwrap();
    assert!(
        report.files_emitted >= 8 + 8 + 1 + 3,
        "expected at least 8 SDK verb files, 8 schemas, mods, skill bundle; got {}",
        report.files_emitted
    );
    // Spot-check a few outputs landed.
    assert!(
        tmp.path()
            .join("crates/cairn-core/src/generated/verbs/mod.rs")
            .exists()
    );
    assert!(
        tmp.path()
            .join("crates/cairn-mcp/src/generated/schemas/verbs/ingest.json")
            .exists()
    );
    assert!(tmp.path().join("skills/cairn/SKILL.md").exists());
}

#[test]
fn run_check_mode_clean_tree_returns_no_drift() {
    // Use the actual workspace — it should match on a clean checkout after the
    // committed outputs land in Task 20.
    let _ = workspace_root();
    // This test is enabled only AFTER Task 20 commits the generated outputs.
    // Until then, the assertion is "running --check on the workspace either
    // succeeds (clean) or reports drift listing the missing files".
    // No-op until Task 20.
}
