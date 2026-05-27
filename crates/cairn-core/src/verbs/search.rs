//! `search` verb dispatcher.
//!
//! Single entry point used by every surface (CLI, SDK, MCP). Performs
//! capability gating, dispatches to the matching `MemoryStore` leg
//! (`search_keyword` / `search_semantic` / `search_hybrid`), applies
//! token-budget trimming, and packages the result + optional explain
//! block into the response envelope.
//!
//! No I/O beyond the `store.*` calls — keeps `cairn-core`'s adapter-free
//! invariant (CLAUDE.md §3).

use crate::config::{CairnConfig, CapabilitySet};
use crate::contract::memory_store::{
    HybridSearchArgs, KeywordCursor, KeywordSearchArgs, MemoryStore, SearchCandidate,
    SemanticSearchArgs,
};
use crate::domain::filter::{ValidatedFilter, validate_filter};
use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::{MemoryKind, MemoryVisibility};
use crate::domain::{BodyHash, ScopeTuple};
use crate::generated::verbs::search::SearchArgsFilters;
use crate::pipeline::explain::{Candidate as ExplainCandidate, ExplainConfig, explain_filter};
use crate::policy_trace::{
    PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry, RecordExclusion,
};
use crate::rebac::{RebacAction, RebacContext, RebacDecision, all_visibilities};
use crate::search::{ScoreExplain, token_budget_trim};

/// Mode requested by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SearchMode {
    /// FTS5 keyword leg only.
    Keyword,
    /// ANN vector leg only.
    Semantic,
    /// RRF fusion + cosine re-rank.
    Hybrid,
}

/// Inputs to [`run`].
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// Free-text query.
    pub query: String,
    /// Selected mode.
    pub mode: SearchMode,
    /// Page size.
    pub limit: usize,
    /// Whether reasoning-bearing records should be returned.
    pub include_reasoning: bool,
    /// Visibility allowlist; empty = no narrowing.
    pub visibility_allowlist: Vec<MemoryVisibility>,
    /// Authorization scope tuple. Threaded into every search SQL path
    /// (keyword, semantic, graph, graph-only hydration). Issue #191.
    /// Use [`ScopeTuple::default`] when no narrowing is required.
    pub auth_scope: ScopeTuple,
    /// `ReBAC` context for shared-tier read access. Empty context fails closed
    /// for `project`, `team`, `org`, and `public`, while local tiers remain
    /// available.
    pub rebac: RebacContext,
    /// Active embedding model label (for semantic + hybrid).
    pub model_label: String,
    /// Optional recall-narrowing metadata filter.
    pub filter: Option<SearchArgsFilters>,
    /// `true` → request explain block from the store and surface it.
    /// Caller must have already verified the `policy_trace` capability.
    pub explain: bool,
}

/// Result of [`run`]: the trimmed candidate page plus optional explain.
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    /// Trimmed candidate page.
    pub candidates: Vec<SearchCandidate>,
    /// Per-candidate score-component explanations, in lockstep with
    /// `candidates`. Populated iff `request.explain` was true and the
    /// `policy_trace` capability was advertised.
    pub explain: Option<Vec<ScoreExplain>>,
    /// Policy gates evaluated for the read.
    pub policy_trace: Vec<PolicyTraceEntry>,
    /// Tier-2 read-filter exclusions, populated only when `request.explain`
    /// is true.
    pub excluded: Option<Vec<crate::policy_trace::RecordExclusion>>,
    /// Hybrid-only: legs that ran in degraded mode (capability missing,
    /// SQL error, deadline, etc.). Empty for keyword/semantic and for
    /// fully successful hybrid runs. Surface code (CLI, MCP, SDK) may
    /// thread this through to operators or callers; v1 verb dispatch
    /// surfaces it here so partial-result signaling is not silently
    /// dropped between the store and the wire surface. Issue #191.
    pub degraded_legs: Vec<crate::search::DegradedLeg>,
    /// True when the response succeeded after a transient semantic
    /// embedding-provider outage. This is derived from `degraded_legs` and is
    /// never set for fail-closed capability errors.
    pub semantic_degraded: bool,
}

/// Search outcome plus optional per-explain-entry skill graph metadata.
#[derive(Debug, Clone)]
pub struct SearchOutcomeWithSkillGraph {
    /// Standard search outcome.
    pub outcome: SearchOutcome,
    /// Optional graph explain payloads in lockstep with `outcome.explain`.
    pub skill_graph: Option<Vec<Option<crate::pipeline::skillify::SkillGraphExplain>>>,
}

fn candidate_has_reasoning(candidate: &SearchCandidate) -> bool {
    // Fail closed: if the row's `record_json` won't parse, treat it as
    // potentially reasoning-bearing and exclude it from default results.
    // The privacy boundary must not silently lower itself on store
    // corruption or version skew.
    let Ok(record_json) = serde_json::from_str::<serde_json::Value>(&candidate.record_json) else {
        return true;
    };
    if record_json
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind == MemoryKind::Reasoning.as_str())
    {
        return true;
    }
    let Some(trace_blocks) = record_json
        .get("extra_frontmatter")
        .and_then(|value| value.get("trace_blocks"))
    else {
        return false;
    };
    // Same fail-closed rule for a non-array `trace_blocks`: schema skew
    // means we cannot prove the row is reasoning-free.
    let Some(blocks) = trace_blocks.as_array() else {
        return true;
    };
    blocks.iter().any(|block| {
        block
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("reasoning"))
    })
}

fn filter_reasoning_candidates(
    candidates: Vec<SearchCandidate>,
    explain: Option<Vec<ScoreExplain>>,
    include_reasoning: bool,
) -> (Vec<SearchCandidate>, Option<Vec<ScoreExplain>>) {
    if include_reasoning {
        return (candidates, explain);
    }

    let mut kept_candidates = Vec::with_capacity(candidates.len());
    let mut kept_explain = explain
        .as_ref()
        .map(|entries| Vec::with_capacity(entries.len()));

    for (index, candidate) in candidates.into_iter().enumerate() {
        if candidate_has_reasoning(&candidate) {
            continue;
        }
        kept_candidates.push(candidate);
        if let (Some(entries), Some(kept)) = (explain.as_ref(), kept_explain.as_mut())
            && let Some(entry) = entries.get(index)
        {
            kept.push(entry.clone());
        }
    }

    (kept_candidates, kept_explain)
}

