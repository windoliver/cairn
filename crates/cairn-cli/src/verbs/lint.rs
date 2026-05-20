//! `cairn lint` handler.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::config::{CairnConfig, StoreKind};
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::projection::{ProjectionLedgerRow, ProjectionSummary, ProjectionTarget};
use cairn_core::generated::common::Ulid;
use cairn_core::generated::verbs::lint::{
    LintData, LintDataFindings, LintDataFindingsKind, LintDataSummary,
};
use cairn_store_sqlite::SqliteMemoryStore;
use clap::ArgMatches;

use crate::config::{self, CliOverrides};
use crate::nexus::{self, ProjectionStatusState};

use super::envelope::emit_json;
use super::reindex;

fn block_on<F: Future>(future: F) -> Result<F::Output, ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|err| {
            eprintln!("cairn lint: failed to initialize async runtime: {err}");
            ExitCode::FAILURE
        })?;
    Ok(runtime.block_on(future))
}

/// Run `cairn lint`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let fix = sub.get_flag("fix");
    let vault_path = match vault_path() {
        Ok(path) => path,
        Err(code) => return code,
    };
    let config = match active_config(&vault_path) {
        Ok(config) => config,
        Err(code) => return code,
    };
    let data = lint_data(&vault_path, &config, fix);
    if json {
        emit_json(&data);
    } else {
        render_human(&data);
    }
    ExitCode::SUCCESS
}

fn vault_path() -> Result<PathBuf, ExitCode> {
    std::env::current_dir().map_err(|err| {
        eprintln!("cairn lint: failed to resolve current directory: {err}");
        ExitCode::FAILURE
    })
}

fn active_config(vault_path: &Path) -> Result<CairnConfig, ExitCode> {
    config::load(vault_path, &CliOverrides::default()).map_err(|err| {
        eprintln!("cairn lint: {err:#}");
        ExitCode::from(78)
    })
}

fn lint_data(vault_path: &Path, config: &CairnConfig, fix: bool) -> LintData {
    if !matches!(config.store.kind, StoreKind::NexusSandbox) {
        return empty_lint_data();
    }

    let mut findings = Vec::new();
    let mut counts = ProjectionLintCounts::default();
    let status = nexus::evaluate_projection_status(vault_path, config);
    if matches!(status.state, ProjectionStatusState::Degraded) {
        findings.push(sidecar_unavailable_finding(
            "Nexus projection sidecar is unavailable".to_owned(),
        ));
    }

    if fix
        && !matches!(status.state, ProjectionStatusState::Degraded)
        && let Err(err) = reindex::rebuild_from_db(vault_path, config)
    {
        findings.push(sidecar_unavailable_finding(format!(
            "Nexus projection rebuild failed: {err}"
        )));
    }

    for summary in projection_summaries(vault_path) {
        add_summary_findings(&summary, &mut findings, &mut counts);
    }
    add_failure_findings(projection_failures(vault_path), &mut findings);
    lint_data_from_findings(findings, counts.stale, counts.missing, counts.failed)
}

#[derive(Default)]
struct ProjectionLintCounts {
    stale: u64,
    missing: u64,
    failed: u64,
}

fn sidecar_unavailable_finding(message: String) -> LintDataFindings {
    LintDataFindings {
        kind: LintDataFindingsKind::ProjectionSidecarUnavailable,
        message,
        projection_target: Some("bm25s_lexical".to_owned()),
        rebuildable: Some(true),
        record_id: None,
        source_hash: None,
    }
}

fn add_summary_findings(
    summary: &ProjectionSummary,
    findings: &mut Vec<LintDataFindings>,
    counts: &mut ProjectionLintCounts,
) {
    if summary.lagging_items == 0 {
        return;
    }
    let target = projection_target_key(&summary.target);
    if summary.failed_items > 0 {
        counts.failed = counts
            .failed
            .saturating_add(usize_to_u64(summary.failed_items));
        if matches!(summary.target, ProjectionTarget::Bm25sLexical) {
            findings.push(projection_failed_finding(summary, &target));
        }
    }
    if summary.missing_items > 0 {
        counts.missing = counts
            .missing
            .saturating_add(usize_to_u64(summary.missing_items));
        findings.push(projection_missing_finding(summary, &target));
    }
    if summary.stale_items > 0 {
        counts.stale = counts
            .stale
            .saturating_add(usize_to_u64(summary.stale_items));
        findings.push(projection_stale_finding(summary, &target));
    }
}

fn projection_failed_finding(summary: &ProjectionSummary, target: &str) -> LintDataFindings {
    LintDataFindings {
        kind: LintDataFindingsKind::ProjectionFailed,
        message: format!(
            "Projection target {target} has {} failed items (current={}, lagging={}, total={})",
            summary.failed_items,
            summary.current_items,
            summary.lagging_items,
            summary.total_authoritative_items
        ),
        projection_target: Some(target.to_owned()),
        rebuildable: Some(true),
        record_id: None,
        source_hash: None,
    }
}

