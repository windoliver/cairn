//! Smoke test — confirms the crate compiles and re-exports the crate name.

#[test]
fn crate_name_is_cairn_core() {
    assert_eq!(env!("CARGO_PKG_NAME"), "cairn-core");
}