/// Errors raised by the dispatcher.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SearchError {
    /// Required capability is not advertised by `status` in this incarnation.
    #[error("capability unavailable: {capability}")]
    CapabilityUnavailable {
        /// The capability identifier (e.g. `cairn.mcp.v1.search.hybrid`).
        capability: &'static str,
    },
    /// Args failed validation before dispatch.
    #[error("invalid args: {reason}")]
    InvalidArgs {
        /// Human-readable reason.
        reason: String,
    },
    /// Filter JSON parsed but failed the allowlist/operator validator.
    #[error("invalid filter: {reason}")]
    InvalidFilter {
        /// Human-readable reason.
        reason: String,
    },
    /// Store impl raised an error.
    ///
    /// The wrapped `StoreError` is `Box<dyn Error + Send + Sync>` (see
    /// `crate::contract::memory_store::StoreError`); surface code should
    /// map this variant to a generic envelope `Internal`/sysexit 70 until
    /// per-adapter typed errors land in #62. Future change: either add a
    /// `kind: &'static str` discriminator or replace `StoreError` with a
    /// typed enum.
    #[error(transparent)]
    Store(#[from] crate::contract::memory_store::StoreError),
}

const POLICY_TRACE_CAP: &str = "cairn.mcp.v1.policy_trace";
const EXPLAIN_FILTER_STALENESS_THRESHOLD_DAYS: u32 = 30;
const READ_FILTER_OVERFETCH_FACTOR: usize = 4;
const READ_FILTER_OVERFETCH_MIN_EXTRA: usize = 8;
const READ_FILTER_OVERFETCH_MAX: usize = 1_000;
const SECONDS_PER_DAY: i64 = 86_400;

/// Fail-closed capability gate for `request.mode`.
fn gate_mode(mode: SearchMode, caps: &CapabilitySet) -> Result<(), SearchError> {
    let (ok, name) = match mode {
        SearchMode::Keyword => (caps.keyword_search, "cairn.mcp.v1.search.keyword"),
        SearchMode::Semantic => (caps.semantic_search, "cairn.mcp.v1.search.semantic"),
        SearchMode::Hybrid => (caps.hybrid_search, "cairn.mcp.v1.search.hybrid"),
    };
    if ok {
        Ok(())
    } else {
        Err(SearchError::CapabilityUnavailable { capability: name })
    }
}

/// Run the dispatcher.
///
/// Order of operations:
/// 1. Mode capability gate (fail closed).
/// 2. `--explain` capability gate (`policy_trace`).
/// 3. Validate the optional recall-narrowing filter.
/// 4. Build mode-specific `*SearchArgs` with `with_explain` set.
/// 5. Dispatch to `store.search_*`.
/// 6. Apply Tier-2 read filters, preserving per-record exclusions for
///    `--explain`.
/// 7. Trim candidates + explain in lockstep using
///    `config.search.max_snippet_chars_per_page`.
///
/// # Errors
///
/// - [`SearchError::CapabilityUnavailable`] for missing mode or
///   `policy_trace` capability.
/// - [`SearchError::InvalidArgs`] when the query is empty.
/// - [`SearchError::InvalidFilter`] when `request.filter` fails validation.
/// - [`SearchError::Store`] propagated from the store impl.
pub async fn run(
    store: &dyn MemoryStore,
    config: &CairnConfig,
    caps: &CapabilitySet,
    request: SearchRequest,
) -> Result<SearchOutcome, SearchError> {
    run_inner(store, config, caps, request).await
}

/// Run search with optional adapter-supplied skill graph metadata for explain output.
///
/// This keeps [`SearchRequest`] source-compatible for existing callers while
/// letting file-system-aware adapters enrich `--explain` responses.
pub async fn run_with_skill_graph_snapshot(
    store: &dyn MemoryStore,
    config: &CairnConfig,
    caps: &CapabilitySet,
    request: SearchRequest,
    skill_graph_snapshot: Option<&crate::pipeline::skillify::SkillLintSnapshot>,
) -> Result<SearchOutcomeWithSkillGraph, SearchError> {
    let outcome = run_inner(store, config, caps, request).await?;
    let skill_graph = skill_graph_explain_for_candidates(
        outcome.explain.as_deref(),
        &outcome.candidates,
        skill_graph_snapshot,
    );
    Ok(SearchOutcomeWithSkillGraph {
        outcome,
        skill_graph,
    })
}

async fn run_inner(
    store: &dyn MemoryStore,
    config: &CairnConfig,
    caps: &CapabilitySet,
    request: SearchRequest,
) -> Result<SearchOutcome, SearchError> {
    if request.query.trim().is_empty() {
        return Err(SearchError::InvalidArgs {
            reason: "query is empty".to_owned(),
        });
    }
    gate_mode(request.mode, caps)?;
    if request.explain && !caps.policy_trace {
        return Err(SearchError::CapabilityUnavailable {
            capability: POLICY_TRACE_CAP,
        });
    }
    let validated_filter = request
        .filter
        .as_ref()
        .map(validate_filter)
        .transpose()
        .map_err(|e| SearchError::InvalidFilter {
            reason: e.to_string(),
        })?;

    let (visibility, rebac_decisions) = visibility_allowlist(&request);

    let (candidates, explain, read_filter_exclusions, degraded_legs) = match request.mode {
        SearchMode::Keyword => {
            let (candidates, explain, exclusions) =
                search_keyword_backfilled(store, &request, validated_filter, visibility).await?;
            (candidates, explain, exclusions, Vec::new())
        }
        SearchMode::Semantic => {
            let args = SemanticSearchArgs {
                query: request.query.clone(),
                filter: validated_filter,
                auth_scope: request.auth_scope.clone(),
                visibility_allowlist: visibility,
                limit: read_filter_fetch_limit(request.limit),
                model_label: request.model_label.clone(),
                with_explain: request.explain,
            };
            let page = store.search_semantic(&args).await?;
            let (candidates, explain, exclusions) = apply_read_filter_backfilling(
                request.mode,
                &page.candidates,
                page.explain.as_deref(),
                request.limit,
            );
            (candidates, explain, exclusions, Vec::new())
        }
        SearchMode::Hybrid => {
            let args = HybridSearchArgs {
                query: request.query.clone(),
                filter: validated_filter,
                auth_scope: request.auth_scope.clone(),
                visibility_allowlist: visibility,
                limit: read_filter_fetch_limit(request.limit),
                model_label: request.model_label.clone(),
                blend: config.search.rerank_blend,
                rrf_k: config.search.rrf_k,
                rerank_topk: config.search.rerank_topk,
                with_explain: request.explain,
                confidence_floor: 1e-3,
                graph_confidence_min: config.search.graph_confidence_min,
            };
            let page = store.search_hybrid(&args).await?;
            let (candidates, explain, exclusions) = apply_read_filter_backfilling(
                request.mode,
                &page.candidates,
                page.explain.as_deref(),
                request.limit,
            );
            let degraded_legs = page.degraded_legs;
            (candidates, explain, exclusions, degraded_legs)
        }
    };

    // Apply the reasoning-hiding privacy filter on top of the Tier-2
    // read filter. Default-hidden unless the caller opts in via
    // `--include-reasoning`. See issue #311.
    let (candidates, explain) =
        filter_reasoning_candidates(candidates, explain, request.include_reasoning);

    let policy_trace = search_policy_trace(&read_filter_exclusions, &rebac_decisions);
    let excluded = request.explain.then_some(read_filter_exclusions);

    let (candidates, explain) = token_budget_trim(
        candidates,
        explain,
        config.search.max_snippet_chars_per_page,
    );

    let semantic_degraded = semantic_degraded(&degraded_legs);

    Ok(SearchOutcome {
        candidates,
        explain,
        policy_trace,
        excluded,
        degraded_legs,
        semantic_degraded,
    })
}

