//! Behavioural test for the `--check` flag.

#![allow(missing_docs)]

use cairn_idl::codegen::{RunMode, RunOpts, run};
use std::path::PathBuf;

fn fork_workspace_outputs() -> tempfile::TempDir {
    // Write a fresh codegen output tree into a tempdir.
    let tmp = tempfile::tempdir().unwrap();
    run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Write,
    })
    .unwrap();
    tmp
}

#[test]
fn check_clean_tree_reports_no_drift() {
    let tmp = fork_workspace_outputs();
    let report = run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Check,
    })
    .unwrap();
    assert!(report.drift.is_empty());
}

#[test]
fn check_after_manual_edit_reports_drift() {
    let tmp = fork_workspace_outputs();
    let target = tmp.path().join("skills/cairn/SKILL.md");
    let mut bytes = std::fs::read(&target).unwrap();
    bytes.extend_from_slice(b"\n<!-- accidental edit -->\n");
    std::fs::write(&target, bytes).unwrap();

    let report = run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Check,
    })
    .unwrap();
    assert_eq!(report.drift, vec![PathBuf::from("skills/cairn/SKILL.md")]);
}

/// A stale file inside an owned generated root must surface as drift, even
/// though no emitter ever produced it. Without this check, removing an IDL
/// source file leaves the corresponding generated artefact committed and
/// `--check` keeps reporting clean.
#[test]
fn check_after_planted_stale_file_reports_drift() {
    let tmp = fork_workspace_outputs();
    let stale_rel = PathBuf::from("crates/cairn-core/src/generated/verbs/zombie.rs");
    let stale_abs = tmp.path().join(&stale_rel);
    std::fs::write(&stale_abs, b"// not part of any emit\n").unwrap();

    let report = run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Check,
    })
    .unwrap();
    assert!(
        report.drift.contains(&stale_rel),
        "stale file {stale_rel:?} not flagged as drift: {:?}",
        report.drift,
    );
}

/// `Write` mode must scrub on-disk files inside an owned root that the latest
/// emit no longer claims — pruning is the active counterpart to the Check
/// detection above.
#[test]
fn write_mode_prunes_stale_files() {
    let tmp = fork_workspace_outputs();
    let stale_rel = PathBuf::from("crates/cairn-cli/src/generated/zombie.rs");
    let stale_abs = tmp.path().join(&stale_rel);
    std::fs::write(&stale_abs, b"// orphan\n").unwrap();
    assert!(stale_abs.exists());

    run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Write,
    })
    .unwrap();
    assert!(
        !stale_abs.exists(),
        "Write mode failed to prune stale {stale_rel:?}",
    );
}
