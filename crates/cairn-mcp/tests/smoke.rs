// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

// Force a real link edge on cairn-core so the boundary test exercises the
// declared dependency, not just a Cargo.toml entry. `cairn-core` has no
// public items yet, so we import it for its side effect only.
use cairn_core as _;

#[test]
fn depends_on_core() {
    assert_eq!(env!("CARGO_PKG_NAME"), "cairn-mcp");
}