fn semantic_degraded(degraded_legs: &[crate::search::DegradedLeg]) -> bool {
    degraded_legs.iter().any(|leg| {
        matches!(
            leg,
            crate::search::DegradedLeg::Semantic {
                reason: crate::search::DegradationReason::TransientProviderOutage
            }
        )
    })
}

fn skill_graph_explain_for_candidates(
    explain: Option<&[ScoreExplain]>,
    candidates: &[SearchCandidate],
    snapshot: Option<&crate::pipeline::skillify::SkillLintSnapshot>,
) -> Option<Vec<Option<crate::pipeline::skillify::SkillGraphExplain>>> {
    let explain = explain?;
    let snapshot = snapshot?;

    let resolver = crate::pipeline::skillify::SkillGraphResolver::new(snapshot);
    Some(
        explain
            .iter()
            .enumerate()
            .map(|(idx, _)| {
                let candidate = candidates.get(idx)?;
                let skill_id = candidate_skill_id(candidate, snapshot)?;
                let closure = resolver.resolve_prerequisites(&skill_id);
                Some(crate::pipeline::skillify::SkillGraphExplain {
                    skill_id,
                    prerequisites: closure.prerequisites,
                    diagnostics: closure
                        .issues
                        .into_iter()
                        .map(|issue| issue.message)
                        .collect(),
                })
            })
            .collect(),
    )
}

fn candidate_skill_id(
    candidate: &SearchCandidate,
    snapshot: &crate::pipeline::skillify::SkillLintSnapshot,
) -> Option<String> {
    let record = serde_json::from_str::<MemoryRecord>(&candidate.record_json).ok()?;
    if let Some(skill_id) = record
        .extra_frontmatter
        .get("skill_id")
        .and_then(serde_json::Value::as_str)
        .filter(|skill_id| !skill_id.trim().is_empty())
    {
        return Some(skill_id.to_owned());
    }

    let lane = record
        .extra_frontmatter
        .get("lane")
        .and_then(serde_json::Value::as_str)
        .filter(|lane| !lane.trim().is_empty())?;
    let mut matches = snapshot
        .skills
        .iter()
        .filter(|skill| skill.lane == lane)
        .map(|skill| skill.skill_id.as_str());
    let skill_id = matches.next()?;
    matches.next().is_none().then(|| skill_id.to_owned())
}

fn visibility_allowlist(request: &SearchRequest) -> (Vec<MemoryVisibility>, Vec<RebacDecision>) {
    let requested = if request.visibility_allowlist.is_empty() {
        all_visibilities().to_vec()
    } else {
        request.visibility_allowlist.clone()
    };
    request
        .rebac
        .filter_visibility_allowlist(RebacAction::Read, &request.auth_scope, &requested)
}

fn search_policy_trace(
    read_filter_exclusions: &[crate::policy_trace::RecordExclusion],
    rebac_decisions: &[RebacDecision],
) -> Vec<PolicyTraceEntry> {
    let read_filter_outcome = if read_filter_exclusions.is_empty() {
        PolicyOutcome::Pass
    } else {
        PolicyOutcome::Deny
    };
    let mut trace = vec![
        PolicyTraceEntry::pass(PolicyGate::SearchScope),
        PolicyTraceEntry::pass(PolicyGate::SearchCapability),
    ];
    trace.extend(
        rebac_decisions
            .iter()
            .copied()
            .map(RebacDecision::to_policy_trace_entry),
    );
    trace.push(PolicyTraceEntry::new(
        PolicyGate::SearchReadFilter,
        read_filter_outcome,
        PolicyDetail::None,
    ));
    trace
}

async fn search_keyword_backfilled(
    store: &dyn MemoryStore,
    request: &SearchRequest,
    filter: Option<ValidatedFilter<'_>>,
    visibility: Vec<MemoryVisibility>,
) -> Result<
    (
        Vec<SearchCandidate>,
        Option<Vec<ScoreExplain>>,
        Vec<RecordExclusion>,
    ),
    SearchError,
> {
    let target_limit = request.limit.max(1);
    let mut cursor: Option<KeywordCursor> = None;
    let mut all_candidates = Vec::new();
    let mut all_explain = request.explain.then(Vec::new);

    loop {
        let args = KeywordSearchArgs {
            query: request.query.clone(),
            filter,
            auth_scope: request.auth_scope.clone(),
            visibility_allowlist: visibility.clone(),
            limit: target_limit,
            cursor: cursor.clone(),
            with_explain: request.explain,
        };
        let page = store.search_keyword(&args).await?;
        let next_cursor = page.next_cursor;
        let candidate_offset = all_candidates.len();
        let page_candidate_count = page.candidates.len();

        if all_explain.is_some() {
            if let Some(mut entries) = page.explain {
                for (idx, entry) in entries.iter_mut().enumerate() {
                    entry.bm25_rank = Some(candidate_offset + idx + 1);
                }
                if let Some(acc) = all_explain.as_mut() {
                    acc.append(&mut entries);
                }
            } else {
                all_explain = None;
            }
        }
        all_candidates.extend(page.candidates);

        let (filtered_candidates, filtered_explain, exclusions) = apply_read_filter_backfilling(
            SearchMode::Keyword,
            &all_candidates,
            all_explain.as_deref(),
            target_limit,
        );
        if filtered_candidates.len() >= target_limit
            || next_cursor.is_none()
            || page_candidate_count == 0
        {
            return Ok((filtered_candidates, filtered_explain, exclusions));
        }
        cursor = next_cursor;
    }
}

