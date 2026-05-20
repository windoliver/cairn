//! `cairn search` handler.

use std::future::Future;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use cairn_core::config::{CairnConfig, StoreKind};
use cairn_core::contract::memory_store::{
    Bm25sPreference, MemoryStore, MemoryStoreError, RankingSignal, RankingSignalName, SearchHit,
    SearchMode, SearchRequest, SearchResponse,
};
use cairn_core::domain::projection::ProjectionTarget;
use cairn_core::generated::common::Ulid;
use cairn_core::generated::verbs::search::{
    Hit, HitRankingSignals, HitRankingSignalsName, HitTrust, SearchArgs, SearchArgsMode,
    SearchArgsRanking, SearchArgsRankingBm25s, SearchData,
};
use cairn_store_sqlite::SqliteMemoryStore;
use clap::ArgMatches;

use crate::config::{self, CliOverrides};
use crate::nexus::projection::{
    ProjectionClient, ProjectionSearchCandidate, ProjectionSearchRequest,
};

use super::envelope::{emit_json, new_operation_id};

const DEFAULT_LIMIT: u32 = 10;

fn block_on<F: Future>(future: F) -> Result<F::Output, ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|err| {
            eprintln!("cairn search: failed to initialize async runtime: {err}");
            ExitCode::FAILURE
        })?;
    Ok(runtime.block_on(future))
}

/// Run `cairn search`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    let (args, limit) = match search_args(sub) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let bm25s = bm25s(args.ranking.as_ref().and_then(|ranking| ranking.bm25s));

    let vault_path = match vault_path() {
        Ok(path) => path,
        Err(code) => return code,
    };
    let active = match active_config(&vault_path) {
        Ok(config) => config,
        Err(code) => return code,
    };
    if let Err(code) = require_bm25s_available(&active, bm25s) {
        return code;
    }

    let data = match search_sqlite(&vault_path, &active, &args, limit, bm25s) {
        Ok(data) => data,
        Err(code) => return code,
    };
    if json {
        emit_json(&data);
    } else {
        for hit in &data.hits {
            println!(
                "{}\t{}\t{}",
                hit.score,
                hit.record_id.0,
                hit.snippet.as_deref().unwrap_or("")
            );
        }
    }
    ExitCode::SUCCESS
}

fn search_args(sub: &ArgMatches) -> Result<(SearchArgs, u32), ExitCode> {
    let Some(query) = sub.get_one::<String>("query").cloned() else {
        eprintln!("cairn search: InvalidArgs — query is required");
        return Err(ExitCode::from(64));
    };
    if query.is_empty() {
        eprintln!("cairn search: InvalidArgs — query must not be empty");
        return Err(ExitCode::from(64));
    }

    let args_mode = parse_mode(sub.get_one::<String>("mode").map(String::as_str));
    let args_bm25s = parse_bm25s(sub.get_one::<String>("bm25s").map(String::as_str));
    let limit = sub
        .get_one::<u32>("limit")
        .copied()
        .unwrap_or(DEFAULT_LIMIT);
    if !(1..=1000).contains(&limit) {
        eprintln!("cairn search: InvalidArgs — limit must be in [1, 1000]");
        return Err(ExitCode::from(64));
    }

    Ok((
        SearchArgs {
            citations: None,
            cursor: None,
            filters: None,
            limit: Some(i64::from(limit)),
            mode: args_mode,
            query,
            ranking: Some(SearchArgsRanking {
                bm25s: Some(args_bm25s),
            }),
            scope: None,
        },
        limit,
    ))
}

fn vault_path() -> Result<std::path::PathBuf, ExitCode> {
    std::env::current_dir().map_err(|err| {
        eprintln!("cairn search: failed to resolve current directory: {err}");
        ExitCode::FAILURE
    })
}

fn active_config(vault_path: &Path) -> Result<CairnConfig, ExitCode> {
    config::load(vault_path, &CliOverrides::default()).map_err(|err| {
        eprintln!("cairn search: {err:#}");
        ExitCode::from(78)
    })
}

fn require_bm25s_available(active: &CairnConfig, bm25s: Bm25sPreference) -> Result<(), ExitCode> {
    if matches!(bm25s, Bm25sPreference::Required)
        && !matches!(active.store.kind, StoreKind::NexusSandbox)
    {
        eprintln!(
            "cairn search: CapabilityUnavailable — bm25s ranking requires store.kind: nexus-sandbox"
        );
        return Err(ExitCode::from(69));
    }
    Ok(())
}

