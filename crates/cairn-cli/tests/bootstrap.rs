//! Integration tests for vault bootstrap (brief §3.1, issue #41).

use std::path::Path;

use cairn_cli::vault::{BootstrapOpts, bootstrap};

fn opts(dir: &Path) -> BootstrapOpts {
    BootstrapOpts { vault_path: dir.to_path_buf(), force: false }
}

#[allow(dead_code)]
fn forced(dir: &Path) -> BootstrapOpts {
    BootstrapOpts { vault_path: dir.to_path_buf(), force: true }
}

#[test]
fn bootstrap_creates_full_tree() {
    let dir = tempfile::tempdir().unwrap();
    bootstrap(&opts(dir.path())).unwrap();

    let expected_dirs = [
        "sources",
        "sources/articles",
        "sources/papers",
        "sources/transcripts",
        "sources/documents",
        "sources/chat",
        "sources/assets",
        "raw",
        "wiki",
        "wiki/entities",
        "wiki/concepts",
        "wiki/summaries",
        "wiki/synthesis",
        "wiki/prompts",
        "skills",
        ".cairn",
        ".cairn/evolution",
        ".cairn/cache",
        ".cairn/models",
    ];
    for rel in &expected_dirs {
        assert!(
            dir.path().join(rel).is_dir(),
            "expected dir missing: {rel}"
        );
    }
}

#[test]
fn bootstrap_receipt_counts_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let receipt = bootstrap(&opts(dir.path())).unwrap();
    assert_eq!(receipt.dirs_created.len(), 19, "first run: all 19 dirs should be created");
    assert_eq!(receipt.dirs_existing.len(), 0);
}
