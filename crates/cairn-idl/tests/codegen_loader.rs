//! Loader tests. Each test feeds the loader an IDL root and asserts the
//! returned [`RawDocument`] contains the expected files / errors.

use std::path::PathBuf;

use cairn_idl::codegen::loader::{load, RawDocument};

fn schema_dir() -> PathBuf {
    PathBuf::from(cairn_idl::SCHEMA_DIR)
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