fn projection_missing_finding(summary: &ProjectionSummary, target: &str) -> LintDataFindings {
    LintDataFindings {
        kind: LintDataFindingsKind::ProjectionMissing,
        message: format!(
            "Projection target {target} has {} missing items (current={}, failed={}, total={})",
            summary.missing_items,
            summary.current_items,
            summary.failed_items,
            summary.total_authoritative_items
        ),
        projection_target: Some(target.to_owned()),
        rebuildable: Some(true),
        record_id: None,
        source_hash: None,
    }
}

fn projection_stale_finding(summary: &ProjectionSummary, target: &str) -> LintDataFindings {
    LintDataFindings {
        kind: LintDataFindingsKind::ProjectionStale,
        message: format!(
            "Projection target {target} has {} stale items (current={}, failed={}, total={})",
            summary.stale_items,
            summary.current_items,
            summary.failed_items,
            summary.total_authoritative_items
        ),
        projection_target: Some(target.to_owned()),
        rebuildable: Some(true),
        record_id: None,
        source_hash: None,
    }
}

fn add_failure_findings(failures: Vec<ProjectionLedgerRow>, findings: &mut Vec<LintDataFindings>) {
    for row in failures {
        if failure_reason(&row).contains("hash mismatch") {
            findings.push(hash_mismatch_finding(row));
            continue;
        }
        if matches!(row.target, ProjectionTarget::Parser(_)) {
            findings.push(parser_failure_finding(row));
        }
    }
}

fn failure_reason(row: &ProjectionLedgerRow) -> String {
    match &row.state {
        cairn_core::domain::projection::ProjectionItemState::Failed { reason } => reason.clone(),
        _ => "projection failed".to_owned(),
    }
}

fn hash_mismatch_finding(row: ProjectionLedgerRow) -> LintDataFindings {
    let target = projection_target_key(&row.target);
    let reason = failure_reason(&row);
    LintDataFindings {
        kind: LintDataFindingsKind::ProjectionHashMismatch,
        message: format!("Projection target {target} rejected sidecar result: {reason}"),
        projection_target: Some(target),
        rebuildable: Some(true),
        record_id: Some(Ulid(row.cursor.record_id.as_str().to_owned())),
        source_hash: row.cursor.source_hash,
    }
}

fn parser_failure_finding(row: ProjectionLedgerRow) -> LintDataFindings {
    let target = projection_target_key(&row.target);
    let reason = failure_reason(&row);
    LintDataFindings {
        kind: LintDataFindingsKind::ProjectionParserFailed,
        message: format!("Parser projection {target} failed: {reason}"),
        projection_target: Some(target),
        rebuildable: Some(true),
        record_id: Some(Ulid(row.cursor.record_id.as_str().to_owned())),
        source_hash: row.cursor.source_hash,
    }
}

fn projection_summaries(vault_path: &Path) -> Vec<ProjectionSummary> {
    let db_path = vault_path.join(".cairn/cairn.db");
    if !db_path.exists() {
        return Vec::new();
    }
    let Ok(store) = SqliteMemoryStore::open(&db_path) else {
        return Vec::new();
    };
    match block_on(store.projection_summaries()) {
        Ok(Ok(summaries)) => summaries,
        _ => Vec::new(),
    }
}

fn projection_failures(vault_path: &Path) -> Vec<ProjectionLedgerRow> {
    let db_path = vault_path.join(".cairn/cairn.db");
    if !db_path.exists() {
        return Vec::new();
    }
    let Ok(store) = SqliteMemoryStore::open(&db_path) else {
        return Vec::new();
    };
    match block_on(store.projection_failures()) {
        Ok(Ok(failures)) => failures,
        _ => Vec::new(),
    }
}

fn projection_target_key(target: &ProjectionTarget) -> String {
    match target {
        ProjectionTarget::Bm25sLexical => "bm25s_lexical".to_owned(),
        _ => target.as_key(),
    }
}

fn lint_data_from_findings(
    findings: Vec<LintDataFindings>,
    projection_stale: u64,
    projection_missing: u64,
    projection_failed: u64,
) -> LintData {
    LintData {
        summary: LintDataSummary {
            contradictions: None,
            orphans: None,
            projection_failed: nonzero(projection_failed),
            projection_missing: nonzero(projection_missing),
            projection_stale: nonzero(projection_stale),
            stale: None,
            total: u64::try_from(findings.len()).unwrap_or(u64::MAX),
        },
        findings,
        report_path: None,
    }
}

fn empty_lint_data() -> LintData {
    lint_data_from_findings(Vec::new(), 0, 0, 0)
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn nonzero(value: u64) -> Option<u64> {
    (value > 0).then_some(value)
}

fn render_human(data: &LintData) {
    println!("lint_findings: {}", data.summary.total);
    for finding in &data.findings {
        if let Some(target) = &finding.projection_target {
            println!("  {:?}: {target}: {}", finding.kind, finding.message);
        } else {
            println!("  {:?}: {}", finding.kind, finding.message);
        }
    }
}
