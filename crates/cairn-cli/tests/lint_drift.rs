//! Issue #254 AC#1: read-only lint emits `projection_drift` / `projection_missing`.

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::MemoryStore as _;
use cairn_core::generated::verbs::lint::{Kind, Severity};
use cairn_store_sqlite::SqliteIdentityRegistry;
use cairn_test_fixtures::store::{FixtureStore, sample_record};

#[tokio::test]
async fn read_only_lint_flags_missing_projection() {
    let store = FixtureStore::default();
    store.upsert(&sample_record()).await.expect("upsert");

    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();
    let vault = tempfile::tempdir().expect("tempdir");

    let result = cairn_cli::verbs::lint::lint_handler(
        &store,
        &registry,
        None,
        &cfg,
        false, // write_report = false
        vault.path(),
    )
    .await
    .expect("lint");

    let missing: Vec<_> = result
        .data
        .findings
        .iter()
        .filter(|f| matches!(f.kind, Kind::ProjectionMissing))
        .collect();
    assert_eq!(missing.len(), 1, "exactly one missing-projection finding");
    assert!(matches!(missing[0].severity, Severity::Warning));

    // No disk mutation in read-only mode.
    let count = std::fs::read_dir(vault.path()).map_or(0, Iterator::count);
    assert_eq!(count, 0, "read-only lint must not write to vault");
}

#[tokio::test]
async fn read_only_lint_flags_drift_against_existing_file() {
    use cairn_core::contract::memory_store::ListArgs;
    use cairn_core::domain::projection::MarkdownProjector;

    let store = FixtureStore::default();
    let r = sample_record();
    store.upsert(&r).await.expect("upsert");
    let stored = store
        .list_active_stored(&ListArgs::default())
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let projected = MarkdownProjector.project(&stored);

    let vault = tempfile::tempdir().expect("tempdir");
    let abs = vault.path().join(&projected.path);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, "totally different content\n").unwrap();

    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();
    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint");

    let drifts: Vec<_> = result
        .data
        .findings
        .iter()
        .filter(|f| matches!(f.kind, Kind::ProjectionDrift))
        .collect();
    assert_eq!(drifts.len(), 1);

    // Disk untouched — content must be exactly what we wrote.
    let on_disk = std::fs::read_to_string(&abs).unwrap();
    assert_eq!(on_disk, "totally different content\n");
}

#[tokio::test]
async fn read_only_lint_no_drift_when_projection_matches() {
    use cairn_core::contract::memory_store::ListArgs;
    use cairn_core::domain::projection::MarkdownProjector;

    let store = FixtureStore::default();
    let r = sample_record();
    store.upsert(&r).await.expect("upsert");
    let stored = store
        .list_active_stored(&ListArgs::default())
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let projected = MarkdownProjector.project(&stored);

    let vault = tempfile::tempdir().expect("tempdir");
    let abs = vault.path().join(&projected.path);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    std::fs::write(&abs, &projected.content).unwrap();

    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();
    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint");

    let projection_findings: Vec<_> = result
        .data
        .findings
        .iter()
        .filter(|f| matches!(f.kind, Kind::ProjectionDrift | Kind::ProjectionMissing))
        .collect();
    assert!(
        projection_findings.is_empty(),
        "no drift expected when on-disk file matches projection; got {projection_findings:?}"
    );
}
