//! Integration cover for `scripts/install-smoke.sh` (issue #100). Runs the
//! script against the just-built binary so the install smoke contract is part
//! of the regular `cargo nextest` run, not just the release-dry-run workflow.

use std::path::PathBuf;
use std::process::Command;

#[test]
fn install_smoke_script_passes_against_built_binary() {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("scripts")
        .join("install-smoke.sh");
    assert!(
        script.is_file(),
        "install-smoke.sh missing at {}",
        script.display()
    );

    let bin = env!("CARGO_BIN_EXE_cairn");
    let out = Command::new("bash")
        .arg(&script)
        .env("CAIRN_BIN", bin)
        .output()
        .expect("run install-smoke.sh");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "install-smoke failed: status={:?}\nstdout: {stdout}\nstderr: {stderr}",
        out.status
    );
    for verb in [
        "ok: bootstrap",
        "ok: status",
        "ok: ingest",
        "ok: search",
        "ok: retrieve",
        "ok: lint",
        "ok: forget",
    ] {
        assert!(
            stdout.contains(verb),
            "missing `{verb}` in smoke output: {stdout}"
        );
    }
}
