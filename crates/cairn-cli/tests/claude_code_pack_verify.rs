//! End-to-end test: `cairn plugins verify` should exit 0 with the bundled
//! pack conformance passing.

use cairn_cli::packs::verify::run_pack_conformance;

#[test]
fn bundled_pack_passes_all_cases() {
    let outcomes = run_pack_conformance("cairn-claude-code");
    let failures: Vec<_> = outcomes
        .iter()
        .filter(|o| o.status.is_err())
        .map(|o| format!("{:?} {} — {:?}", o.tier, o.name, o.status))
        .collect();
    assert!(failures.is_empty(), "failures: {failures:?}");
}
