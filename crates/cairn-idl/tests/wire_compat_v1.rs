//! Wire-compat gate for `cairn.mcp.v1` — issue #98, brief §8.0.a / §15.
//!
//! Snapshots a fingerprint over every contract file in canonical order
//! (manifest + envelope + errors + capabilities + extensions + common +
//! prelude + verbs + plugin) and an exact-equality check between
//! `index.json`'s `x-cairn-files` map and `CONTRACT_FILES`. Together
//! these catch: edits to any contract file, reorder/regroup of
//! `x-cairn-files`, edits to manifest-only fields like
//! `x-cairn-verb-ids`, and added/removed/renamed schema files that
//! kept the same cardinality.
#![allow(missing_docs)]

use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

fn schema_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema")
}

/// Canonical category order within `index.json#x-cairn-files`. Pinned
/// here so a reordered or renamed category is a hard fail rather than
/// a silent drift (Rust's default `serde_json::Map` is `BTreeMap`,
/// so iteration order is alphabetical — relying on it would mask
/// canonical-order drift).
const CATEGORY_ORDER: &[&str] = &[
    "envelope",
    "errors",
    "capabilities",
    "extensions",
    "common",
    "prelude",
    "verbs",
    "plugin",
];

/// Canonical contract-file list — `index.json` plus every entry in its
/// `x-cairn-files` map, in canonical category order. Pinned so a missing,
/// added, or reordered file is a hard fail. Must equal
/// `["index.json"]` ++ `flatten(index.json#x-cairn-files in CATEGORY_ORDER)`.
const CONTRACT_FILES: &[&str] = &[
    "index.json",
    "envelope/request.json",
    "envelope/response.json",
    "envelope/signed_intent.json",
    "errors/error.json",
    "capabilities/capabilities.json",
    "extensions/registry.json",
    "common/primitives.json",
    "common/record_exclusion.json",
    "common/scope_filter.json",
    "prelude/status.json",
    "prelude/handshake.json",
    "verbs/ingest.json",
    "verbs/search.json",
    "verbs/retrieve.json",
    "verbs/summarize.json",
    "verbs/assemble_hot.json",
    "verbs/capture_trace.json",
    "verbs/lint.json",
    "verbs/forget.json",
    "plugin/manifest.json",
];

fn read_index() -> serde_json::Value {
    let path = schema_root().join("index.json");
    let bytes = fs::read(&path).unwrap_or_else(|err| panic!("read index.json: {err}"));
    serde_json::from_slice(&bytes).unwrap_or_else(|err| panic!("parse index.json: {err}"))
}

#[test]
fn manifest_fingerprint_matches_snapshot() {
    let root = schema_root();
    let mut hasher = Sha256::new();
    for rel in CONTRACT_FILES {
        let bytes = fs::read(root.join(rel)).unwrap_or_else(|err| panic!("read {rel}: {err}"));
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
        hasher.update([0u8]);
    }
    let digest = format!("{:x}", hasher.finalize());
    insta::assert_snapshot!("manifest_fingerprint", digest);
}

#[test]
fn per_file_snapshots_match() {
    let root = schema_root();
    for rel in CONTRACT_FILES {
        let bytes = fs::read(root.join(rel)).unwrap_or_else(|err| panic!("read {rel}: {err}"));
        let body = String::from_utf8(bytes).unwrap_or_else(|err| panic!("utf8 {rel}: {err}"));
        // Slug: replace `/` and `.` with `_` for a flat snap key.
        let slug = rel.replace(['/', '.'], "_");
        insta::assert_snapshot!(format!("file__{slug}"), body);
    }
}

#[test]
fn contract_files_matches_index_json_exactly() {
    let index = read_index();
    let files = index
        .get("x-cairn-files")
        .and_then(serde_json::Value::as_object)
        .expect("index.json: x-cairn-files must be an object");

    let mut actual_categories: Vec<&str> = files.keys().map(String::as_str).collect();
    actual_categories.sort_unstable();
    let mut expected_categories: Vec<&str> = CATEGORY_ORDER.to_vec();
    expected_categories.sort_unstable();
    assert_eq!(
        actual_categories, expected_categories,
        "index.json#x-cairn-files category set drifted from CATEGORY_ORDER. \
         A category was added, removed, or renamed; update CATEGORY_ORDER \
         (and CONTRACT_FILES) to keep the wire-compat gate honest."
    );

    let mut flattened: Vec<String> = vec!["index.json".to_string()];
    for category in CATEGORY_ORDER {
        let arr = files
            .get(*category)
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("x-cairn-files.{category} must be an array"));
        for entry in arr {
            let rel = entry
                .as_str()
                .unwrap_or_else(|| panic!("x-cairn-files.{category} entries must be strings"));
            flattened.push(rel.to_string());
        }
    }

    let expected: Vec<String> = CONTRACT_FILES.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        flattened, expected,
        "CONTRACT_FILES drifted from index.json#x-cairn-files. The wire-compat \
         snapshot set must equal `[\"index.json\"]` ++ flatten(x-cairn-files in \
         CATEGORY_ORDER). Update CONTRACT_FILES (and possibly CATEGORY_ORDER) \
         to match — or revert the schema/index.json edit if it was unintended."
    );
}
