// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Compile-time gate on `ApplyToken` construction.
//!
//! `ApplyToken` carries the WAL-only invariant that no caller outside
//! `cairn_core::wal` may mint one. trybuild verifies that both
//! struct-literal construction and a direct call to the private
//! `new()` fail to compile in user code.

#[test]
fn apply_token_cannot_be_minted_outside_wal() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/apply_token_*.rs");
}