fn read_filter_fetch_limit(limit: usize) -> usize {
    let base = limit.max(1);
    base.saturating_mul(READ_FILTER_OVERFETCH_FACTOR)
        .max(base.saturating_add(READ_FILTER_OVERFETCH_MIN_EXTRA))
        .min(READ_FILTER_OVERFETCH_MAX)
}

fn cap_search_results(
    mut candidates: Vec<SearchCandidate>,
    mut explain: Option<Vec<ScoreExplain>>,
    limit: usize,
) -> (Vec<SearchCandidate>, Option<Vec<ScoreExplain>>) {
    let cap = limit.max(1);
    if candidates.len() > cap {
        candidates.truncate(cap);
    }
    if let Some(entries) = explain.as_mut()
        && entries.len() > cap
    {
        entries.truncate(cap);
    }
    (candidates, explain)
}

fn apply_read_filter_backfilling(
    mode: SearchMode,
    candidates: &[SearchCandidate],
    explain: Option<&[ScoreExplain]>,
    limit: usize,
) -> (
    Vec<SearchCandidate>,
    Option<Vec<ScoreExplain>>,
    Vec<RecordExclusion>,
) {
    let cap = limit.max(1);
    let total = candidates.len();
    let mut window = cap.min(total);

    loop {
        let window_candidates = candidates.iter().take(window).cloned().collect();
        let window_explain = explain.map(|entries| entries.iter().take(window).cloned().collect());
        let (filtered_candidates, filtered_explain, exclusions) =
            apply_read_filter(mode, window_candidates, window_explain);
        if filtered_candidates.len() >= cap || window == total {
            let (filtered_candidates, filtered_explain) =
                cap_search_results(filtered_candidates, filtered_explain, cap);
            return (filtered_candidates, filtered_explain, exclusions);
        }
        window = total.min(window.saturating_add(cap));
    }
}

fn apply_read_filter(
    mode: SearchMode,
    candidates: Vec<SearchCandidate>,
    explain: Option<Vec<ScoreExplain>>,
) -> (
    Vec<SearchCandidate>,
    Option<Vec<ScoreExplain>>,
    Vec<crate::policy_trace::RecordExclusion>,
) {
    let mut projected = Vec::with_capacity(candidates.len());
    for (idx, candidate) in candidates.iter().enumerate() {
        if let Some(explain_candidate) = explain_candidate(mode, idx, candidate, explain.as_deref())
        {
            projected.push((idx, explain_candidate));
        }
    }

    if projected.is_empty() {
        return (candidates, explain, Vec::new());
    }

    let cfg = ExplainConfig {
        staleness_threshold_days: EXPLAIN_FILTER_STALENESS_THRESHOLD_DAYS,
    };
    let (kept, excluded) = explain_filter(
        projected
            .iter()
            .map(|(_, candidate)| candidate.clone())
            .collect(),
        cfg,
    );
    if excluded.is_empty() {
        return (candidates, explain, excluded);
    }

    let kept_targets = kept
        .iter()
        .map(|candidate| candidate.target_id().to_string())
        .collect::<std::collections::HashSet<_>>();
    let mut keep_by_index = vec![true; candidates.len()];
    for (idx, candidate) in &projected {
        keep_by_index[*idx] = kept_targets.contains(&candidate.target_id().to_string());
    }

    let filtered_candidates = candidates
        .into_iter()
        .enumerate()
        .filter_map(|(idx, candidate)| keep_by_index[idx].then_some(candidate))
        .collect();
    let filtered_explain = explain.map(|entries| {
        entries
            .into_iter()
            .enumerate()
            .filter_map(|(idx, entry)| {
                keep_by_index
                    .get(idx)
                    .copied()
                    .unwrap_or(true)
                    .then_some(entry)
            })
            .collect()
    });

    (filtered_candidates, filtered_explain, excluded)
}

fn explain_candidate(
    mode: SearchMode,
    idx: usize,
    candidate: &SearchCandidate,
    explain: Option<&[ScoreExplain]>,
) -> Option<ExplainCandidate> {
    let record = serde_json::from_str::<MemoryRecord>(&candidate.record_json).ok()?;
    Some(ExplainCandidate::from_scope_filter(
        candidate.target_id.clone(),
        age_days(candidate.staleness_seconds),
        relevance_score(mode, idx, candidate, explain),
        BodyHash::compute(&record.body).to_string(),
    ))
}

fn age_days(staleness_seconds: i64) -> u32 {
    let days = staleness_seconds.max(0) / SECONDS_PER_DAY;
    u32::try_from(days).unwrap_or(u32::MAX)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "read-filter ranking consumes f32 and clamps non-finite values"
)]
fn relevance_score(
    mode: SearchMode,
    idx: usize,
    candidate: &SearchCandidate,
    explain: Option<&[ScoreExplain]>,
) -> f32 {
    let score = match mode {
        SearchMode::Keyword => -candidate.bm25,
        SearchMode::Semantic => candidate
            .semantic_distance
            .map_or(0.0, |distance| -f64::from(distance)),
        SearchMode::Hybrid => explain
            .and_then(|entries| entries.get(idx))
            .map_or_else(|| fallback_rank_score(idx), |entry| entry.final_score),
    };
    if score.is_finite() {
        score.clamp(f64::from(f32::MIN), f64::from(f32::MAX)) as f32
    } else {
        0.0
    }
}

