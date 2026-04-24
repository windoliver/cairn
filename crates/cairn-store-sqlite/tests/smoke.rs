// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

#[test]
fn crate_name_matches() {
    assert_eq!(env!("CARGO_PKG_NAME"), "cairn-store-sqlite");
}
