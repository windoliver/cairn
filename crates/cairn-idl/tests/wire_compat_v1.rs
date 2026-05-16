//! Wire-compat gate for `cairn.mcp.v1` — issue #98, brief §8.0.a / §15.
//!
//! Snapshots a single fingerprint over every contract file in
//! canonical order (envelope + errors + capabilities + extensions +
//! common + prelude + verbs + plugin manifest). Any unintentional edit
//! to a schema produces a one-line `.snap` diff; intentional edits
//! require `cargo insta accept` and an entry in the PR description.
#![allow(missing_docs)]

use std::fs;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

fn schema_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema")
}

/// Canonical contract-file list — must match `schema/index.json`'s
/// `x-cairn-files` order. Pinned here so a missing file is a hard fail
/// rather than a silent drift.
const CONTRACT_FILES: &[&str] = &[
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
fn contract_files_matches_index_json_count() {
    let index_path = schema_root().join("index.json");
    let bytes = std::fs::read(&index_path).unwrap_or_else(|err| panic!("read index.json: {err}"));
    let index: serde_json::Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|err| panic!("parse index.json: {err}"));
    let files = index
        .get("x-cairn-files")
        .and_then(serde_json::Value::as_object)
        .expect("index.json: x-cairn-files must be an object");
    let total: usize = files
        .values()
        .map(|arr| arr.as_array().map_or(0, std::vec::Vec::len))
        .sum();
    assert_eq!(
        total,
        CONTRACT_FILES.len(),
        "CONTRACT_FILES (len={}) drifted from index.json#x-cairn-files (total={}). \
         A new schema file was added to index.json without updating CONTRACT_FILES \
         — add it to keep the wire-compat gate honest.",
        CONTRACT_FILES.len(),
        total,
    );
}
