//! `cairn reindex` handler for Nexus projection rebuilds.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use cairn_core::config::StoreKind;
use cairn_core::contract::memory_store::{MemoryStore, ProjectionApplyItem, ProjectionRecord};
use cairn_core::domain::projection::{
    ParserProjectionKind, ProjectionCursor, ProjectionItemState, ProjectionLedgerRow,
    ProjectionTarget,
};
use cairn_store_sqlite::SqliteMemoryStore;
use clap::ArgMatches;

use crate::config::{self, CliOverrides};
use crate::nexus::projection::{
    ProjectionApplyRequest, ProjectionClient, ProjectionRequestItem, ProjectionResponseItem,
};

use super::envelope::{emit_json, new_operation_id};

/// Run `cairn reindex`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    if !sub.get_flag("from-db") {
        eprintln!("cairn reindex: --from-db is required");
        return ExitCode::from(64);
    }

    let vault_path = match std::env::current_dir() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("cairn reindex: failed to resolve current directory: {err}");
            return ExitCode::FAILURE;
        }
    };

    let active = match config::load(&vault_path, &CliOverrides::default()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("cairn reindex: {err:#}");
            return ExitCode::from(78);
        }
    };

    if active.store.kind != StoreKind::NexusSandbox {
        eprintln!("cairn reindex: requires store.kind: nexus-sandbox");
        return ExitCode::from(78);
    }

    let summary = match rebuild_from_db(&vault_path, &active) {
        Ok(summary) => summary,
        Err(err) => {
            eprintln!("cairn reindex: {err}");
            return ExitCode::from(69);
        }
    };

    if sub.get_flag("json") {
        emit_json(&serde_json::json!({
            "target": summary.primary_target,
            "items": summary.items,
            "targets": summary.targets,
        }));
    } else {
        println!("target: {}", summary.primary_target);
        println!("items: {}", summary.items);
    }

    ExitCode::SUCCESS
}

/// Result of one rebuild run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReindexSummary {
    /// Primary target kept for stable existing JSON/human output.
    pub primary_target: String,
    /// Total response items applied to the ledger.
    pub items: usize,
    /// Targets attempted.
    pub targets: Vec<String>,
}

/// Rebuild derived projections from authoritative `SQLite` records.
pub(crate) fn rebuild_from_db(
    vault_path: &Path,
    active: &cairn_core::config::CairnConfig,
) -> Result<ReindexSummary, String> {
    if active.store.kind != StoreKind::NexusSandbox {
        return Err("requires store.kind: nexus-sandbox".to_owned());
    }
    let db_path = vault_path.join(".cairn/cairn.db");
    let store = SqliteMemoryStore::open(&db_path)
        .map_err(|err| format!("open {}: {err}", db_path.display()))?;
    let records =
        block_on(store.projection_records()).map_err(|err| format!("projection records: {err}"))?;
    let client = ProjectionClient::new(
        active.store.nexus.endpoint.clone(),
        "/projection/apply".to_owned(),
        Duration::from_millis(active.store.nexus.health_timeout_ms),
    );
    let mut total_items = 0usize;
    let mut targets = Vec::new();
    for (target, items) in projection_batches(&records) {
        let target_key = target.as_key();
        targets.push(target_key.clone());
        let request_items = items
            .iter()
            .map(|record| request_item_for_target(&target, record))
            .collect::<Vec<_>>();
        let request = ProjectionApplyRequest {
            operation_id: new_operation_id().0,
            target: target_key,
            items: request_items,
        };
        let response = client.apply(&request)?;
        let ledger_items = ledger_items_from_response(&target, &items, response.items)?;
        total_items = total_items.saturating_add(ledger_items.len());
        block_on(store.apply_projection_items(ledger_items))
            .map_err(|err| format!("apply projection ledger: {err}"))?;
    }
    Ok(ReindexSummary {
        primary_target: ProjectionTarget::Bm25sLexical.as_key(),
        items: total_items,
        targets,
    })
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("reindex runtime")
        .block_on(future)
}

fn projection_batches(
    records: &[ProjectionRecord],
) -> Vec<(ProjectionTarget, Vec<ProjectionRecord>)> {
    let mut batches = vec![(ProjectionTarget::Bm25sLexical, records.to_vec())];
    for target in [
        ProjectionTarget::Parser(ParserProjectionKind::PdfText),
        ProjectionTarget::Parser(ParserProjectionKind::DocxText),
        ProjectionTarget::Parser(ParserProjectionKind::VideoFrameText),
        ProjectionTarget::Parser(ParserProjectionKind::VisionCaption),
    ] {
        let target_records = records
            .iter()
            .filter(|record| {
                record
                    .source_path
                    .as_deref()
                    .and_then(parser_target_for_source)
                    == Some(target.clone())
            })
            .cloned()
            .collect::<Vec<_>>();
        if !target_records.is_empty() {
            batches.push((target, target_records));
        }
    }
    batches
}

