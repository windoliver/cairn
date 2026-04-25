//! Loader tests. Each test feeds the loader an IDL root and asserts the
//! returned [`RawDocument`] contains the expected files / errors.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use cairn_idl::codegen::loader::{load, RawDocument};
use tempfile::TempDir;

fn schema_dir() -> PathBuf {
    PathBuf::from(cairn_idl::SCHEMA_DIR)
}

/// Copy the real schema tree into a tempdir so a single file can be mutated
/// without touching the source.
fn fork_schema() -> TempDir {
    let src = schema_dir();
    let dst = tempfile::tempdir().unwrap();
    copy_tree(&src, dst.path());
    dst
}

fn copy_tree(src: &std::path::Path, dst: &std::path::Path) {
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&dst_path).unwrap();
            copy_tree(&entry.path(), &dst_path);
        } else {
            fs::copy(entry.path(), dst_path).unwrap();
        }
    }
}

fn write_json(path: &std::path::Path, value: &serde_json::Value) {
    let mut f = fs::File::create(path).unwrap();
    let bytes = serde_json::to_vec_pretty(value).unwrap();
    f.write_all(&bytes).unwrap();
}

#[test]
fn loads_real_schema_root() {
    let doc: RawDocument = load(&schema_dir()).expect("real schema must load");
    // Manifest pins eight verbs.
    assert_eq!(doc.verbs.len(), 8, "expected 8 verbs, got {}", doc.verbs.len());
    // Two preludes (status, handshake).
    assert_eq!(doc.preludes.len(), 2);
    // index.json itself is captured.
    assert!(doc.index.get("x-cairn-verb-ids").is_some());
}

#[test]
fn rejects_wrong_contract_id() {
    let tmp = fork_schema();
    let path = tmp.path().join("verbs/ingest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["x-cairn-contract"] = serde_json::json!("cairn.mcp.v2");
    write_json(&path, &value);

    let err = load(tmp.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("x-cairn-contract"), "got: {msg}");
}

#[test]
fn rejects_verb_missing_args_defs() {
    let tmp = fork_schema();
    let path = tmp.path().join("verbs/ingest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["$defs"].as_object_mut().unwrap().remove("Args");
    write_json(&path, &value);

    let err = load(tmp.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("Args"), "got: {msg}");
}

#[test]
fn rejects_unknown_capability_reference() {
    let tmp = fork_schema();
    let path = tmp.path().join("verbs/forget.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["x-cairn-capability"] = serde_json::json!("cairn.mcp.v1.does_not_exist");
    write_json(&path, &value);

    let err = load(tmp.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("does_not_exist"), "got: {msg}");
}

#[test]
fn rejects_dangling_ref() {
    let tmp = fork_schema();
    let path = tmp.path().join("verbs/ingest.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    // Replace the Ulid ref in Data.record_id with a nonexistent one.
    value["$defs"]["Data"]["properties"]["record_id"] =
        serde_json::json!({ "$ref": "../common/primitives.json#/$defs/NotARealType" });
    write_json(&path, &value);

    let err = load(tmp.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("NotARealType"), "got: {msg}");
}
