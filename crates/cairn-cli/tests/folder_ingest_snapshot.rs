//! Snapshot test: folder ingest human summary output is stable.

use cairn_cli::verbs::ingest::report::{FolderIngestSummary, render_human};

#[test]
fn folder_ingest_human_snapshot() {
    let summary = FolderIngestSummary {
        scanned: 3,
        cached: 1,
        processed: 2,
        skipped: 4,
        warnings: 0,
        entities_new: 5,
        entities_merged: 0,
        edges_new: 1,
        contradictions_resolved: 0,
        records_written: 0,
        plans: 2,
        batch_size: 64,
        operation_ids: vec!["01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()],
        elapsed_ms: 2300,
        dry_run: true,
        mode: "keyword".to_string(),
    };

    insta::assert_snapshot!("folder_ingest_human", render_human("./docs", &summary));
}
