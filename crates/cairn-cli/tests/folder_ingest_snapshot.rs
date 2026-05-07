//! Snapshot test: folder ingest human summary output is stable.

use cairn_cli::verbs::ingest::report::{FolderIngestSummary, render_human};

#[test]
fn folder_ingest_human_snapshot() {
    let summary = FolderIngestSummary {
        scanned: 5,
        cached: 1,
        processed: 4,
        skipped: 0,
        warnings: 0,
        entities_new: 12,
        entities_merged: 0,
        edges_new: 3,
        contradictions_resolved: 0,
        records_written: 4,
        plans: 2,
        batch_size: 2,
        operation_ids: vec![
            "01HQZX9F5N0000000000000000".to_owned(),
            "01HQZX9F5N0000000000000001".to_owned(),
        ],
        elapsed_ms: 1200,
        dry_run: false,
        mode: "keyword".to_owned(),
    };

    insta::assert_snapshot!("folder_ingest_human", render_human("./docs", &summary));
}
