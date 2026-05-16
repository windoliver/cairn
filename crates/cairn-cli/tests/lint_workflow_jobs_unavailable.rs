//! Issue #92 round-7 finding 7.1: when the `workflow_jobs` reader
//! cannot be constructed (DB present but unreadable, schema probe
//! failed, index probe failed), the lint pipeline must surface a
//! `DeferredCheck` Info finding so the operator does not mistake
//! "no `workflow_health` findings" for "queue is healthy".

use cairn_core::config::CairnConfig;
use cairn_core::generated::verbs::lint::{Kind, Severity};
use cairn_store_sqlite::SqliteIdentityRegistry;
use cairn_test_fixtures::store::FixtureStore;

#[tokio::test]
async fn workflow_jobs_db_missing_workflow_jobs_table_emits_deferred_check_finding() {
    // Stage a vault whose `.cairn/cairn.db` is a valid SQLite
    // database with the full canonical schema (so `lint`'s other
    // passes — source-forget, consent journal — can run), then drop
    // the `workflow_jobs` table to simulate post-migration drift.
    // The column probe in `SqliteWorkflowJobsReader::new` will
    // surface `ColumnMissing` and lint must catch the `Err` and emit
    // a `DeferredCheck` Info finding rather than the silent-no-op
    // round-7 finding 7.1 called out.
    let vault = tempfile::tempdir().expect("tempdir");
    let cairn_dir = vault.path().join(".cairn");
    std::fs::create_dir_all(&cairn_dir).expect("mk .cairn dir");
    let db_path = cairn_dir.join("cairn.db");
    // Apply the canonical migration chain via `open_sync` so the DB
    // is identical to a real vault, then drop `workflow_jobs` to
    // simulate the schema-drift case.
    {
        let _conn = cairn_store_sqlite::open_sync(&db_path).expect("apply migrations");
    }
    let conn = rusqlite::Connection::open(&db_path).expect("reopen for surgery");
    conn.execute("DROP TABLE workflow_jobs", [])
        .expect("drop workflow_jobs to simulate drift");
    drop(conn);

    let store = FixtureStore::default();
    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();

    let result = cairn_cli::verbs::lint::lint_handler(
        &store,
        &registry,
        None,
        &cfg,
        false, // write_report = false
        vault.path(),
    )
    .await
    .expect("lint must not abort on unavailable workflow_jobs reader");

    let deferred: Vec<&cairn_core::generated::verbs::lint::Finding> = result
        .data
        .findings
        .iter()
        .filter(|f| {
            matches!(f.kind, Kind::DeferredCheck)
                && matches!(f.severity, Severity::Info)
                && f.message.contains("workflow_health")
                && f.message.contains("workflow_jobs reader unavailable")
        })
        .collect();
    assert_eq!(
        deferred.len(),
        1,
        "exactly one workflow_jobs-unavailable DeferredCheck finding: \
         got {} findings overall, full set: {:?}",
        result.data.findings.len(),
        result.data.findings
    );
    assert!(
        deferred[0].suggested_fix.is_some(),
        "deferred check must carry a remediation hint"
    );

    // Summary aggregates stay consistent: this finding bumps
    // `total`, `by_severity.info`, and `by_kind["deferred_check"]`.
    let by_kind = result
        .data
        .summary
        .by_kind
        .as_object()
        .expect("by_kind is an object");
    let dc_count = by_kind
        .get("deferred_check")
        .and_then(serde_json::Value::as_u64)
        .expect("deferred_check entry present");
    assert!(
        dc_count >= 1,
        "by_kind.deferred_check must include the workflow_jobs finding: got {dc_count}"
    );
}

#[tokio::test]
async fn workflow_jobs_db_absent_emits_no_deferred_check_finding() {
    // Sanity check the silent-absent path: a vault that has never
    // run a workflow (and therefore has no `.cairn/cairn.db`) must
    // NOT see a workflow_jobs-unavailable finding. The DeferredCheck
    // is reserved for *unexpected* unavailability (DB present but
    // broken), not for the fresh-init case.
    let vault = tempfile::tempdir().expect("tempdir");
    // Intentionally do NOT create .cairn/cairn.db.

    let store = FixtureStore::default();
    let registry = SqliteIdentityRegistry::open_in_memory().expect("registry");
    let cfg = CairnConfig::default();

    let result =
        cairn_cli::verbs::lint::lint_handler(&store, &registry, None, &cfg, false, vault.path())
            .await
            .expect("lint");

    let workflow_jobs_findings: Vec<_> = result
        .data
        .findings
        .iter()
        .filter(|f| f.message.contains("workflow_jobs reader unavailable"))
        .collect();
    assert!(
        workflow_jobs_findings.is_empty(),
        "absent DB should not emit workflow_jobs-unavailable findings: {workflow_jobs_findings:?}"
    );
}
