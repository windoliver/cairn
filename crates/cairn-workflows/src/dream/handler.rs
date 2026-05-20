//! `DreamHandler` — tiered dream distillation worker (brief §10.1, §10.2).
//!
//! The `llm` mode reads up to the tier's configured
//! `window_size_records` recent records bound by the payload's scope.
//! The `hybrid` mode first prunes duplicate bodies, then uses the same
//! bounded LLM distillation call. Both modes upsert a deterministic
//! `reasoning` record carrying tier, worker, budget, and source
//! evidence metadata. When no `LLMProvider` is wired the handler returns
//! [`HandlerOutcome::Permanent`](crate::scheduler::HandlerOutcome) so
//! the scheduler stops retrying — the capability gate in `status`
//! mirrors this by holding back `cairn.workflows.v1.dream`.
//!
//! Out of scope (deferred to follow-ups, brief §10.1):
//! * `AgentDreamWorker` / P2 tool-loop workers.
//! * WAL `FlushPlan` integration — the minimum path upserts directly via
//!   `MemoryStore::upsert` like the existing consolidation handler.

use std::sync::Arc;

use cairn_core::config::{DreamConfig, DreamTierConfig, DreamWorkerMode, ExtractBudget};
use cairn_core::contract::job_store::{FailureClass, JobKind, JobPayload};
use cairn_core::contract::llm_provider::{CompletionOutput, CompletionRequest, LLMProvider};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore, TombstoneReason};
use cairn_core::domain::{
    ScopeTuple,
    taxonomy::{MemoryClass, MemoryKind},
};
use tracing::{info, warn};

use crate::dream::DreamPayload;
use crate::scheduler::{HandlerOutcome, JobHandler};
use crate::synthetic::{SyntheticRecordSpec, build_synthetic_record};

/// The `JobKind` discriminator stored in `workflow_jobs.kind`.
pub const DREAM_KIND: &str = "dream.distill_window";

const DREAM_AGENT_ID: &str = "agt:cairn-workflows:dream-handler:v1";
const DREAM_SENSOR_ID: &str = "snr:cairn-workflows:dream:v1";
const DREAM_CONSENT_REF: &str = "consent:system:dream-workflow";

/// Minimum-path `DreamWorkflow` handler. Holds an `Option<Arc<dyn
/// LLMProvider>>` so the same constructor compiles on deployments
/// without an LLM — the handler simply returns `Permanent` in that
/// case, matching the "where configured" qualifier on the issue.
pub struct DreamHandler {
    store: Arc<dyn MemoryStore>,
    config: DreamConfig,
    llm: Option<Arc<dyn LLMProvider>>,
}

