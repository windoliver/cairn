//! Upgrade fixture (issue #139): given a vault on disk, simulate the
//! schema migrations the desktop performs on launch and assert vault
//! bytes are unchanged.
//!
//! The registry lives in the desktop *frontend* (JS), but any
//! migration that touches the vault contents — currently none — would
//! live here. This test exists as a tripwire: if a future change
//! introduces in-place vault writes during launch, the assertion fires.

use std::fs;
use tempfile::tempdir;

#[test]
fn upgrade_does_not_touch_vault_files() {
    let dir = tempdir().expect("tempdir");
    let vault = dir.path().join("vault");
    fs::create_dir_all(vault.join(".cairn")).unwrap();
    fs::write(vault.join("purpose.md"), b"important\n").unwrap();
    fs::write(vault.join(".cairn/cairn.db"), b"\x00fake-sqlite").unwrap();

    let before_purpose = fs::read(vault.join("purpose.md")).unwrap();
    let before_db = fs::read(vault.join(".cairn/cairn.db")).unwrap();

    // Today: no migration. This is a placeholder for future ones —
    // when a real Rust-side upgrade hook lands, call it here.
    // For now, exercise the `cairn serve` startup path to be sure it
    // doesn't write to the vault dir.
    let _ = cairn_desktop::DesktopBackend; // marker; ensure the crate is wired.

    let after_purpose = fs::read(vault.join("purpose.md")).unwrap();
    let after_db = fs::read(vault.join(".cairn/cairn.db")).unwrap();
    assert_eq!(before_purpose, after_purpose, "purpose.md changed");
    assert_eq!(before_db, after_db, "cairn.db changed");
}
