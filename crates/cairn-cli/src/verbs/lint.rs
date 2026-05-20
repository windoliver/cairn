//! `cairn lint` handler.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cairn_core::config::{CairnConfig, StoreKind};
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::projection::{ProjectionSummary, ProjectionTarget};
use cairn_core::generated::verbs::lint::{
    LintData, LintDataFindings, LintDataFindingsKind, LintDataSummary,
};
use cairn_store_sqlite::SqliteMemoryStore;
use clap::ArgMatches;

use crate::config::{self, CliOverrides};
use crate::nexus::{self, ProjectionStatusState};

use super::envelope::emit_json;

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

fn lint_data(vault_path: &Path, config: &CairnConfig, _fix: bool) -> LintData {
    if !matches!(config.store.kind, StoreKind::NexusSandbox) {
        return empty_lint_data();
    }

    let mut findings = Vec::new();
    let status = nexus::evaluate_projection_status(vault_path, config);
    if matches!(status.state, ProjectionStatusState::Degraded) {
        findings.push(LintDataFindings {
            kind: LintDataFindingsKind::ProjectionSidecarUnavailable,
            message: "Nexus projection sidecar is unavailable".to_owned(),
            projection_target: Some("bm25s_lexical".to_owned()),
            rebuildable: Some(true),
            record_id: None,
            source_hash: None,
        });
    }

    for summary in projection_summaries(vault_path) {
        if summary.lagging_items == 0 {
            continue;
        }
        let target = projection_target_key(&summary.target);
        findings.push(LintDataFindings {
            kind: LintDataFindingsKind::ProjectionStale,
            message: format!(
                "Projection target {target} has {} lagging items (current={}, failed={}, total={})",
                summary.lagging_items,
                summary.current_items,
                summary.failed_items,
                summary.total_authoritative_items
            ),
            projection_target: Some(target),
            rebuildable: Some(true),
            record_id: None,
            source_hash: None,
        });
    }

    lint_data_from_findings(findings)
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

fn projection_target_key(target: &ProjectionTarget) -> String {
    match target {
        ProjectionTarget::Bm25sLexical => "bm25s_lexical".to_owned(),
        _ => target.as_key(),
    }
}

fn lint_data_from_findings(findings: Vec<LintDataFindings>) -> LintData {
    let projection_stale = count_kind(&findings, LintDataFindingsKind::ProjectionStale);
    LintData {
        summary: LintDataSummary {
            contradictions: None,
            orphans: None,
            projection_failed: None,
            projection_missing: None,
            projection_stale: nonzero(projection_stale),
            stale: None,
            total: u64::try_from(findings.len()).unwrap_or(u64::MAX),
        },
        findings,
        report_path: None,
    }
}

fn empty_lint_data() -> LintData {
    lint_data_from_findings(Vec::new())
}

fn count_kind(findings: &[LintDataFindings], kind: LintDataFindingsKind) -> u64 {
    u64::try_from(
        findings
            .iter()
            .filter(|finding| finding.kind == kind)
            .count(),
    )
    .unwrap_or(u64::MAX)
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
