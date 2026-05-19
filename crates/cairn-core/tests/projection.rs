use cairn_core::domain::{
    projection::{
        ParserProjectionKind, ProjectionCursor, ProjectionItemState, ProjectionLedgerRow,
        ProjectionSummary, ProjectionTarget,
    },
    record::RecordId,
};

fn record_id(raw: &str) -> RecordId {
    RecordId::parse(raw).expect("valid test ULID")
}

#[test]
fn projection_target_keys_are_stable() {
    assert_eq!(ProjectionTarget::Bm25sLexical.as_key(), "bm25s_lexical");
    assert_eq!(
        ProjectionTarget::Parser(ParserProjectionKind::PdfText).as_key(),
        "parser_pdf_text"
    );
    assert_eq!(
        ProjectionTarget::Parser(ParserProjectionKind::VisionCaption).as_key(),
        "parser_vision_caption"
    );
}

#[test]
fn ledger_row_detects_current_hash() {
    let cursor = ProjectionCursor {
        record_id: record_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        wal_sequence: 42,
        record_hash: "sha256:record-a".to_owned(),
        source_hash: Some("sha256:source-a".to_owned()),
    };
    let row = ProjectionLedgerRow {
        target: ProjectionTarget::Bm25sLexical,
        cursor: cursor.clone(),
        state: ProjectionItemState::Current,
        updated_at: "2026-05-19T12:00:00Z".to_owned(),
    };

    assert!(row.is_current_for(&cursor));

    let changed = ProjectionCursor {
        record_hash: "sha256:record-b".to_owned(),
        ..cursor
    };
    assert!(!row.is_current_for(&changed));
}

#[test]
fn summary_counts_lag_and_failures() {
    let summary = ProjectionSummary::from_rows(
        ProjectionTarget::Parser(ParserProjectionKind::DocxText),
        4,
        [
            ProjectionItemState::Current,
            ProjectionItemState::Missing,
            ProjectionItemState::Stale,
            ProjectionItemState::Failed {
                reason: "parser exited 2".to_owned(),
            },
        ],
        Some("2026-05-19T12:00:00Z".to_owned()),
    );

    assert_eq!(summary.total_authoritative_items, 4);
    assert_eq!(summary.current_items, 1);
    assert_eq!(summary.lagging_items, 3);
    assert_eq!(summary.failed_items, 1);
    assert_eq!(summary.target.as_key(), "parser_docx_text");
}
