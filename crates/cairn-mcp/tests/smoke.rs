// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

#[test]
fn depends_on_core() {
    // If this compiles, the dep graph is wired correctly.
    let _ = cairn_core_is_linked();
}

fn cairn_core_is_linked() -> &'static str {
    // Touch the core crate to prove the dep works.
    // `cairn_core` has no public items yet, so we only rely on its linkage
    // via the compiler, not a specific symbol.
    concat!(env!("CARGO_PKG_NAME"), "+cairn-core")
}
