//! Smoke test — verifies that `cairn_cli::verbs` is a reachable public module.

#[test]
fn verbs_mod_exists() {
    // Compilation of this file is the test — if cairn_cli::verbs is missing
    // this file will not compile.
    let _: fn() = cairn_cli::verbs::smoke_fn;
}
