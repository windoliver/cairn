//! Re-running codegen against an already-clean workspace is a no-op.

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
fn second_run_produces_no_drift() {
    // First run writes outputs to a tempdir.
    let tmp = tempfile::tempdir().unwrap();
    run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Write,
    })
    .unwrap();

    // Second run in Check mode should report zero drift.
    let report = run(&RunOpts {
        workspace_root: tmp.path().to_path_buf(),
        mode: RunMode::Check,
    })
    .unwrap();
    assert!(
        report.drift.is_empty(),
        "second run reports drift: {:?}",
        report.drift
    );
}

#[test]
fn workspace_check_is_clean() {
    // After Task 20 commits the outputs, --check on the actual workspace must pass.
    let report = run(&RunOpts {
        workspace_root: workspace_root(),
        mode: RunMode::Check,
    })
    .unwrap();
    assert!(
        report.drift.is_empty(),
        "drift in committed workspace: {:?}",
        report.drift
    );
}
