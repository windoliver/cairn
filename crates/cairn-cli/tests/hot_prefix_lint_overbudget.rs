//! E2E for `hot_memory_over_budget` lint kind: ensure that when
//! `purpose.md` exceeds `vault.hot_memory.max_bytes`, the walker
//! emits a `HotMemoryOverBudget` Error finding via `lint_handler`.
//!
//! This test pins the bug fix where `lint_step_body_sync` was
//! truncating bodies to the budget, masking overflow from the walker.

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
async fn oversized_purpose_md_emits_hot_memory_over_budget() {
    let vault = tempfile::tempdir().expect("vault");
    bootstrap(&opts(vault.path().to_path_buf())).expect("bootstrap");

    // Stuff purpose.md with 4 KB so the default 25 KB budget isn't
    // the bottleneck — instead, we shrink the budget below the file.
    let big_body = "A".repeat(4096);
    std::fs::write(vault.path().join("purpose.md"), &big_body).expect("write");

    let store = FixtureStore::default();
    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let mut cfg = CairnConfig::default();
    cfg.vault.hot_memory.max_bytes = 64; // way smaller than 4 KB.
    // resolve_recipe consults the named-recipe table first; shrink the
    // default `chat` preset's budget so it routes through.
    if let Some(preset) = cfg.vault.hot_memory.recipes.get_mut("chat") {
        preset.max_bytes = 64;
    }

    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint_handler");

    let kinds: Vec<Kind> = result.data.findings.iter().map(|f| f.kind).collect();
    assert!(
        kinds.contains(&Kind::HotMemoryOverBudget),
        "expected HotMemoryOverBudget; got {kinds:?} -- bodies should not be silently truncated to fit the budget"
    );
}
