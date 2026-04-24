#[test]
fn crate_name_matches() {
    assert_eq!(env!("CARGO_PKG_NAME"), "cairn-store-sqlite");
}
