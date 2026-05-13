//! Pure hot-memory assembly for core callers.
//!
//! This module is intentionally adapter-free: it performs deterministic
//! in-memory ranking, rendering, budgeting, and metadata reporting only.

use crate::contract::memory_store::{HotMemoryRequest, MemoryStore, MemoryStoreError};
use serde::{Deserialize, Serialize};

/// Source bucket for hot-memory assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotMemorySourceKind {
    /// Durable purpose statements.
    Purpose,
    /// User or agent profile facts.
    Profile,
    /// Explicitly pinned memory.
    Pinned,
    /// High-salience retrieved records.
    HighSalience,
    /// Current project state.
    ProjectState,
    /// Rolling summary context.
    RollingSummary,
    /// Procedural playbook guidance.
    Playbook,
    /// Recent user behavior signals.
    RecentUserSignal,
}

/// Reason a source did not fully fit in the assembled prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotMemoryTruncationReason {
    /// No budget remained for the source.
    BudgetExhausted,
    /// A section was partially included on a UTF-8 boundary.
    SectionTruncated,
    /// The source did not fit and was omitted.
    RecordOmitted,
}

/// Candidate input source for hot-memory assembly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotMemorySource {
    /// Source bucket.
    pub kind: HotMemorySourceKind,
    /// Optional backing record identifier.
    pub record_id: Option<String>,
    /// Optional source title.
    pub title: Option<String>,
    /// Source body text.
    pub body: String,
    /// Salience score supplied by the caller.
    pub salience: f32,
    /// Evidence score supplied by the caller.
    pub evidence_score: f32,
    /// Centrality score supplied by the caller.
    pub centrality_score: f32,
    /// Update timestamp string, sorted lexicographically descending.
    pub updated_at: String,
}

/// Complete hot-memory assembly input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotMemoryInput {
    /// Candidate sources to assemble.
    pub sources: Vec<HotMemorySource>,
    /// Caller-provided revision for cache-key construction by adapters.
    pub source_revision: String,
}

/// Options controlling assembly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotMemoryOptions {
    /// Maximum prefix size in bytes.
    pub budget_bytes: u32,
    /// Blend weight for centrality in rank calculation.
    pub god_node_weight: f32,
    /// Cache metadata to echo in the output.
    pub cache: HotMemoryCacheInfo,
    /// Enabled source kinds in assembly order. Empty means the default design order.
    pub source_order: Vec<HotMemorySourceKind>,
}

/// Cache state reported by the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotMemoryCacheStatus {
    /// Assembled result came from cache.
    Hit,
    /// Cache did not contain an assembled result.
    Miss,
    /// Cache was refreshed.
    Refreshed,
}

/// Cache metadata echoed through assembly output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotMemoryCacheInfo {
    /// Cache status.
    pub status: HotMemoryCacheStatus,
    /// Caller-defined cache key.
    pub key: String,
}

/// Per-kind source assembly summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotMemorySourceSummary {
    /// Source kind summarized.
    pub kind: HotMemorySourceKind,
    /// Number of candidate sources for this kind.
    pub attempted: u32,
    /// Number of records with any included bytes.
    pub included: u32,
    /// Number of records omitted entirely.
    pub omitted: u32,
    /// Included bytes for this kind.
    pub bytes: u32,
}

/// Truncation or omission decision for a source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotMemoryTruncation {
    /// Source kind affected.
    pub kind: HotMemorySourceKind,
    /// Optional backing record identifier.
    pub record_id: Option<String>,
    /// Reason the source was not fully included.
    pub reason: HotMemoryTruncationReason,
    /// Full rendered source byte count.
    pub attempted_bytes: u32,
    /// Included byte count.
    pub included_bytes: u32,
}

/// Assembled hot-memory prefix and metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotMemoryOutput {
    /// Rendered prefix text.
    pub prefix: String,
    /// Prefix size in bytes.
    pub bytes: u32,
    /// Per-kind source summaries in design order.
    pub sources: Vec<HotMemorySourceSummary>,
    /// Truncation and omission decisions.
    pub truncation: Vec<HotMemoryTruncation>,
    /// Echoed cache metadata.
    pub cache: HotMemoryCacheInfo,
}

