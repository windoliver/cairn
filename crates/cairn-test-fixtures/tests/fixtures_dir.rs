//! Confirms `fixtures_dir()` resolves to the workspace `fixtures/` directory
//! and that the directory exists on disk.

use std::path::PathBuf;

#[test]
fn fixtures_dir_resolves_and_exists() {
    let dir = cairn_test_fixtures::fixtures_dir();
    assert!(
        dir.is_absolute(),
        "fixtures_dir should be absolute, got {dir:?}"
    );
    assert!(
        dir.exists(),
        "fixtures dir should exist on disk, got {dir:?}"
    );
    assert!(
        dir.is_dir(),
        "fixtures dir should be a directory, got {dir:?}"
    );
    assert_eq!(
        dir.file_name().and_then(|s| s.to_str()),
        Some("fixtures"),
        "fixtures dir should be named `fixtures`, got {dir:?}",
    );

    // README planted in Step 8.1 must be visible through the helper.
    let readme: PathBuf = dir.join("README.md");
    assert!(
        readme.is_file(),
        "fixtures/README.md should exist, got {readme:?}"
    );
}