impl DreamHandler {
    /// Construct a handler. Pass `llm = None` when no `LLMProvider` is
    /// configured; the handler still implements `JobHandler` so a
    /// queued job drains as `Permanent` rather than monopolising the
    /// worker pool with retries.
    #[must_use]
    pub fn new(
        store: Arc<dyn MemoryStore>,
        config: DreamConfig,
        llm: Option<Arc<dyn LLMProvider>>,
    ) -> Self {
        Self { store, config, llm }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "linear distillation pipeline: window collect → pre-LLM dedupe → LLM call → \
                  post-LLM dedupe → upsert → post-upsert source-liveness recheck. Splitting \
                  the pipeline obscures the round-by-round race-closure structure."
    )]
    async fn run_once(
        &self,
        payload: DreamPayload,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Sanity cap on total pages scanned: each page is
        // `window_size_records` rows, and we walk at most
        // `DREAM_FETCH_PAGE_CAP` pages before bailing — keeps a
        // dream-heavy vault from monopolising the worker.
        const DREAM_FETCH_PAGE_CAP: usize = 64;

        let Some(llm) = self.llm.as_ref() else {
            // The Permanent branch above is what the scheduler sees;
            // this Err path is for callers that drive `run_once`
            // directly (e.g. integration tests).
            return Err("no llm provider configured".into());
        };

        let tier_config = self.config.tier_config(payload.tier);

        // Collect `window_size_records` *non-dream* records, paging
        // through `list` until we either reach the cap or exhaust
        // the store. Without this, a single capped page that
        // happens to contain the most-recent prior dream record
        // would silently shrink the source set, shift the
        // sources-hash in `target_key`, and produce a new target on
        // replay (round-2 adversarial review #2).
        let window_cap = tier_config.window_size_records as usize;
        let mut filtered: Vec<cairn_core::domain::record::MemoryRecord> =
            Vec::with_capacity(window_cap);
        let mut seen_body_hashes = std::collections::BTreeSet::new();
        let mut cursor = None;
        let mut cursor_exhausted = false;
        for _ in 0..DREAM_FETCH_PAGE_CAP {
            let args = ListArgs {
                scope: payload.bound_scope.clone(),
                limit: window_cap.max(1),
                cursor: cursor.clone(),
                ..ListArgs::default()
            };
            let page = self.store.list(&args).await?;
            if page.records.is_empty() {
                cursor_exhausted = true;
                break;
            }
            for r in page.records {
                if r.extra_frontmatter.get("dream").is_some_and(|v| {
                    v.get("produced_by").and_then(|p| p.as_str())
                        == Some("cairn-workflows::DreamHandler")
                }) {
                    continue;
                }
                if tier_config.worker == DreamWorkerMode::Hybrid {
                    let body_hash = crate::synthetic::sha256_hex(r.body.as_bytes());
                    if !seen_body_hashes.insert(body_hash) {
                        continue;
                    }
                }
                filtered.push(r);
                if filtered.len() >= window_cap {
                    break;
                }
            }
            if filtered.len() >= window_cap {
                break;
            }
            if let Some(next) = page.next_cursor {
                cursor = Some(next);
            } else {
                cursor_exhausted = true;
                break;
            }
        }
        // Round-5 adversarial review #2 + round-6 #2 + round-7 #3:
        // distinguish (i) "no eligible records exist" (legitimately
        // Ok) from (ii) "page cap fired before we gathered enough
        // non-dream sources" (a transient condition that should
        // retry — a dream-heavy vault must not permanently distill
        // from a truncated input). Treat ANY cap-with-partial-window
        // state as Err, not just the empty case. Emit a `warn!` so
        // operators see the cap firing in workflow logs before the
        // scheduler classifies the retry.
        if !cursor_exhausted && filtered.len() < window_cap {
            warn!(
                key = %payload.key,
                got = filtered.len(),
                want = window_cap,
                page_cap = DREAM_FETCH_PAGE_CAP,
                "dream: page cap fired with partial window — returning Err to retry"
            );
            return Err(format!(
                "dream: page cap ({DREAM_FETCH_PAGE_CAP}) fired with only \
                 {got}/{want} non-dream sources collected",
                got = filtered.len(),
                want = window_cap,
            )
            .into());
        }
        if filtered.is_empty() {
            info!(key = %payload.key, "dream: no records in window — nothing to distill");
            return Ok(());
        }

        let mut source_record_ids: Vec<String> =
            filtered.iter().map(|r| r.id.as_str().to_owned()).collect();
        source_record_ids.sort();
        // `target_key` folds in (a) the caller's bound scope so two
        // tenants sharing a `payload.key` cannot supersede each
        // other's dream record, and (b) a hash of the sorted source
        // ids so a replay against a different input window produces
        // a different target (round-1 adversarial review #2).
        let scope_wire = payload
            .bound_scope
            .as_ref()
            .map(ScopeTuple::canonical_wire)
            .unwrap_or_default();
        let sources_hash = crate::synthetic::sha256_hex(source_record_ids.join(",").as_bytes());
        let target_key = format!(
            "dream:{tier}:{scope_wire}:{key}:{sources_hash}",
            tier = payload.tier.as_str(),
            key = payload.key
        );

        // Pre-LLM existence check: if an active dream record already
        // exists at this deterministic target, skip the LLM call. Two
        // failure modes this closes (round-4 adversarial review #4):
        //   1. Concurrent same-key jobs: worker A and worker B both
        //      lease their own jobs targeting the same (scope, key,
        //      sources_hash); the second one to reach this check
        //      sees A's upserted record and exits without
        //      regenerating non-deterministic LLM content.
        //   2. Retry after a successful upsert but failed handler
        //      completion: the next attempt finds the prior record
        //      and exits without re-prompting the LLM.
        //
        // Round-10 adversarial review: if the existing active record
        // at this target is itself orphaned (one of its sources has
        // been tombstoned since), tombstone it before returning so
        // retries that arrived AFTER a failed post-upsert cleanup
        // still converge to a clean state. Without this, a dream
        // committed by a prior attempt whose post-upsert recheck /
        // self-tombstone failed could remain active indefinitely.
        let target_id = crate::synthetic::stable_target_id(&target_key)?;
        if let Some(existing) = self.store.get_active_by_target(&target_id).await? {
            let existing_sources: Vec<String> = existing
                .record
                .extra_frontmatter
                .get("dream")
                .and_then(|v| v.get("source_record_ids"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            let mut any_stale = false;
            for source_str in &existing_sources {
                let source_id = cairn_core::domain::RecordId::parse(source_str.clone())?;
                if self.store.get(&source_id).await?.is_none() {
                    any_stale = true;
                    break;
                }
            }
            if any_stale {
                warn!(
                    key = %payload.key,
                    target_id = target_id.as_str(),
                    record_id = existing.record.id.as_str(),
                    "dream: existing active record has stale sources — tombstoning before retry"
                );
                self.store
                    .tombstone(&existing.record.id, TombstoneReason::Forget)
                    .await?;
                // Fall through and let this attempt re-distill.
            } else {
                info!(
                    key = %payload.key,
                    target_id = target_id.as_str(),
                    "dream: active record already exists at deterministic target — skipping LLM"
                );
                return Ok(());
            }
        }

        let prompt = render_dream_prompt(&payload.key, &filtered, &tier_config);
        let req = CompletionRequest::builder()
            .prompt(prompt)
            .budget(ExtractBudget {
                max_tokens: Some(tier_config.completion_token_budget),
                max_wall_ms: Some(tier_config.max_wall_ms),
                max_turns: None,
            })
            .build();
        let body = match llm.complete(&req).await? {
            CompletionOutput::Text(s) => s,
            CompletionOutput::Json(v) => serde_json::to_string(&v).unwrap_or_default(),
            // `CompletionOutput` is `#[non_exhaustive]`; future
            // variants drop into a deterministic fallback rather than
            // crashing the workflow.
            other => format!("{other:?}"),
        };
        let body_budget = tier_config.completion_token_budget as usize * 4;
        if body.len() > body_budget {
            return Err(format!(
                "dream budget exceeded for tier {tier}: body length {actual} > char budget {budget}",
                tier = payload.tier,
                actual = body.len(),
                budget = body_budget,
            )
            .into());
        }

        let mut extras = std::collections::BTreeMap::new();
        extras.insert(
            "dream".to_owned(),
            serde_json::json!({
                "source_record_ids": source_record_ids,
                "window_size":        filtered.len(),
                "tier":               payload.tier.as_str(),
                "worker":             match tier_config.worker {
                    DreamWorkerMode::Llm => "llm",
                    DreamWorkerMode::Hybrid => "hybrid",
                },
                "cadence":            tier_config.cadence,
                "input_window":       tier_config.input_window,
                "output_kind":        tier_config.output_kind,
                "budget": {
                    "max_tokens":     tier_config.completion_token_budget,
                    "max_wall_ms":    tier_config.max_wall_ms,
                    "max_tool_calls": tier_config.max_tool_calls,
                },
                "produced_by":        "cairn-workflows::DreamHandler",
            }),
        );

        // Post-LLM recheck (round-5 adversarial review #1): two
        // workers leasing same-target jobs can both miss the
        // pre-LLM `get_active_by_target` and race to upsert. The
        // recheck here narrows the TOCTOU window from "duration of
        // LLM call" to "duration of one store roundtrip" by
        // dropping our LLM output when a peer already published.
        //
        // Full atomicity needs either (a) an
        // `insert_if_no_active_target` store primitive, or (b)
        // enqueuing dream jobs with `queue_key = target_id` so the
        // scheduler serializes peers. See
        // `DreamPayload::recommended_queue_key`.
        if self.store.get_active_by_target(&target_id).await?.is_some() {
            warn!(
                key = %payload.key,
                target_id = target_id.as_str(),
                "dream: peer published while we were calling LLM — dropping output to avoid version churn"
            );
            return Ok(());
        }

        // Source-liveness revalidation (round-8 adversarial review
        // #2): a concurrent expiration / forget can tombstone one
        // of the records we just digested. Without this check, the
        // dream record would still cite (in `extras.source_record_ids`)
        // sources that have been retired — and worse, surface
        // content into hot reads that the operator believed expired.
        // Drop our output if any source disappeared between the
        // list and now; the next scheduled sweep will pick up the
        // updated active set.
        for id_str in &source_record_ids {
            let id = cairn_core::domain::RecordId::parse(id_str.clone())?;
            if self.store.get(&id).await?.is_none() {
                warn!(
                    key = %payload.key,
                    source_id = id_str.as_str(),
                    "dream: source record tombstoned between list and upsert — dropping output"
                );
                return Ok(());
            }
        }

        let scope = scope_for(&payload);
        let record = build_synthetic_record(SyntheticRecordSpec {
            kind: MemoryKind::Reasoning,
            class: MemoryClass::Semantic,
            scope,
            body,
            target_key: &target_key,
            extras,
            agent_id: DREAM_AGENT_ID,
            sensor_id: DREAM_SENSOR_ID,
            consent_ref: DREAM_CONSENT_REF,
            record_id_override: None,
        })?;

        let outcome = self.store.upsert(&record).await?;

        // Post-upsert source-liveness recheck (round-9 adversarial
        // review #1): the pre-upsert check above is racy — a
        // concurrent `cairn forget --record` or expiration sweep
        // can tombstone one of our source records AFTER we
        // verified it but BEFORE the upsert commits. Without this
        // recheck, the dream record would persist as an active
        // reasoning record whose `extras.dream.source_record_ids`
        // references retired content — and worse, surface that
        // content into hot reads. Mirror the consolidation
        // pattern: recheck after commit, tombstone our own row
        // with `TombstoneReason::Forget` if any source
        // disappeared. (A future dream forget-cleanup handler —
        // parallel to `ConsolidationForgetCleanupHandler` — will
        // also tombstone us as belt-and-suspenders when the forget
        // verb fires; that's deferred to a follow-up that also
        // wires the cleanup into `cairn-cli/src/verbs/forget.rs`.)
        for id_str in &source_record_ids {
            let id = cairn_core::domain::RecordId::parse(id_str.clone())?;
            if self.store.get(&id).await?.is_none() {
                warn!(
                    key = %payload.key,
                    record_id = outcome.record_id.as_str(),
                    source_id = id_str.as_str(),
                    "dream: source tombstoned during upsert — tombstoning own record"
                );
                self.store
                    .tombstone(&outcome.record_id, TombstoneReason::Forget)
                    .await?;
                return Ok(());
            }
        }

        if outcome.content_changed {
            info!(key = %payload.key, "dream: upserted distillation record");
        } else {
            info!(key = %payload.key, "dream: idempotent replay (same body hash)");
        }
        Ok(())
    }
}

fn scope_for(payload: &DreamPayload) -> ScopeTuple {
    // ScopeTuple::validate rejects empty tuples — every synthesized
    // dream record gets at least the workflow's agent identity in
    // the `agent` dim so brief §6.5 validation passes even when the
    // caller supplied no bound scope.
    match payload.bound_scope.as_ref() {
        Some(base) => {
            let mut s = base.clone();
            if s.agent.is_none() {
                s.agent = Some(DREAM_AGENT_ID.to_owned());
            }
            s
        }
        None => ScopeTuple {
            agent: Some(DREAM_AGENT_ID.to_owned()),
            ..ScopeTuple::default()
        },
    }
}

/// Render the LLM prompt for a dream window. Pure: identical record set
/// produces identical bytes, so replays are deterministic when the
/// configured `llm_temperature` is `0.0`.
#[must_use]
pub fn render_dream_prompt(
    key: &str,
    records: &[cairn_core::domain::record::MemoryRecord],
    tier_config: &DreamTierConfig,
) -> String {
    let mut s = String::with_capacity(256 + records.len() * 128);
    s.push_str("# Dream distillation\n\n");
    s.push_str("Tier: ");
    s.push_str(tier_config.tier.as_str());
    s.push('\n');
    s.push_str("Worker: ");
    s.push_str(match tier_config.worker {
        DreamWorkerMode::Llm => "llm",
        DreamWorkerMode::Hybrid => "hybrid",
    });
    s.push('\n');
    s.push_str("Key: ");
    s.push_str(key);
    s.push_str("\nRecords:\n");
    for r in records {
        s.push_str("- ");
        s.push_str(r.id.as_str());
        s.push_str(": ");
        // Cap each record body so the prompt stays bounded even with
        // very long bodies. The LLM sees a digest, not the raw record.
        let trimmed = r.body.chars().take(512).collect::<String>();
        s.push_str(&trimmed);
        s.push('\n');
    }
    s.push_str(
        "\nProduce a 3-5 sentence Markdown distillation that captures the most \
         memorable themes. Return only the distillation body — no preamble.",
    );
    s
}

#[async_trait::async_trait]
impl JobHandler for DreamHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(DREAM_KIND)
    }

    async fn handle(&self, payload_bytes: &JobPayload) -> HandlerOutcome {
        let payload = match DreamPayload::from_bytes(payload_bytes) {
            Ok(p) => p,
            Err(e) => {
                return HandlerOutcome::Permanent {
                    reason: format!("dream payload decode failed: {e}"),
                    class: FailureClass::Validation,
                };
            }
        };

        if self.llm.is_none() {
            warn!(
                key = %payload.key,
                "dream: no LLMProvider wired — declining permanently \
                 (the capability gate in status hides this workflow \
                 until an LLM provider lands)"
            );
            return HandlerOutcome::Permanent {
                reason: "no llm provider configured".into(),
                class: FailureClass::Validation,
            };
        }

        if !self.config.enabled {
            return HandlerOutcome::Permanent {
                reason: "dream.enabled = false in config".into(),
                class: FailureClass::Validation,
            };
        }

        match self.run_once(payload).await {
            Ok(()) => HandlerOutcome::Done,
            Err(e) => HandlerOutcome::Retry {
                reason: e.to_string(),
                class: FailureClass::Transient,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::NoopMemoryStore;
    use cairn_core::config::DreamTier;

    #[tokio::test]
    async fn handle_returns_permanent_when_no_llm() {
        let store: Arc<dyn MemoryStore> = Arc::new(NoopMemoryStore::default());
        let h = DreamHandler::new(
            store,
            DreamConfig {
                enabled: true,
                ..DreamConfig::default()
            },
            None,
        );
        let p = DreamPayload {
            tier: DreamTier::LightSleep,
            key: "sess-1".into(),
            bound_scope: None,
        };
        let bytes = p.to_bytes().expect("encode");
        let outcome = h.handle(&bytes).await;
        assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
    }

    #[tokio::test]
    async fn handle_returns_permanent_when_decode_fails() {
        let store: Arc<dyn MemoryStore> = Arc::new(NoopMemoryStore::default());
        let h = DreamHandler::new(store, DreamConfig::default(), None);
        let outcome = h.handle(&b"{not json".to_vec()).await;
        assert!(matches!(outcome, HandlerOutcome::Permanent { .. }));
    }

    #[test]
    fn render_dream_prompt_is_deterministic_for_empty_window() {
        let tier = DreamTierConfig::light_sleep_default();
        let a = render_dream_prompt("k", &[], &tier);
        let b = render_dream_prompt("k", &[], &tier);
        assert_eq!(a, b);
    }
}
