//! Integration tests for `cairn lint` projection diagnostics.

use std::future::Future;
use std::process::Command;

use cairn_core::{
    contract::memory_store::{MemoryStore, ProjectionApplyItem},
    domain::{
        projection::{
            ProjectionCursor, ProjectionItemState, ProjectionLedgerRow, ProjectionTarget,
        },
        record::RecordId,
    },
};
use cairn_store_sqlite::SqliteMemoryStore;

const RECORD_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write_nexus_config(vault: &std::path::Path) {
    let dir = vault.join(".cairn");
    std::fs::create_dir_all(&dir).expect("create .cairn");
    std::fs::write(
        dir.join("config.yaml"),
        "store:\n  kind: nexus-sandbox\n  nexus:\n    endpoint: http://127.0.0.1:1\n",
    )
    .expect("write config");
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("test runtime")
        .block_on(future)
}

fn record_id(raw: &str) -> RecordId {
    RecordId::parse(raw).expect("valid test ULID")
}

fn seed_failed_projection(vault: &std::path::Path) {
    let store = SqliteMemoryStore::open(&vault.join(".cairn/cairn.db")).expect("open sqlite");
    store
        .insert_test_record(RECORD_ID, "failed projection record", 1, "sha256:record-a")
        .expect("insert record");
    block_on(store.apply_projection_items(vec![ProjectionApplyItem {
        row: ProjectionLedgerRow {
            target: ProjectionTarget::Bm25sLexical,
            cursor: ProjectionCursor {
                record_id: record_id(RECORD_ID),
                wal_sequence: 1,
                record_hash: "sha256:record-a".to_owned(),
                source_hash: None,
            },
            state: ProjectionItemState::Failed {
                reason: "parser failed".to_owned(),
            },
            updated_at: "2026-05-19T12:00:00Z".to_owned(),
        },
    }]))
    .expect("apply failed projection");
}

#[test]
fn lint_json_reports_projection_sidecar_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--json"])
        .output()
        .expect("cairn lint --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(
        stdout.contains("projection_sidecar_unavailable"),
        "{stdout}"
    );
}

#[test]
fn lint_fix_mentions_projection_rebuild() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--fix", "--json"])
        .output()
        .expect("cairn lint --fix --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    assert!(stdout.contains("\"rebuildable\":true"), "{stdout}");
}

#[test]
fn lint_json_reports_projection_failed_summary() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_nexus_config(dir.path());
    seed_failed_projection(dir.path());

    let out = cli()
        .current_dir(dir.path())
        .args(["lint", "--json"])
        .output()
        .expect("cairn lint --json");

    assert!(out.status.success(), "exit: {:?}", out.status);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).expect("lint json");
    assert_eq!(value["summary"]["projection_failed"], 1);
    assert!(value["summary"]["projection_stale"].is_null());
    let findings = value["findings"].as_array().expect("findings array");
    assert!(
        findings
            .iter()
            .any(|finding| finding["kind"] == "projection_failed")
    );
    assert!(
        !findings
            .iter()
            .any(|finding| finding["kind"] == "projection_stale")
    );
}