impl HotMemoryCacheInfo {
    /// Build cache-hit metadata.
    #[must_use]
    pub fn hit(key: impl Into<String>) -> Self {
        Self {
            status: HotMemoryCacheStatus::Hit,
            key: key.into(),
        }
    }

    /// Build cache-miss metadata.
    #[must_use]
    pub fn miss(key: impl Into<String>) -> Self {
        Self {
            status: HotMemoryCacheStatus::Miss,
            key: key.into(),
        }
    }

    /// Build cache-refresh metadata.
    #[must_use]
    pub fn refreshed(key: impl Into<String>) -> Self {
        Self {
            status: HotMemoryCacheStatus::Refreshed,
            key: key.into(),
        }
    }
}

/// Assemble a deterministic hot-memory prefix from in-memory sources.
#[must_use]
pub fn assemble_hot_memory(input: &HotMemoryInput, options: HotMemoryOptions) -> HotMemoryOutput {
    let weight = clamp_weight(options.god_node_weight);
    let budget = options.budget_bytes as usize;
    let source_order = normalize_source_order(&options.source_order);
    let mut prefix = String::new();
    let mut summaries = Vec::with_capacity(source_order.len());
    let mut truncation = Vec::new();

    for kind in source_order {
        let mut group: Vec<&HotMemorySource> = input
            .sources
            .iter()
            .filter(|source| source.kind == kind)
            .collect();
        group.sort_by(|left, right| compare_sources(left, right, weight));

        let attempted = to_u32(group.len());
        let mut included = 0;
        let mut omitted = 0;
        let mut bytes = 0;

        for source in group {
            let rendered = render_source(source);
            let attempted_bytes = rendered.len();
            let remaining = budget.saturating_sub(prefix.len());

            if attempted_bytes <= remaining {
                prefix.push_str(&rendered);
                included += 1;
                bytes += to_u32(attempted_bytes);
                continue;
            }

            if can_truncate(kind) && remaining > 0 {
                let truncated = truncate_utf8(&rendered, remaining);
                let included_bytes = truncated.len();
                prefix.push_str(truncated);
                if included_bytes > 0 {
                    included += 1;
                    bytes += to_u32(included_bytes);
                } else {
                    omitted += 1;
                }
                truncation.push(HotMemoryTruncation {
                    kind,
                    record_id: source.record_id.clone(),
                    reason: HotMemoryTruncationReason::SectionTruncated,
                    attempted_bytes: to_u32(attempted_bytes),
                    included_bytes: to_u32(included_bytes),
                });
            } else {
                omitted += 1;
                truncation.push(HotMemoryTruncation {
                    kind,
                    record_id: source.record_id.clone(),
                    reason: omission_reason(remaining),
                    attempted_bytes: to_u32(attempted_bytes),
                    included_bytes: 0,
                });
            }
        }

        summaries.push(HotMemorySourceSummary {
            kind,
            attempted,
            included,
            omitted,
            bytes,
        });
    }

    HotMemoryOutput {
        bytes: to_u32(prefix.len()),
        prefix,
        sources: summaries,
        truncation,
        cache: options.cache,
    }
}

/// Assemble hot memory through a store-backed input source and cache.
///
/// This helper stays adapter-free by depending only on the core `MemoryStore`
/// contract and the pure in-memory assembler in this module.
pub async fn assemble_hot_with_store<S: MemoryStore + ?Sized>(
    store: &S,
    request: &HotMemoryRequest,
) -> Result<HotMemoryOutput, MemoryStoreError> {
    let input = store.hot_memory_input(request).await?;
    let key = store.hot_memory_cache_key(request, &input)?;
    if let Some(mut cached) = store.load_hot_memory_cache(&key).await? {
        cached.cache = HotMemoryCacheInfo::hit(key);
        return Ok(cached);
    }
    let output = assemble_hot_memory(
        &input,
        HotMemoryOptions {
            budget_bytes: request.budget_bytes,
            god_node_weight: request.god_node_weight,
            cache: HotMemoryCacheInfo::refreshed(key.clone()),
            source_order: request.source_kinds.clone(),
        },
    );
    store.store_hot_memory_cache(&key, &output).await?;
    Ok(output)
}

