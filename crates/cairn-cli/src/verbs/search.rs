//! `cairn search` handler.

use std::future::Future;
use std::path::Path;
use std::process::ExitCode;

use cairn_core::config::{CairnConfig, StoreKind};
use cairn_core::contract::memory_store::{
    Bm25sPreference, MemoryStore, MemoryStoreError, RankingSignalName, SearchMode, SearchRequest,
    SearchResponse,
};
use cairn_core::generated::common::Ulid;
use cairn_core::generated::verbs::search::{
    Hit, HitRankingSignals, HitRankingSignalsName, HitTrust, SearchArgs, SearchArgsMode,
    SearchArgsRanking, SearchArgsRankingBm25s, SearchData,
};
use cairn_store_sqlite::SqliteMemoryStore;
use clap::ArgMatches;

use crate::config::{self, CliOverrides};

use super::envelope::emit_json;

const DEFAULT_LIMIT: u32 = 10;
const DIAGNOSTIC_RECORD_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

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

    let data = match search_sqlite(&vault_path, &args, limit, bm25s) {
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
        bm25s,
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
    Ok(search_data(response))
}

fn search_data(response: SearchResponse) -> SearchData {
    SearchData {
        hits: if response.hits.is_empty() {
            vec![diagnostic_hit()]
        } else {
            response
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
                .collect()
        },
        next_cursor: None,
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

fn diagnostic_hit() -> Hit {
    Hit {
        citation: None,
        ranking_signals: vec![HitRankingSignals {
            name: HitRankingSignalsName::SqliteFts5,
            reason: Some("no sqlite fts hits".to_owned()),
            score: None,
            used: false,
        }],
        record_id: Ulid(DIAGNOSTIC_RECORD_ID.to_owned()),
        score: 0.0,
        snippet: None,
        trust: HitTrust::Unknown,
    }
}
