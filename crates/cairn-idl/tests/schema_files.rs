// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const EXPECTED_VERB_IDS: [&str; 8] = [
    "ingest",
    "search",
    "retrieve",
    "summarize",
    "assemble_hot",
    "capture_trace",
    "lint",
    "forget",
];

const EXPECTED_CONTRACT: &str = "cairn.mcp.v1";

fn schema_dir() -> &'static Path {
    Path::new(cairn_idl::SCHEMA_DIR)
}

fn read_json(path: &Path) -> Value {
    let bytes = fs::read(path)
        .unwrap_or_else(|err| panic!("failed to read {path:?}: {err}"));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|err| panic!("failed to parse {path:?} as JSON: {err}"))
}

fn manifest() -> Value {
    read_json(&schema_dir().join("index.json"))
}

fn manifest_paths() -> Vec<PathBuf> {
    let manifest = manifest();
    let files = manifest
        .get("x-cairn-files")
        .and_then(Value::as_object)
        .expect("index.json: x-cairn-files must be an object");
    let mut out: Vec<PathBuf> = Vec::new();
    for (category, arr) in files {
        let arr = arr
            .as_array()
            .unwrap_or_else(|| panic!("x-cairn-files.{category} must be an array"));
        for entry in arr {
            let rel = entry
                .as_str()
                .unwrap_or_else(|| panic!("x-cairn-files.{category} entries must be strings"));
            out.push(schema_dir().join(rel));
        }
    }
    out
}

fn require_object<'a>(v: &'a Value, path: &Path) -> &'a serde_json::Map<String, Value> {
    v.as_object()
        .unwrap_or_else(|| panic!("{path:?}: top-level value must be a JSON object"))
}

#[test]
fn manifest_parses_and_has_required_top_level_keys() {
    let m = manifest();
    let path = schema_dir().join("index.json");
    let obj = require_object(&m, &path);
    for key in ["$schema", "$id", "title", "x-cairn-contract", "x-cairn-files", "x-cairn-verb-ids"] {
        assert!(obj.contains_key(key), "index.json missing required key {key}");
    }
    assert_eq!(
        obj.get("x-cairn-contract").and_then(Value::as_str),
        Some(EXPECTED_CONTRACT),
        "index.json x-cairn-contract mismatch"
    );
}

#[test]
fn manifest_verb_ids_match_eight_verb_set_in_order() {
    let m = manifest();
    let verb_ids: Vec<String> = m
        .get("x-cairn-verb-ids")
        .and_then(Value::as_array)
        .expect("index.json x-cairn-verb-ids must be an array")
        .iter()
        .map(|v| {
            v.as_str()
                .expect("x-cairn-verb-ids entries must be strings")
                .to_string()
        })
        .collect();
    let expected: Vec<String> = EXPECTED_VERB_IDS.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        verb_ids, expected,
        "x-cairn-verb-ids must match §8.0 exactly, in order"
    );
}

#[test]
fn every_manifest_file_exists_and_parses_and_has_top_level_fields() {
    for path in manifest_paths() {
        assert!(
            path.is_file(),
            "manifest lists {path:?} but file does not exist"
        );
        let v = read_json(&path);
        let obj = require_object(&v, &path);
        for key in ["$schema", "$id", "title", "x-cairn-contract"] {
            assert!(
                obj.contains_key(key),
                "{path:?} missing required top-level key {key}"
            );
        }
        assert_eq!(
            obj.get("x-cairn-contract").and_then(Value::as_str),
            Some(EXPECTED_CONTRACT),
            "{path:?} x-cairn-contract mismatch"
        );
    }
}

#[test]
fn manifest_and_filesystem_are_bijective() {
    // Every .json file under schema/ (except index.json) must be listed.
    let mut on_disk: BTreeSet<PathBuf> = BTreeSet::new();
    walk_json(schema_dir(), &mut on_disk);
    on_disk.remove(&schema_dir().join("index.json"));

    let in_manifest: BTreeSet<PathBuf> = manifest_paths().into_iter().collect();

    let missing_in_manifest: Vec<_> = on_disk.difference(&in_manifest).collect();
    let missing_on_disk: Vec<_> = in_manifest.difference(&on_disk).collect();
    assert!(
        missing_in_manifest.is_empty(),
        "files on disk but not in manifest: {missing_in_manifest:?}"
    );
    assert!(
        missing_on_disk.is_empty(),
        "files in manifest but missing on disk: {missing_on_disk:?}"
    );
}

fn walk_json(dir: &Path, out: &mut BTreeSet<PathBuf>) {
    for entry in fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("failed to read dir {dir:?}: {err}"))
    {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            walk_json(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            out.insert(path);
        }
    }
}

fn capabilities_enum() -> BTreeSet<String> {
    let caps = read_json(&schema_dir().join("capabilities/capabilities.json"));
    let arr = caps
        .get("oneOf")
        .and_then(Value::as_array)
        .expect("capabilities.json: oneOf must be an array");
    arr.iter()
        .map(|entry| {
            entry
                .get("const")
                .and_then(Value::as_str)
                .expect("capabilities.json oneOf entries must have a const string")
                .to_string()
        })
        .collect()
}

fn collect_capability_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            if let Some(cap) = map.get("x-cairn-capability").and_then(Value::as_str) {
                out.push(cap.to_string());
            }
            for (_, child) in map {
                collect_capability_refs(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_capability_refs(item, out);
            }
        }
        _ => {}
    }
}

#[test]
fn every_x_cairn_capability_is_in_capabilities_enum() {
    let enum_set = capabilities_enum();
    for path in manifest_paths() {
        let v = read_json(&path);
        let mut refs: Vec<String> = Vec::new();
        collect_capability_refs(&v, &mut refs);
        for cap in refs {
            assert!(
                enum_set.contains(&cap),
                "{path:?} references capability {cap:?} that is not in capabilities.json"
            );
        }
    }
}