fn search_sqlite(
    vault_path: &Path,
    active: &CairnConfig,
    args: &SearchArgs,
    limit: u32,
    bm25s: Bm25sPreference,
) -> Result<SearchData, ExitCode> {
    let db_path = vault_path.join(".cairn/cairn.db");
    let store = match SqliteMemoryStore::open(&db_path) {
        Ok(store) => store,
        Err(err) => {
            eprintln!(
                "cairn search: Store — failed to open {}: {err}",
                db_path.display()
            );
            return Err(ExitCode::from(74));
        }
    };
    let response = match block_on(store.search(SearchRequest {
        query: args.query.clone(),
        mode: mode(args.mode),
        limit,
        bm25s: Bm25sPreference::Disabled,
    }))? {
        Ok(response) => response,
        Err(MemoryStoreError::CapabilityUnavailable(capability)) => {
            eprintln!("cairn search: CapabilityUnavailable — {capability}");
            return Err(ExitCode::from(69));
        }
        Err(MemoryStoreError::Store(err)) => {
            eprintln!("cairn search: Store — {err}");
            return Err(ExitCode::from(74));
        }
        Err(err) => {
            eprintln!("cairn search: Store — {err}");
            return Err(ExitCode::from(74));
        }
    };
    let mut response = response;
    if !bm25s_projection_current(active, &store, bm25s, &mut response)? {
        return Ok(search_data(response));
    }
    let response = apply_bm25s(active, &args.query, limit, bm25s, response)?;
    Ok(search_data(response))
}

fn search_data(response: SearchResponse) -> SearchData {
    SearchData {
        hits: response
            .hits
            .into_iter()
            .map(|hit| Hit {
                citation: None,
                ranking_signals: hit
                    .ranking_signals
                    .into_iter()
                    .map(|signal| HitRankingSignals {
                        name: signal_name(signal.name),
                        reason: signal.reason,
                        score: signal.score,
                        used: signal.used,
                    })
                    .collect(),
                record_id: Ulid(hit.record_id.as_str().to_owned()),
                score: hit.score,
                snippet: hit.snippet,
                trust: HitTrust::Unknown,
            })
            .collect(),
        next_cursor: None,
    }
}

fn apply_bm25s(
    active: &CairnConfig,
    query: &str,
    limit: u32,
    bm25s: Bm25sPreference,
    mut response: SearchResponse,
) -> Result<SearchResponse, ExitCode> {
    if matches!(bm25s, Bm25sPreference::Disabled) || response.hits.is_empty() {
        return Ok(response);
    }
    if !matches!(active.store.kind, StoreKind::NexusSandbox) {
        return Ok(response);
    }
    let client = ProjectionClient::new(
        active.store.nexus.endpoint.clone(),
        "/projection/apply".to_owned(),
        Duration::from_millis(active.store.nexus.health_timeout_ms),
    );
    let request = ProjectionSearchRequest {
        operation_id: new_operation_id().0,
        query: query.to_owned(),
        candidates: response
            .hits
            .iter()
            .map(|hit| ProjectionSearchCandidate {
                record_id: hit.record_id.as_str().to_owned(),
                record_hash: hit.record_hash.clone(),
            })
            .collect(),
        limit,
    };
    let bm25s_response = match client.search(&request) {
        Ok(response) => response,
        Err(err) if matches!(bm25s, Bm25sPreference::Required) => {
            eprintln!("cairn search: CapabilityUnavailable — {err}");
            return Err(ExitCode::from(69));
        }
        Err(err) => {
            mark_bm25s_skipped(&mut response.hits, &err);
            return Ok(response);
        }
    };
    let mut by_record = std::collections::HashMap::new();
    for hit in bm25s_response.hits {
        by_record.insert(hit.record_id.clone(), hit);
    }
    let mut missing_required = false;
    for hit in &mut response.hits {
        match by_record.remove(hit.record_id.as_str()) {
            Some(bm25s_hit) if bm25s_hit.record_hash == hit.record_hash => {
                hit.score += bm25s_hit.score;
                hit.ranking_signals.push(RankingSignal {
                    name: RankingSignalName::NexusBm25s,
                    used: true,
                    score: Some(bm25s_hit.score),
                    reason: bm25s_hit.reason,
                });
            }
            Some(_) => {
                missing_required = true;
                hit.ranking_signals.push(RankingSignal {
                    name: RankingSignalName::NexusBm25s,
                    used: false,
                    score: None,
                    reason: Some("nexus bm25s hash mismatch".to_owned()),
                });
            }
            None => {
                missing_required = true;
                hit.ranking_signals.push(RankingSignal {
                    name: RankingSignalName::NexusBm25s,
                    used: false,
                    score: None,
                    reason: Some("nexus bm25s missing".to_owned()),
                });
            }
        }
    }
    if matches!(bm25s, Bm25sPreference::Required) && missing_required {
        eprintln!("cairn search: CapabilityUnavailable — nexus bm25s ranking incomplete");
        return Err(ExitCode::from(69));
    }
    response.hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(response)
}