#[allow(clippy::cast_precision_loss)]
fn fallback_rank_score(idx: usize) -> f64 {
    1.0 / (1.0 + idx as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::{
        Edge, EdgeDir, EdgeKey, HybridSearchPage, KeywordSearchPage, ListArgs, ListPage,
        MemoryStoreCapabilities, RecordVersion, SemanticSearchPage, StoreError, TombstoneReason,
        UpsertOutcome,
    };
    use crate::contract::version::{ContractVersion, VersionRange};
    use crate::domain::record::MemoryRecord;
    use crate::domain::{RecordId, TargetId};
    use std::sync::Mutex;

    /// Stub store that records which leg was called.
    ///
    /// `last_hybrid` captures the
    /// `(blend, rrf_k, rerank_topk, graph_confidence_min)` args from the
    /// most recent `search_hybrid` call so tests can assert config knobs
    /// flow through correctly.
    struct CallRecorder {
        calls: Mutex<Vec<&'static str>>,
        capabilities: MemoryStoreCapabilities,
        last_hybrid: Mutex<Option<(f32, usize, usize, f32)>>,
    }

    #[async_trait::async_trait]
    impl MemoryStore for CallRecorder {
        fn name(&self) -> &'static str {
            "recorder"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            &self.capabilities
        }
        fn supported_contract_versions(&self) -> VersionRange {
            use super::super::super::contract::memory_store::CONTRACT_VERSION;
            // Accept exactly the current contract version and up to the next minor.
            VersionRange::new(
                CONTRACT_VERSION,
                ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
            )
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            unimplemented!()
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            Ok(ListPage {
                records: vec![],
                next_cursor: None,
            })
        }
        async fn tombstone(
            &self,
            _id: &RecordId,
            _reason: TombstoneReason,
        ) -> Result<(), StoreError> {
            Ok(())
        }
        async fn versions(&self, _target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            Ok(vec![])
        }
        async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
            Ok(())
        }
        async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
            Ok(false)
        }
        async fn neighbours(&self, _id: &RecordId, _dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            Ok(vec![])
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            self.calls.lock().expect("mutex").push("keyword");
            Ok(KeywordSearchPage {
                candidates: vec![],
                next_cursor: None,
                explain: None,
            })
        }
        async fn search_semantic(
            &self,
            _args: &SemanticSearchArgs<'_>,
        ) -> Result<SemanticSearchPage, StoreError> {
            self.calls.lock().expect("mutex").push("semantic");
            Ok(SemanticSearchPage {
                candidates: vec![],
                explain: None,
            })
        }
        async fn search_hybrid(
            &self,
            args: &HybridSearchArgs<'_>,
        ) -> Result<HybridSearchPage, StoreError> {
            self.calls.lock().expect("mutex").push("hybrid");
            *self.last_hybrid.lock().expect("mutex") = Some((
                args.blend,
                args.rrf_k,
                args.rerank_topk,
                args.graph_confidence_min,
            ));
            Ok(HybridSearchPage {
                candidates: vec![],
                explain: None,
                degraded_legs: vec![],
            })
        }
    }

    fn caps(keyword: bool, semantic: bool, hybrid: bool) -> CapabilitySet {
        CapabilitySet {
            keyword_search: keyword,
            semantic_search: semantic,
            hybrid_search: hybrid,
            llm_extract: false,
            agent_extract: false,
            agent_dream: false,
            screen_capture_enabled: false,
            graph_edges: false,
            policy_trace: true,
            replay_sequence: true,
            replay_challenge: true,
        }
    }

    fn req(mode: SearchMode) -> SearchRequest {
        SearchRequest {
            query: "hello".to_owned(),
            mode,
            limit: 10,
            include_reasoning: false,
            visibility_allowlist: vec![],
            auth_scope: ScopeTuple::default(),
            rebac: crate::rebac::RebacContext::default(),
            model_label: "MiniLM-L6-v2".to_owned(),
            filter: None,
            explain: false,
        }
    }

    #[test]
    fn default_search_visibility_fails_closed_for_shared_tiers() {
        let request = req(SearchMode::Keyword);
        let (allowed, decisions) = visibility_allowlist(&request);

        assert_eq!(
            allowed,
            vec![MemoryVisibility::Private, MemoryVisibility::Session]
        );
        assert_eq!(decisions.len(), 4, "one decision per shared tier");
        assert!(
            decisions.iter().all(|decision| !decision.allowed()),
            "shared tiers must deny without ReBAC relations"
        );
    }

    #[test]
    fn search_visibility_allows_shared_tier_with_matching_rebac_relation() {
        let principal = crate::domain::Identity::parse("agt:cairn:test:reader:v1").expect("valid");
        let scope = ScopeTuple {
            tenant: Some("acme".to_owned()),
            workspace: Some("eng".to_owned()),
            entity: Some("ingest".to_owned()),
            ..ScopeTuple::default()
        };
        let mut request = req(SearchMode::Keyword);
        request.auth_scope = scope.clone();
        request.rebac = crate::rebac::RebacContext::new(
            principal.clone(),
            vec![crate::rebac::RebacRelation::new(
                principal,
                crate::rebac::RebacAction::Read,
                scope,
                MemoryVisibility::Project,
            )],
        );

        let (allowed, decisions) = visibility_allowlist(&request);

        assert!(allowed.contains(&MemoryVisibility::Project));
        assert!(
            !allowed.contains(&MemoryVisibility::Team),
            "relations are exact by shared tier"
        );
        assert!(
            decisions
                .iter()
                .any(|decision| decision.tier == MemoryVisibility::Project && decision.allowed()),
            "project-tier relation should be visible in policy trace decisions"
        );
    }

    #[tokio::test]
    async fn keyword_routes_to_keyword_leg() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities {
                fts: true,
                vector: false,
                graph_edges: false,
                transactions: true,
                per_record_consent_model: true,
                graph_search: false,
            },
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        run(
            &store,
            &config,
            &caps(true, false, false),
            req(SearchMode::Keyword),
        )
        .await
        .expect("ok");
        assert_eq!(store.calls.lock().expect("mutex").as_slice(), &["keyword"]);
    }

    #[tokio::test]
    async fn semantic_rejected_when_capability_absent() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities::default(),
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        let err = run(
            &store,
            &config,
            &caps(true, false, false),
            req(SearchMode::Semantic),
        )
        .await
        .expect_err("expected error");
        match err {
            SearchError::CapabilityUnavailable { capability } => {
                assert_eq!(capability, "cairn.mcp.v1.search.semantic");
            }
            other => panic!("wrong error: {other:?}"),
        }
        assert!(store.calls.lock().expect("mutex").is_empty());
    }

    #[tokio::test]
    async fn hybrid_routes_when_capability_set() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities {
                fts: true,
                vector: true,
                graph_edges: false,
                transactions: true,
                per_record_consent_model: true,
                graph_search: false,
            },
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        run(
            &store,
            &config,
            &caps(true, true, true),
            req(SearchMode::Hybrid),
        )
        .await
        .expect("ok");
        assert_eq!(store.calls.lock().expect("mutex").as_slice(), &["hybrid"]);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "stub MemoryStore impl is unavoidably wide"
    )]
    #[tokio::test]
    async fn hybrid_propagates_degraded_legs_through_search_outcome() {
        use crate::search::{DegradationReason, DegradedLeg, GraphSource};

        struct DegradedHybridStore;

        #[async_trait::async_trait]
        impl MemoryStore for DegradedHybridStore {
            fn name(&self) -> &'static str {
                "degraded"
            }
            fn capabilities(&self) -> &MemoryStoreCapabilities {
                static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                    fts: true,
                    vector: true,
                    graph_edges: false,
                    transactions: true,
                    per_record_consent_model: true,
                    graph_search: false,
                };
                &CAPS
            }
            fn supported_contract_versions(&self) -> crate::contract::version::VersionRange {
                use crate::contract::memory_store::CONTRACT_VERSION;
                use crate::contract::version::ContractVersion;
                crate::contract::version::VersionRange::new(
                    CONTRACT_VERSION,
                    ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
                )
            }
            async fn upsert(
                &self,
                _r: &crate::domain::record::MemoryRecord,
            ) -> Result<crate::contract::memory_store::UpsertOutcome, StoreError> {
                unimplemented!()
            }
            async fn get(
                &self,
                _id: &crate::domain::RecordId,
            ) -> Result<Option<crate::domain::record::MemoryRecord>, StoreError> {
                Ok(None)
            }
            async fn list(
                &self,
                _args: &crate::contract::memory_store::ListArgs,
            ) -> Result<crate::contract::memory_store::ListPage, StoreError> {
                unimplemented!()
            }
            async fn tombstone(
                &self,
                _id: &crate::domain::RecordId,
                _reason: crate::contract::memory_store::TombstoneReason,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn versions(
                &self,
                _t: &crate::domain::TargetId,
            ) -> Result<Vec<crate::contract::memory_store::RecordVersion>, StoreError> {
                Ok(vec![])
            }
            async fn put_edge(
                &self,
                _e: &crate::contract::memory_store::Edge,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn remove_edge(
                &self,
                _k: &crate::contract::memory_store::EdgeKey,
            ) -> Result<bool, StoreError> {
                Ok(false)
            }
            async fn neighbours(
                &self,
                _id: &crate::domain::RecordId,
                _d: crate::contract::memory_store::EdgeDir,
            ) -> Result<Vec<crate::contract::memory_store::Edge>, StoreError> {
                Ok(vec![])
            }
            async fn search_keyword(
                &self,
                _args: &KeywordSearchArgs<'_>,
            ) -> Result<KeywordSearchPage, StoreError> {
                Ok(KeywordSearchPage {
                    candidates: vec![],
                    next_cursor: None,
                    explain: None,
                })
            }
            async fn search_semantic(
                &self,
                _args: &SemanticSearchArgs<'_>,
            ) -> Result<SemanticSearchPage, StoreError> {
                Ok(SemanticSearchPage {
                    candidates: vec![],
                    explain: None,
                })
            }
            async fn search_hybrid(
                &self,
                _args: &HybridSearchArgs<'_>,
            ) -> Result<HybridSearchPage, StoreError> {
                Ok(HybridSearchPage {
                    candidates: vec![],
                    explain: None,
                    degraded_legs: vec![
                        DegradedLeg::Semantic {
                            reason: DegradationReason::TransientProviderOutage,
                        },
                        DegradedLeg::Graph {
                            reason: DegradationReason::CapabilityUnavailable,
                            source: GraphSource::All,
                        },
                    ],
                })
            }
        }

        let config = CairnConfig::default();
        let outcome = run(
            &DegradedHybridStore,
            &config,
            &caps(true, true, true),
            req(SearchMode::Hybrid),
        )
        .await
        .expect("hybrid run");
        assert_eq!(
            outcome.degraded_legs.len(),
            2,
            "hybrid must propagate store's degraded_legs through SearchOutcome"
        );
        assert!(
            outcome.semantic_degraded,
            "transient semantic provider outage must set semantic_degraded"
        );
    }

    #[tokio::test]
    async fn keyword_outcome_has_empty_degraded_legs() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities::default(),
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        let outcome = run(
            &store,
            &config,
            &caps(true, false, false),
            req(SearchMode::Keyword),
        )
        .await
        .expect("ok");
        assert!(
            outcome.degraded_legs.is_empty(),
            "keyword leg never reports degraded_legs"
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "stub MemoryStore impl is unavoidably wide"
    )]
    #[tokio::test]
    async fn auth_scope_threaded_into_keyword_args() {
        use std::sync::Mutex as StdMutex;

        struct ScopeRecorder {
            captured: StdMutex<Option<ScopeTuple>>,
        }
        #[async_trait::async_trait]
        impl MemoryStore for ScopeRecorder {
            fn name(&self) -> &'static str {
                "scope-recorder"
            }
            fn capabilities(&self) -> &MemoryStoreCapabilities {
                static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                    fts: true,
                    vector: false,
                    graph_edges: false,
                    transactions: true,
                    per_record_consent_model: true,
                    graph_search: false,
                };
                &CAPS
            }
            fn supported_contract_versions(&self) -> crate::contract::version::VersionRange {
                use crate::contract::memory_store::CONTRACT_VERSION;
                use crate::contract::version::ContractVersion;
                crate::contract::version::VersionRange::new(
                    CONTRACT_VERSION,
                    ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
                )
            }
            async fn upsert(
                &self,
                _r: &crate::domain::record::MemoryRecord,
            ) -> Result<crate::contract::memory_store::UpsertOutcome, StoreError> {
                unimplemented!()
            }
            async fn get(
                &self,
                _id: &crate::domain::RecordId,
            ) -> Result<Option<crate::domain::record::MemoryRecord>, StoreError> {
                Ok(None)
            }
            async fn list(
                &self,
                _args: &crate::contract::memory_store::ListArgs,
            ) -> Result<crate::contract::memory_store::ListPage, StoreError> {
                unimplemented!()
            }
            async fn tombstone(
                &self,
                _id: &crate::domain::RecordId,
                _reason: crate::contract::memory_store::TombstoneReason,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn versions(
                &self,
                _t: &crate::domain::TargetId,
            ) -> Result<Vec<crate::contract::memory_store::RecordVersion>, StoreError> {
                Ok(vec![])
            }
            async fn put_edge(
                &self,
                _e: &crate::contract::memory_store::Edge,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn remove_edge(
                &self,
                _k: &crate::contract::memory_store::EdgeKey,
            ) -> Result<bool, StoreError> {
                Ok(false)
            }
            async fn neighbours(
                &self,
                _id: &crate::domain::RecordId,
                _d: crate::contract::memory_store::EdgeDir,
            ) -> Result<Vec<crate::contract::memory_store::Edge>, StoreError> {
                Ok(vec![])
            }
            async fn search_keyword(
                &self,
                args: &KeywordSearchArgs<'_>,
            ) -> Result<KeywordSearchPage, StoreError> {
                *self.captured.lock().expect("mutex") = Some(args.auth_scope.clone());
                Ok(KeywordSearchPage {
                    candidates: vec![],
                    next_cursor: None,
                    explain: None,
                })
            }
            async fn search_semantic(
                &self,
                _args: &SemanticSearchArgs<'_>,
            ) -> Result<SemanticSearchPage, StoreError> {
                unimplemented!()
            }
            async fn search_hybrid(
                &self,
                _args: &HybridSearchArgs<'_>,
            ) -> Result<HybridSearchPage, StoreError> {
                unimplemented!()
            }
        }

        let store = ScopeRecorder {
            captured: StdMutex::new(None),
        };
        let mut request = req(SearchMode::Keyword);
        request.auth_scope = ScopeTuple {
            tenant: Some("acme".into()),
            ..Default::default()
        };
        let config = CairnConfig::default();
        run(&store, &config, &caps(true, false, false), request)
            .await
            .expect("ok");
        let captured = store.captured.lock().expect("mutex").clone();
        assert_eq!(
            captured.expect("captured").tenant.as_deref(),
            Some("acme"),
            "verb dispatcher must thread request.auth_scope into the leg's args"
        );
    }

    #[tokio::test]
    async fn empty_query_rejected() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities::default(),
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        let mut request = req(SearchMode::Keyword);
        request.query = "  ".to_owned();
        let err = run(&store, &config, &caps(true, false, false), request)
            .await
            .expect_err("expected error");
        assert!(matches!(err, SearchError::InvalidArgs { .. }));
    }

    // M4-A: explain rejected when policy_trace is absent
    #[tokio::test]
    async fn explain_rejected_when_policy_trace_absent() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities {
                fts: true,
                vector: true,
                graph_edges: false,
                transactions: true,
                per_record_consent_model: true,
                graph_search: false,
            },
            last_hybrid: Mutex::new(None),
        };
        let config = CairnConfig::default();
        let mut request = req(SearchMode::Hybrid);
        request.explain = true;
        let mut c = caps(true, true, true); // mode caps allowed
        c.policy_trace = false; // but policy_trace not advertised
        let err = run(&store, &config, &c, request)
            .await
            .expect_err("should reject");
        match err {
            SearchError::CapabilityUnavailable { capability } => {
                assert_eq!(capability, "cairn.mcp.v1.policy_trace");
            }
            other => panic!("wrong error: {other:?}"),
        }
        // No store call should have happened.
        assert!(store.calls.lock().expect("mutex").is_empty());
    }

    // M4-B: hybrid config knobs (blend, rrf_k, rerank_topk) flow through from config
    #[tokio::test]
    async fn hybrid_args_carry_config_knobs() {
        let store = CallRecorder {
            calls: Mutex::new(vec![]),
            capabilities: MemoryStoreCapabilities {
                fts: true,
                vector: true,
                graph_edges: false,
                transactions: true,
                per_record_consent_model: true,
                graph_search: false,
            },
            last_hybrid: Mutex::new(None),
        };
        let mut config = CairnConfig::default();
        config.search.rerank_blend = 0.42;
        config.search.rrf_k = 99;
        config.search.rerank_topk = 33;
        config.search.graph_confidence_min = 0.7;
        run(
            &store,
            &config,
            &caps(true, true, true),
            req(SearchMode::Hybrid),
        )
        .await
        .expect("hybrid run");
        let captured = store.last_hybrid.lock().expect("mutex").expect("captured");
        assert!((captured.0 - 0.42).abs() < 1e-6, "blend mismatch");
        assert_eq!(captured.1, 99, "rrf_k mismatch");
        assert_eq!(captured.2, 33, "rerank_topk mismatch");
        assert!(
            (captured.3 - 0.7).abs() < 1e-6,
            "graph_confidence_min mismatch: got {}",
            captured.3,
        );
    }

    // M4-C: token_budget_trim is invoked — oversized candidates are trimmed.
    // Allow function_too_many_lines: the length comes from the mandatory
    // MemoryStore boilerplate for OversizedStore, not from logic complexity.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn dispatcher_applies_token_budget_trim() {
        use crate::domain::ScopeTuple;
        use crate::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};

        /// Build a `SearchCandidate` with a fixed-length snippet.
        fn oversized_candidate(index: usize, snippet_len: usize) -> SearchCandidate {
            let id_str = format!("01HQZX9F5N{index:016X}");
            SearchCandidate {
                record_id: RecordId::parse(id_str).expect("valid record id"),
                target_id: TargetId::parse("01HQZX9F5N0000000000000000").expect("valid target id"),
                scope: ScopeTuple::default(),
                kind: MemoryKind::Fact,
                class: MemoryClass::Episodic,
                visibility: MemoryVisibility::Private,
                bm25: 0.0,
                recency_seconds: 0,
                confidence: 1.0,
                salience: 1.0,
                staleness_seconds: 0,
                snippet: "x".repeat(snippet_len),
                record_json: "{}".to_owned(),
                semantic_distance: None,
            }
        }

        struct OversizedStore;

        #[async_trait::async_trait]
        impl MemoryStore for OversizedStore {
            fn name(&self) -> &'static str {
                "oversized"
            }
            fn capabilities(&self) -> &MemoryStoreCapabilities {
                static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                    fts: true,
                    vector: false,
                    graph_edges: false,
                    transactions: true,
                    per_record_consent_model: true,
                    graph_search: false,
                };
                &CAPS
            }
            fn supported_contract_versions(&self) -> VersionRange {
                use super::super::super::contract::memory_store::CONTRACT_VERSION;
                VersionRange::new(
                    CONTRACT_VERSION,
                    ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
                )
            }
            async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
                unimplemented!()
            }
            async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
                Ok(None)
            }
            async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
                unimplemented!()
            }
            async fn tombstone(
                &self,
                _id: &RecordId,
                _reason: TombstoneReason,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn versions(&self, _target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
                unimplemented!()
            }
            async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
                unimplemented!()
            }
            async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
                unimplemented!()
            }
            async fn neighbours(
                &self,
                _id: &RecordId,
                _dir: EdgeDir,
            ) -> Result<Vec<Edge>, StoreError> {
                unimplemented!()
            }
            async fn search_keyword(
                &self,
                _args: &KeywordSearchArgs<'_>,
            ) -> Result<KeywordSearchPage, StoreError> {
                // Return 3 candidates each with a 1000-char snippet.
                let cands = (0..3).map(|i| oversized_candidate(i, 1000)).collect();
                Ok(KeywordSearchPage {
                    candidates: cands,
                    next_cursor: None,
                    explain: None,
                })
            }
            async fn search_semantic(
                &self,
                _args: &SemanticSearchArgs<'_>,
            ) -> Result<SemanticSearchPage, StoreError> {
                unimplemented!()
            }
            async fn search_hybrid(
                &self,
                _args: &HybridSearchArgs<'_>,
            ) -> Result<HybridSearchPage, StoreError> {
                unimplemented!()
            }
        }

        let mut config = CairnConfig::default();
        // 1500 chars budget is less than 3 * 1000 = 3000, so the trim should
        // keep only the first candidate (which alone is 1000 chars — within
        // budget) and drop the rest (1000 + 1000 = 2000 > 1500).
        config.search.max_snippet_chars_per_page = 1500;
        let outcome = run(
            &OversizedStore,
            &config,
            &caps(true, false, false),
            req(SearchMode::Keyword),
        )
        .await
        .expect("ok");
        assert!(
            outcome.candidates.len() < 3,
            "trim should have reduced count from 3, got {}",
            outcome.candidates.len()
        );
    }

    fn reasoning_candidate(
        prefix: &str,
        index: usize,
        record_json: &serde_json::Value,
    ) -> SearchCandidate {
        use crate::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};

        let id_str = format!("{prefix}{index:016X}");
        SearchCandidate {
            record_id: RecordId::parse(id_str).expect("valid record id"),
            target_id: TargetId::parse(format!("{prefix}0000000000000000"))
                .expect("valid target id"),
            scope: ScopeTuple::default(),
            kind: MemoryKind::Fact,
            class: MemoryClass::Episodic,
            visibility: MemoryVisibility::Private,
            bm25: 0.0,
            recency_seconds: 0,
            confidence: 1.0,
            salience: 1.0,
            staleness_seconds: 0,
            snippet: format!("candidate-{index}"),
            record_json: serde_json::to_string(record_json).expect("record json"),
            semantic_distance: None,
        }
    }

    struct ReasoningStore {
        id_prefix: &'static str,
    }

    #[async_trait::async_trait]
    impl MemoryStore for ReasoningStore {
        fn name(&self) -> &'static str {
            "reasoning"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: true,
                vector: false,
                graph_edges: false,
                transactions: true,
                per_record_consent_model: true,
                graph_search: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            use super::super::super::contract::memory_store::CONTRACT_VERSION;
            VersionRange::new(
                CONTRACT_VERSION,
                ContractVersion::new(CONTRACT_VERSION.major, CONTRACT_VERSION.minor + 1, 0),
            )
        }
        async fn upsert(&self, _r: &MemoryRecord) -> Result<UpsertOutcome, StoreError> {
            unimplemented!()
        }
        async fn get(&self, _id: &RecordId) -> Result<Option<MemoryRecord>, StoreError> {
            Ok(None)
        }
        async fn list(&self, _args: &ListArgs) -> Result<ListPage, StoreError> {
            unimplemented!()
        }
        async fn tombstone(
            &self,
            _id: &RecordId,
            _reason: TombstoneReason,
        ) -> Result<(), StoreError> {
            unimplemented!()
        }
        async fn versions(&self, _target: &TargetId) -> Result<Vec<RecordVersion>, StoreError> {
            unimplemented!()
        }
        async fn put_edge(&self, _edge: &Edge) -> Result<(), StoreError> {
            unimplemented!()
        }
        async fn remove_edge(&self, _key: &EdgeKey) -> Result<bool, StoreError> {
            unimplemented!()
        }
        async fn neighbours(&self, _id: &RecordId, _dir: EdgeDir) -> Result<Vec<Edge>, StoreError> {
            unimplemented!()
        }
        async fn search_keyword(
            &self,
            _args: &KeywordSearchArgs<'_>,
        ) -> Result<KeywordSearchPage, StoreError> {
            Ok(KeywordSearchPage {
                candidates: vec![
                    reasoning_candidate(
                        self.id_prefix,
                        0,
                        &serde_json::json!({
                            "kind": "trace",
                            "extra_frontmatter": {
                                "trace_blocks": [
                                    {"kind": "reasoning", "text": "private chain"}
                                ]
                            }
                        }),
                    ),
                    reasoning_candidate(
                        self.id_prefix,
                        1,
                        &serde_json::json!({
                            "kind": "reasoning"
                        }),
                    ),
                    reasoning_candidate(self.id_prefix, 2, &serde_json::json!({})),
                ],
                next_cursor: None,
                explain: None,
            })
        }
        async fn search_semantic(
            &self,
            _args: &SemanticSearchArgs<'_>,
        ) -> Result<SemanticSearchPage, StoreError> {
            unimplemented!()
        }
        async fn search_hybrid(
            &self,
            _args: &HybridSearchArgs<'_>,
        ) -> Result<HybridSearchPage, StoreError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn dispatcher_hides_reasoning_candidates_by_default() {
        let config = CairnConfig::default();
        let outcome = run(
            &ReasoningStore {
                id_prefix: "01HQZX9F5P",
            },
            &config,
            &caps(true, false, false),
            req(SearchMode::Keyword),
        )
        .await
        .expect("ok");

        assert_eq!(
            outcome.candidates.len(),
            1,
            "reasoning result should be filtered"
        );
        assert_eq!(outcome.candidates[0].snippet, "candidate-2");
    }

    #[tokio::test]
    async fn dispatcher_keeps_reasoning_candidates_when_opted_in() {
        let config = CairnConfig::default();
        let mut request = req(SearchMode::Keyword);
        request.include_reasoning = true;
        let outcome = run(
            &ReasoningStore {
                id_prefix: "01HQZX9F5Q",
            },
            &config,
            &caps(true, false, false),
            request,
        )
        .await
        .expect("ok");

        assert_eq!(
            outcome.candidates.len(),
            3,
            "opt-in should keep reasoning result"
        );
    }
}
