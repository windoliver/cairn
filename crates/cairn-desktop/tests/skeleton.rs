//! Skeleton checks for the desktop backend crate.

use cairn_desktop::DesktopBackend;

#[test]
fn desktop_backend_type_is_exported() {
    let backend = DesktopBackend;
    assert_eq!(backend, DesktopBackend);
}
