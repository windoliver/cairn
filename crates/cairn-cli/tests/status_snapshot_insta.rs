//! Snapshot matrix for `cairn status --json` over the v0.1 config space.
//! Volatile fields (`incarnation`, `started_at`, `version`, `build`) are
//! masked so snapshots stay deterministic across builds. Run
//! `cargo insta review` to update intentional changes; reject any change
//! that mutates `capabilities[]` semantics without a corresponding spec
//! update.

use std::path::Path;
use std::process::Command;

fn cairn_bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    // Avoid CWD-pollution from the workspace root's transient `.cairn/`
    // (e.g. `.cairn/models/` from an embedding-model download).
    cmd.current_dir(std::env::temp_dir());
    cmd.env_remove("CAIRN_VAULT");
    cmd
}

fn write_vault_id(root: &Path) {
    std::fs::create_dir_all(root.join(".cairn")).unwrap();
    std::fs::write(
        root.join(".cairn").join("vault.id"),
        b"01HZZ0000000000000000000AB\n",
    )
    .unwrap();
}

fn mask_status_volatiles(v: &mut serde_json::Value) {
    if let Some(si) = v["server_info"].as_object_mut() {
        si.insert("incarnation".into(), "<masked>".into());
        si.insert("started_at".into(), "<masked>".into());
        si.insert("version".into(), "<masked>".into());
        si.insert("build".into(), "<masked>".into());
    }
    if let Some(path) = v.pointer_mut("/health/authority_db/path") {
        *path = "<masked>".into();
    }
}

/// Run `cairn --vault <dir> status --json` with optional config overrides.
/// Returns the parsed JSON with volatile fields masked.
fn run_status(dir: &Path, config_yaml: Option<&str>) -> serde_json::Value {
    if let Some(yaml) = config_yaml {
        std::fs::write(dir.join(".cairn").join("config.yaml"), yaml).unwrap();
    }
    let out = cairn_bin()
        .args(["--vault", dir.to_str().unwrap(), "status", "--json"])
        .output()
        .expect("spawn cairn");
    assert!(
        out.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
    );
    let mut v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    mask_status_volatiles(&mut v);
    v
}

#[test]
fn snapshot_default_p0_bound_vault() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    let v = run_status(tmp.path(), None);
    insta::assert_json_snapshot!("default_p0_bound_vault", v);
}

#[test]
fn snapshot_local_embeddings_off() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    let v = run_status(
        tmp.path(),
        Some("vault:\n  name: test\nsearch:\n  local_embeddings: false\n"),
    );
    insta::assert_json_snapshot!("local_embeddings_off", v);
}

#[test]
fn snapshot_unbound_dir() {
    let tmp = tempfile::tempdir().unwrap();
    // No .cairn/vault.id written — explicit --vault to an unbound dir
    // should produce empty capabilities. The CLI exits EX_CONFIG when
    // an explicit vault target is unbound; this test exercises the
    // implicit-CWD path that returns empty capabilities. Use cairn_bin
    // with no --vault flag, run from the unbound tempdir directly.
    let out = {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
        cmd.current_dir(tmp.path());
        cmd.env_remove("CAIRN_VAULT");
        cmd.args(["status", "--json"]);
        cmd.output().expect("spawn cairn")
    };
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    mask_status_volatiles(&mut v);
    insta::assert_json_snapshot!("unbound_dir", v);
}

#[test]
fn snapshot_sdk_new_no_store() {
    let mut v = serde_json::to_value(cairn_sdk::Sdk::new().status()).expect("serialize");
    mask_status_volatiles(&mut v);
    insta::assert_json_snapshot!("sdk_new_no_store", v);
}
