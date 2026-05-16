//! Wire-compat gate for `cairn.mcp.v1` — issue #98, brief §8.0.a / §15.
//!
//! Snapshots a single fingerprint over all 10 contract files in
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