fn bm25s_projection_current(
    active: &CairnConfig,
    store: &SqliteMemoryStore,
    bm25s: Bm25sPreference,
    response: &mut SearchResponse,
) -> Result<bool, ExitCode> {
    if matches!(bm25s, Bm25sPreference::Disabled) || response.hits.is_empty() {
        return Ok(false);
    }
    if !matches!(active.store.kind, StoreKind::NexusSandbox) {
        return Ok(false);
    }
    let ready = match block_on(store.projection_summaries())? {
        Ok(summaries) => summaries.iter().any(|summary| {
            summary.target == ProjectionTarget::Bm25sLexical
                && summary.failed_items == 0
                && summary.lagging_items == 0
                && summary.current_items >= response.hits.len()
        }),
        Err(err) => {
            if matches!(bm25s, Bm25sPreference::Required) {
                eprintln!("cairn search: CapabilityUnavailable — projection status: {err}");
                return Err(ExitCode::from(69));
            }
            mark_bm25s_skipped(
                response.hits.as_mut_slice(),
                &format!("projection status: {err}"),
            );
            return Ok(false);
        }
    };
    if ready {
        return Ok(true);
    }
    let reason = "nexus bm25s projection not current";
    if matches!(bm25s, Bm25sPreference::Required) {
        eprintln!("cairn search: CapabilityUnavailable — {reason}");
        return Err(ExitCode::from(69));
    }
    mark_bm25s_skipped(response.hits.as_mut_slice(), reason);
    Ok(false)
}

fn mark_bm25s_skipped(hits: &mut [SearchHit], reason: &str) {
    for hit in hits {
        hit.ranking_signals.push(RankingSignal {
            name: RankingSignalName::NexusBm25s,
            used: false,
            score: None,
            reason: Some(reason.to_owned()),
        });
    }
}

fn parse_mode(raw: Option<&str>) -> SearchArgsMode {
    match raw {
        Some("semantic") => SearchArgsMode::Semantic,
        Some("hybrid") => SearchArgsMode::Hybrid,
        _ => SearchArgsMode::Keyword,
    }
}

fn parse_bm25s(raw: Option<&str>) -> SearchArgsRankingBm25s {
    match raw {
        Some("required") => SearchArgsRankingBm25s::Required,
        Some("disabled") => SearchArgsRankingBm25s::Disabled,
        _ => SearchArgsRankingBm25s::Auto,
    }
}

fn mode(raw: SearchArgsMode) -> SearchMode {
    match raw {
        SearchArgsMode::Semantic => SearchMode::Semantic,
        SearchArgsMode::Hybrid => SearchMode::Hybrid,
        _ => SearchMode::Keyword,
    }
}

fn bm25s(raw: Option<SearchArgsRankingBm25s>) -> Bm25sPreference {
    match raw {
        Some(SearchArgsRankingBm25s::Required) => Bm25sPreference::Required,
        Some(SearchArgsRankingBm25s::Disabled) => Bm25sPreference::Disabled,
        _ => Bm25sPreference::Auto,
    }
}

fn signal_name(name: RankingSignalName) -> HitRankingSignalsName {
    match name {
        RankingSignalName::SqliteVec => HitRankingSignalsName::SqliteVec,
        RankingSignalName::NexusBm25s => HitRankingSignalsName::NexusBm25s,
        _ => HitRankingSignalsName::SqliteFts5,
    }
}
