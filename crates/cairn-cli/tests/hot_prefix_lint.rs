//! E2E for hot_memory lint: clean bootstrap → no findings; missing
//! purpose.md → BrokenSourceLink Error. See issue #83.
//!
//! Tests call `lint_handler` directly (the CLI-crate library boundary),
//! following the same pattern as `lint_drift.rs`. The `cairn lint` binary
//! also routes the default (read-only) path through `lint_handler` as of
//! the wiring fix in Task 28; `run_edge_lint` is now only invoked for
//! `cairn lint --fix` (WAL edge contradiction resolution).

// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use cairn_cli::vault::{BootstrapOpts, bootstrap};
use cairn_core::config::CairnConfig;
use cairn_core::generated::verbs::lint::Kind;
use cairn_store_sqlite::SqliteIdentityRegistry;
use cairn_test_fixtures::store::FixtureStore;

fn opts(vault_path: std::path::PathBuf) -> BootstrapOpts {
    BootstrapOpts {
        vault_path,
        force: true,
    }
}

#[tokio::test]
async fn clean_bootstrap_vault_emits_no_hot_memory_findings() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&opts(vault.path().to_path_buf())).expect("bootstrap");

    let store = FixtureStore::default();
    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();

    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint_handler");

    let forbidden_kinds = [
        Kind::BrokenSourceLink,
        Kind::MissingSummary,
        Kind::StaleProfileLine,
        Kind::HotMemoryOverBudget,
    ];
    for kind in forbidden_kinds {
        let count = result
            .data
            .findings
            .iter()
            .filter(|f| f.kind == kind)
            .count();
        assert_eq!(
            count, 0,
            "clean vault must not emit {kind:?}; findings: {:?}",
            result.data.findings
        );
    }
}

#[tokio::test]
async fn missing_purpose_md_emits_broken_source_link() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&opts(vault.path().to_path_buf())).expect("bootstrap");
    // Bootstrap created purpose.md; remove it to trigger the check.
    std::fs::remove_file(vault.path().join("purpose.md")).expect("remove purpose.md");

    let store = FixtureStore::default();
    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();

    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint_handler");

    let has_broken_source_link = result
        .data
        .findings
        .iter()
        .any(|f| f.kind == Kind::BrokenSourceLink);
    assert!(
        has_broken_source_link,
        "expected BrokenSourceLink finding after removing purpose.md; findings: {:?}",
        result.data.findings
    );
    assert!(
        result.has_error,
        "has_error must be true when BrokenSourceLink (Error severity) is present"
    );
}
