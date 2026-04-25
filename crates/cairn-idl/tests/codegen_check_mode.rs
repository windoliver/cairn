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