fn request_item_for_target(
    target: &ProjectionTarget,
    record: &ProjectionRecord,
) -> ProjectionRequestItem {
    let source_hash = source_hash_for_target(target, record);
    ProjectionRequestItem {
        record_id: record.cursor.record_id.as_str().to_owned(),
        wal_sequence: record.cursor.wal_sequence,
        record_hash: record.cursor.record_hash.clone(),
        source_hash,
        source_path: source_path_for_target(target, record),
        body: record.body.clone(),
    }
}

fn ledger_item_from_response(
    target: &ProjectionTarget,
    records: &[ProjectionRecord],
    item: ProjectionResponseItem,
) -> Option<ProjectionApplyItem> {
    let record = records
        .iter()
        .find(|record| record.cursor.record_id.as_str() == item.record_id)?;
    if record.cursor.record_hash != item.record_hash {
        return Some(ProjectionApplyItem {
            row: ledger_row(
                target.clone(),
                record,
                ProjectionItemState::Failed {
                    reason: "projection hash mismatch".to_owned(),
                },
            ),
        });
    }
    let source_hash = source_hash_for_target(target, record);
    if source_hash != item.source_hash {
        return Some(ProjectionApplyItem {
            row: ledger_row(
                target.clone(),
                record,
                ProjectionItemState::Failed {
                    reason: "projection source hash mismatch".to_owned(),
                },
            ),
        });
    }
    let state = match item.state.as_str() {
        "current" => ProjectionItemState::Current,
        "missing" => ProjectionItemState::Missing,
        "stale" => ProjectionItemState::Stale,
        "failed" => ProjectionItemState::Failed {
            reason: item
                .reason
                .unwrap_or_else(|| "projection failed".to_owned()),
        },
        other => ProjectionItemState::Failed {
            reason: format!("unknown projection state {other}"),
        },
    };
    Some(ProjectionApplyItem {
        row: ledger_row(target.clone(), record, state),
    })
}

fn ledger_items_from_response(
    target: &ProjectionTarget,
    records: &[ProjectionRecord],
    response_items: Vec<ProjectionResponseItem>,
) -> Result<Vec<ProjectionApplyItem>, String> {
    let expected = records
        .iter()
        .map(|record| record.cursor.record_id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut ledger_items = Vec::new();
    for item in response_items {
        if !expected.contains(&item.record_id) {
            return Err(format!(
                "unexpected projection response for {} from target {}",
                item.record_id,
                target.as_key()
            ));
        }
        if !seen.insert(item.record_id.clone()) {
            return Err(format!(
                "duplicate projection response for {} from target {}",
                item.record_id,
                target.as_key()
            ));
        }
        let ledger_item = ledger_item_from_response(target, records, item).ok_or_else(|| {
            format!(
                "projection response could not be matched for target {}",
                target.as_key()
            )
        })?;
        ledger_items.push(ledger_item);
    }
    if let Some(missing) = expected.difference(&seen).next() {
        return Err(format!(
            "missing projection response for {missing} from target {}",
            target.as_key()
        ));
    }
    Ok(ledger_items)
}

fn ledger_row(
    target: ProjectionTarget,
    record: &ProjectionRecord,
    state: ProjectionItemState,
) -> ProjectionLedgerRow {
    let source_hash = source_hash_for_target(&target, record);
    ProjectionLedgerRow {
        target,
        cursor: ProjectionCursor {
            record_id: record.cursor.record_id.clone(),
            wal_sequence: record.cursor.wal_sequence,
            record_hash: record.cursor.record_hash.clone(),
            source_hash,
        },
        state,
        updated_at: chrono_like_now(),
    }
}

fn parser_target_for_source(path: &str) -> Option<ProjectionTarget> {
    let extension = Path::new(path).extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("pdf") {
        return Some(ProjectionTarget::Parser(ParserProjectionKind::PdfText));
    }
    if extension.eq_ignore_ascii_case("docx") {
        return Some(ProjectionTarget::Parser(ParserProjectionKind::DocxText));
    }
    if extension.eq_ignore_ascii_case("json") && path.to_ascii_lowercase().contains("frame") {
        return Some(ProjectionTarget::Parser(
            ParserProjectionKind::VideoFrameText,
        ));
    }
    if ["png", "jpg", "jpeg", "webp"]
        .iter()
        .any(|image_ext| extension.eq_ignore_ascii_case(image_ext))
    {
        return Some(ProjectionTarget::Parser(
            ParserProjectionKind::VisionCaption,
        ));
    }
    None
}

fn source_hash_for_target(target: &ProjectionTarget, record: &ProjectionRecord) -> Option<String> {
    if matches!(target, ProjectionTarget::Parser(_)) {
        record.source_hash.clone()
    } else {
        None
    }
}

fn source_path_for_target(target: &ProjectionTarget, record: &ProjectionRecord) -> Option<String> {
    if matches!(target, ProjectionTarget::Parser(_)) {
        record.source_path.clone()
    } else {
        None
    }
}

fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let (y, mo, d, h, mi, s) = secs_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn secs_to_ymdhms(mut s: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = s % 60;
    s /= 60;
    let min = s % 60;
    s /= 60;
    let hour = s % 24;
    s /= 24;
    let mut days = s;
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = is_leap(year);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &m in &months {
        if days < m {
            break;
        }
        days -= m;
        month += 1;
    }
    (year, month, days + 1, hour, min, sec)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}
