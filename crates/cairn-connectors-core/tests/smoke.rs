//! Compile-only smoke test: confirms the crate builds and the
//! `CONTRACT_VERSION` placeholder is accessible (issue #130 T4).
//! Replaced by a richer test suite in T5–T20.
#![allow(missing_docs)]

/// Verify `CONTRACT_VERSION` is accessible from outside the crate.
#[test]
fn crate_compiles() {
    let _ = cairn_connectors_core::CONTRACT_VERSION;
}