fn kind_order() -> &'static [HotMemorySourceKind] {
    &[
        HotMemorySourceKind::Purpose,
        HotMemorySourceKind::Profile,
        HotMemorySourceKind::Pinned,
        HotMemorySourceKind::HighSalience,
        HotMemorySourceKind::ProjectState,
        HotMemorySourceKind::RollingSummary,
        HotMemorySourceKind::Playbook,
        HotMemorySourceKind::RecentUserSignal,
    ]
}

/// Default hot-memory source order from the design.
#[must_use]
pub fn default_source_order() -> Vec<HotMemorySourceKind> {
    kind_order().to_vec()
}

fn normalize_source_order(source_order: &[HotMemorySourceKind]) -> Vec<HotMemorySourceKind> {
    let mut order = if source_order.is_empty() {
        default_source_order()
    } else {
        source_order.to_vec()
    };
    let mut seen = std::collections::HashSet::new();
    order.retain(|kind| seen.insert(*kind));
    order
}

fn compare_sources(
    left: &HotMemorySource,
    right: &HotMemorySource,
    god_node_weight: f32,
) -> std::cmp::Ordering {
    rank(right, god_node_weight)
        .total_cmp(&rank(left, god_node_weight))
        .then_with(|| clean_score(right.salience).total_cmp(&clean_score(left.salience)))
        .then_with(|| {
            clean_score(right.evidence_score).total_cmp(&clean_score(left.evidence_score))
        })
        .then_with(|| right.updated_at.cmp(&left.updated_at))
        .then_with(|| left.record_id.cmp(&right.record_id))
}

fn rank(source: &HotMemorySource, god_node_weight: f32) -> f32 {
    let base = clean_score(source.salience).midpoint(clean_score(source.evidence_score));
    (base * (1.0 - god_node_weight)) + (clean_score(source.centrality_score) * god_node_weight)
}

fn clean_score(score: f32) -> f32 {
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn clamp_weight(weight: f32) -> f32 {
    if weight.is_nan() {
        0.0
    } else {
        weight.clamp(0.0, 1.0)
    }
}

fn render_source(source: &HotMemorySource) -> String {
    let mut out = String::new();
    out.push_str("## ");
    out.push_str(kind_label(source.kind));
    out.push('\n');
    if let Some(title) = &source.title {
        out.push_str(title);
        out.push('\n');
    }
    out.push_str(&source.body);
    out.push_str("\n\n");
    out
}

fn kind_label(kind: HotMemorySourceKind) -> &'static str {
    match kind {
        HotMemorySourceKind::Purpose => "purpose",
        HotMemorySourceKind::Profile => "profile",
        HotMemorySourceKind::Pinned => "pinned",
        HotMemorySourceKind::HighSalience => "high_salience",
        HotMemorySourceKind::ProjectState => "project_state",
        HotMemorySourceKind::RollingSummary => "rolling_summary",
        HotMemorySourceKind::Playbook => "playbook",
        HotMemorySourceKind::RecentUserSignal => "recent_user_signal",
    }
}

fn can_truncate(kind: HotMemorySourceKind) -> bool {
    matches!(
        kind,
        HotMemorySourceKind::Purpose | HotMemorySourceKind::Profile
    )
}

fn omission_reason(remaining: usize) -> HotMemoryTruncationReason {
    if remaining == 0 {
        HotMemoryTruncationReason::BudgetExhausted
    } else {
        HotMemoryTruncationReason::RecordOmitted
    }
}

fn truncate_utf8(input: &str, max_bytes: usize) -> &str {
    let mut idx = max_bytes.min(input.len());
    while !input.is_char_boundary(idx) {
        idx = idx.saturating_sub(1);
    }
    &input[..idx]
}

fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CacheStore {
        input: HotMemoryInput,
        cached: std::sync::Mutex<Option<HotMemoryOutput>>,
    }

    #[async_trait::async_trait]
    impl crate::contract::memory_store::MemoryStore for CacheStore {
        fn name(&self) -> &'static str {
            "cache-store"
        }

        fn capabilities(&self) -> &crate::contract::memory_store::MemoryStoreCapabilities {
            static CAPS: crate::contract::memory_store::MemoryStoreCapabilities =
                crate::contract::memory_store::MemoryStoreCapabilities {
                    fts: true,
                    vector: false,
                    graph_edges: true,
                    transactions: true,
                };
            &CAPS
        }

        fn supported_contract_versions(&self) -> crate::contract::version::VersionRange {
            crate::contract::version::VersionRange::new(
                crate::contract::version::ContractVersion::new(0, 1, 0),
                crate::contract::version::ContractVersion::new(0, 2, 0),
            )
        }

        async fn hot_memory_input(
            &self,
            _request: &crate::contract::memory_store::HotMemoryRequest,
        ) -> Result<HotMemoryInput, crate::contract::memory_store::MemoryStoreError> {
            Ok(self.input.clone())
        }

        fn hot_memory_cache_key(
            &self,
            _request: &crate::contract::memory_store::HotMemoryRequest,
            input: &HotMemoryInput,
        ) -> Result<String, crate::contract::memory_store::MemoryStoreError> {
            Ok(format!("key-{}", input.source_revision))
        }

        async fn load_hot_memory_cache(
            &self,
            _key: &str,
        ) -> Result<Option<HotMemoryOutput>, crate::contract::memory_store::MemoryStoreError>
        {
            Ok(self.cached.lock().expect("test mutex").clone())
        }

        async fn store_hot_memory_cache(
            &self,
            _key: &str,
            output: &HotMemoryOutput,
        ) -> Result<(), crate::contract::memory_store::MemoryStoreError> {
            *self.cached.lock().expect("test mutex") = Some(output.clone());
            Ok(())
        }

        async fn invalidate_hot_memory_cache(
            &self,
            _scope: crate::contract::memory_store::HotMemoryInvalidationScope,
        ) -> Result<u64, crate::contract::memory_store::MemoryStoreError> {
            Ok(0)
        }
    }

    fn source(kind: HotMemorySourceKind, id: &str, body: &str, rank: f32) -> HotMemorySource {
        HotMemorySource {
            kind,
            record_id: Some(id.to_owned()),
            title: Some(id.to_owned()),
            body: body.to_owned(),
            salience: rank,
            evidence_score: rank,
            centrality_score: 0.0,
            updated_at: "2026-05-12T00:00:00Z".to_owned(),
        }
    }

    #[tokio::test]
    async fn assemble_hot_with_store_returns_refreshed_then_hit() {
        let store = CacheStore {
            input: HotMemoryInput {
                sources: vec![source(
                    HotMemorySourceKind::Purpose,
                    "01J0000000000000000000001",
                    "purpose",
                    1.0,
                )],
                source_revision: "rev".to_owned(),
            },
            cached: std::sync::Mutex::new(None),
        };
        let request = crate::contract::memory_store::HotMemoryRequest {
            session_id: None,
            agent_id: None,
            budget_bytes: 4096,
            config_fingerprint: "config".to_owned(),
            god_node_weight: 0.3,
            source_kinds: default_source_order(),
        };
        let first = assemble_hot_with_store(&store, &request)
            .await
            .expect("first");
        assert_eq!(first.cache.status, HotMemoryCacheStatus::Refreshed);
        let second = assemble_hot_with_store(&store, &request)
            .await
            .expect("second");
        assert_eq!(second.cache.status, HotMemoryCacheStatus::Hit);
    }

    #[test]
    fn assembles_sources_in_design_order() {
        let input = HotMemoryInput {
            sources: vec![
                source(
                    HotMemorySourceKind::Playbook,
                    "01J0000000000000000000004",
                    "playbook",
                    0.7,
                ),
                source(
                    HotMemorySourceKind::Purpose,
                    "01J0000000000000000000001",
                    "purpose",
                    0.1,
                ),
                source(
                    HotMemorySourceKind::Profile,
                    "01J0000000000000000000002",
                    "profile",
                    0.1,
                ),
                source(
                    HotMemorySourceKind::Pinned,
                    "01J0000000000000000000003",
                    "pinned",
                    0.9,
                ),
            ],
            source_revision: "rev-a".to_owned(),
        };
        let out = assemble_hot_memory(
            &input,
            HotMemoryOptions {
                budget_bytes: 4096,
                god_node_weight: 0.3,
                cache: HotMemoryCacheInfo::miss("key-a"),
                source_order: default_source_order(),
            },
        );
        assert!(out.prefix.find("purpose").unwrap() < out.prefix.find("profile").unwrap());
        assert!(out.prefix.find("profile").unwrap() < out.prefix.find("pinned").unwrap());
        assert!(out.prefix.find("pinned").unwrap() < out.prefix.find("playbook").unwrap());
        assert_eq!(out.bytes as usize, out.prefix.len());
    }

    #[test]
    fn truncates_on_utf8_boundary_and_reports_decision() {
        let input = HotMemoryInput {
            sources: vec![source(
                HotMemorySourceKind::Purpose,
                "01J0000000000000000000001",
                "alpha em dash beta",
                1.0,
            )],
            source_revision: "rev-b".to_owned(),
        };
        let out = assemble_hot_memory(
            &input,
            HotMemoryOptions {
                budget_bytes: 12,
                god_node_weight: 0.0,
                cache: HotMemoryCacheInfo::miss("key-b"),
                source_order: default_source_order(),
            },
        );
        assert!(out.prefix.is_char_boundary(out.prefix.len()));
        assert!(out.bytes <= 12);
        assert_eq!(out.truncation[0].kind, HotMemorySourceKind::Purpose);
        assert_eq!(
            out.truncation[0].reason,
            HotMemoryTruncationReason::SectionTruncated
        );
    }

    #[test]
    fn centrality_weight_changes_order_within_section() {
        let input = HotMemoryInput {
            sources: vec![
                HotMemorySource {
                    centrality_score: 1.0,
                    ..source(
                        HotMemorySourceKind::HighSalience,
                        "01J0000000000000000000001",
                        "central",
                        0.1,
                    )
                },
                HotMemorySource {
                    centrality_score: 0.0,
                    ..source(
                        HotMemorySourceKind::HighSalience,
                        "01J0000000000000000000002",
                        "evidence",
                        0.9,
                    )
                },
            ],
            source_revision: "rev-c".to_owned(),
        };
        let out = assemble_hot_memory(
            &input,
            HotMemoryOptions {
                budget_bytes: 4096,
                god_node_weight: 0.7,
                cache: HotMemoryCacheInfo::miss("key-c"),
                source_order: default_source_order(),
            },
        );
        assert!(out.prefix.find("central").unwrap() < out.prefix.find("evidence").unwrap());
    }

    #[test]
    fn cache_status_serializes_as_snake_case() {
        let json = serde_json::to_string(&HotMemoryCacheStatus::Refreshed).unwrap();
        assert_eq!(json, r#""refreshed""#);
    }

    #[test]
    fn non_finite_scores_do_not_outrank_valid_scores() {
        let input = HotMemoryInput {
            sources: vec![
                HotMemorySource {
                    salience: f32::INFINITY,
                    evidence_score: f32::NAN,
                    centrality_score: f32::INFINITY,
                    ..source(
                        HotMemorySourceKind::HighSalience,
                        "01J0000000000000000000001",
                        "invalid",
                        0.0,
                    )
                },
                HotMemorySource {
                    salience: 0.5,
                    evidence_score: 0.5,
                    centrality_score: 0.5,
                    ..source(
                        HotMemorySourceKind::HighSalience,
                        "01J0000000000000000000002",
                        "valid",
                        0.0,
                    )
                },
            ],
            source_revision: "rev-d".to_owned(),
        };
        let out = assemble_hot_memory(
            &input,
            HotMemoryOptions {
                budget_bytes: 4096,
                god_node_weight: 0.5,
                cache: HotMemoryCacheInfo::miss("key-d"),
                source_order: default_source_order(),
            },
        );
        assert!(out.prefix.find("valid").unwrap() < out.prefix.find("invalid").unwrap());
    }

    #[test]
    fn ties_fall_back_to_record_id_ascending() {
        let input = HotMemoryInput {
            sources: vec![
                source(
                    HotMemorySourceKind::HighSalience,
                    "01J0000000000000000000002",
                    "second",
                    0.5,
                ),
                source(
                    HotMemorySourceKind::HighSalience,
                    "01J0000000000000000000001",
                    "first",
                    0.5,
                ),
            ],
            source_revision: "rev-e".to_owned(),
        };
        let out = assemble_hot_memory(
            &input,
            HotMemoryOptions {
                budget_bytes: 4096,
                god_node_weight: 0.0,
                cache: HotMemoryCacheInfo::miss("key-e"),
                source_order: default_source_order(),
            },
        );
        assert!(out.prefix.find("first").unwrap() < out.prefix.find("second").unwrap());
    }
}
