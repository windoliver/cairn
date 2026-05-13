//! `cairn assemble_hot` handler.

use std::process::ExitCode;

use cairn_core::contract::memory_store::HotMemoryRequest;
use cairn_core::generated::common::Ulid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::assemble_hot::{
    AssembleHotData, CacheInfo, HotCacheStatus, HotSourceKind, SourceSummary, TruncationDecision,
    TruncationDecisionReason,
};
use cairn_core::hot_memory::{
    HotMemoryCacheStatus, HotMemoryOutput, HotMemorySourceKind, HotMemoryTruncationReason,
    assemble_hot_with_store,
};
use cairn_store_sqlite::SqliteMemoryStore;
use clap::ArgMatches;

use crate::config::{self, CliOverrides};

use super::envelope::{emit_json, human_error, new_operation_id};

/// Run `cairn assemble_hot`.
#[must_use]
pub fn run(sub: &ArgMatches) -> ExitCode {
    let json = sub.get_flag("json");
    match assemble_hot_response(sub) {
        Ok(resp) => {
            if json {
                emit_json(&resp);
            } else if let Some(ResponseData::AssembleHot(data)) = resp.data.as_ref() {
                println!(
                    "cairn assemble_hot: committed {} bytes from {} source buckets (cache: {:?})",
                    data.bytes,
                    data.sources.len(),
                    data.cache.status
                );
                print!("{}", data.prefix);
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            let resp = internal_response(&message);
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "assemble_hot",
                    "Internal",
                    resp.error
                        .as_ref()
                        .and_then(|err| err.get("message"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("assemble_hot failed"),
                    &resp.operation_id,
                );
            }
            ExitCode::FAILURE
        }
    }
}

fn assemble_hot_response(sub: &ArgMatches) -> Result<Response, String> {
    let vault_path = std::env::current_dir().map_err(|e| format!("reading current dir: {e}"))?;
    let config = config::load(&vault_path, &CliOverrides::default())
        .map_err(|e| format!("loading config: {e:#}"))?;
    let store =
        SqliteMemoryStore::open(&vault_path).map_err(|e| format!("opening sqlite store: {e}"))?;
    let budget = sub
        .get_one::<u32>("budget")
        .copied()
        .unwrap_or(config.vault.hot_memory.max_bytes);
    let config_fingerprint = serde_json::to_string(&config.vault.hot_memory)
        .map_err(|e| format!("serializing hot-memory config fingerprint: {e}"))?;
    let request = HotMemoryRequest {
        session_id: sub.get_one::<String>("session_id").cloned(),
        agent_id: None,
        budget_bytes: budget,
        config_fingerprint,
        god_node_weight: config.vault.hot_memory.god_node_weight,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("building tokio runtime: {e}"))?;
    let output = runtime
        .block_on(assemble_hot_with_store(&store, &request))
        .map_err(|e| format!("assembling hot memory: {e}"))?;

    Ok(Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(ResponseData::AssembleHot(to_assemble_hot_data(output))),
        error: None,
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Committed,
        target: None,
        verb: ResponseVerb::AssembleHot,
    })
}

fn internal_response(message: &str) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: None,
        error: Some(serde_json::json!({
            "code": "Internal",
            "message": message,
        })),
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Aborted,
        target: None,
        verb: ResponseVerb::AssembleHot,
    }
}

fn to_assemble_hot_data(output: HotMemoryOutput) -> AssembleHotData {
    let cache_status = map_cache_status(&output.cache.status);
    AssembleHotData {
        bytes: u64::from(output.bytes),
        cache: CacheInfo {
            key: output.cache.key,
            status: cache_status,
        },
        prefix: output.prefix,
        sources: output
            .sources
            .into_iter()
            .map(|source| SourceSummary {
                attempted: u64::from(source.attempted),
                bytes: u64::from(source.bytes),
                included: u64::from(source.included),
                kind: map_source_kind(source.kind),
                omitted: u64::from(source.omitted),
            })
            .collect(),
        truncation: output
            .truncation
            .into_iter()
            .map(|decision| TruncationDecision {
                attempted_bytes: u64::from(decision.attempted_bytes),
                included_bytes: u64::from(decision.included_bytes),
                kind: map_source_kind(decision.kind),
                reason: map_truncation_reason(decision.reason),
                record_id: decision.record_id.map(Ulid),
            })
            .collect(),
    }
}

fn map_source_kind(kind: HotMemorySourceKind) -> HotSourceKind {
    match kind {
        HotMemorySourceKind::Purpose => HotSourceKind::Purpose,
        HotMemorySourceKind::Profile => HotSourceKind::Profile,
        HotMemorySourceKind::Pinned => HotSourceKind::Pinned,
        HotMemorySourceKind::HighSalience => HotSourceKind::HighSalience,
        HotMemorySourceKind::ProjectState => HotSourceKind::ProjectState,
        HotMemorySourceKind::RollingSummary => HotSourceKind::RollingSummary,
        HotMemorySourceKind::Playbook => HotSourceKind::Playbook,
        HotMemorySourceKind::RecentUserSignal => HotSourceKind::RecentUserSignal,
    }
}

fn map_cache_status(status: &HotMemoryCacheStatus) -> HotCacheStatus {
    match status {
        HotMemoryCacheStatus::Hit => HotCacheStatus::Hit,
        HotMemoryCacheStatus::Miss => HotCacheStatus::Miss,
        HotMemoryCacheStatus::Refreshed => HotCacheStatus::Refreshed,
    }
}

fn map_truncation_reason(reason: HotMemoryTruncationReason) -> TruncationDecisionReason {
    match reason {
        HotMemoryTruncationReason::BudgetExhausted => TruncationDecisionReason::BudgetExhausted,
        HotMemoryTruncationReason::SectionTruncated => TruncationDecisionReason::SectionTruncated,
        HotMemoryTruncationReason::RecordOmitted => TruncationDecisionReason::RecordOmitted,
    }
}
