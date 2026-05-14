//! Fixture-backed desktop backend tests.

use cairn_desktop::fixture::DesktopFixture;

#[test]
fn fixture_loads_alpha_vault_records_and_folders() {
    let fixture = DesktopFixture::load_default().expect("fixture loads");

    assert_eq!(fixture.vault.id, "desktop-alpha");
    assert_eq!(fixture.folders.len(), 2);
    assert_eq!(fixture.records.len(), 3);
    assert!(
        fixture
            .records
            .iter()
            .any(|record| record.id == "rec-alpha-001" && record.links == ["rec-alpha-002"])
    );
}

#[test]
fn fixture_contains_lint_and_reconcile_examples() {
    let fixture = DesktopFixture::load_default().expect("fixture loads");

    assert_eq!(fixture.lint_findings.len(), 1);
    assert_eq!(
        fixture.reconcile_examples.mutable_record_id,
        "rec-alpha-001"
    );
    assert_eq!(fixture.reconcile_examples.immutable_field, "confidence");
}
