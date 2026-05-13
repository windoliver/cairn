//! `ConsolidationHandler` — pulls a window of trace turns from the store,
//! calls [`cairn_core::pipeline::consolidation::compute_rolling_summary`],
//! and upserts the resulting `reasoning` record. Idempotent: re-running
//! against the same window emits the same record body (the deterministic
//! placeholder) and the store deduplicates via body hash.

use std::sync::Arc;

use cairn_core::config::ConsolidationConfig;
use cairn_core::contract::job_store::{JobKind, JobPayload};
use cairn_core::contract::memory_store::{MemoryStore, TombstoneReason, UpsertOutcome};
use cairn_core::domain::{
    ActorChainEntry, ChainRole, EvidenceVector, Identity, Provenance, Rfc3339Timestamp, ScopeTuple,
    TargetId,
    record::{Ed25519Signature, MemoryRecord, RecordId},
    taxonomy::{MemoryClass, MemoryKind, MemoryVisibility},
};
use cairn_core::pipeline::consolidation::{
    RollingSummaryDraft, SummaryStatus, compute_rolling_summary, pick_window,
};
use cairn_store_sqlite::SqliteMemoryStore;
use tracing::{info, warn};

use crate::consolidation::ConsolidationPayload;
use crate::scheduler::{HandlerOutcome, JobHandler};

/// The kind discriminator stored in `workflow_jobs.kind`.
pub const CONSOLIDATION_KIND: &str = "consolidation.rolling_summary";

/// Agent identity used as the author of system-generated rolling-summary
/// records. The workflow host is an agent, not a human, and not a sensor.
const WORKFLOW_AGENT_ID: &str = "agt:cairn-workflows:consolidation-handler:v1";

/// Sensor identity recorded in `provenance.source_sensor`. For workflow-
/// generated records, the workflow itself is the "sensor" that observed the
/// session's turn stream. Uses the `snr:` prefix required by [`Identity`].
const WORKFLOW_SENSOR_ID: &str = "snr:cairn-workflows:consolidation:v1";

/// `consent_ref` placeholder for system-generated records.
const SYSTEM_CONSENT_REF: &str = "consent:system:consolidation-workflow";

/// Handler entry — needs the concrete [`SqliteMemoryStore`] because the
/// trace-window helper lives on the adapter, not the trait. A future
/// `TraceReader` trait could pull this back into core.
pub struct ConsolidationHandler {
    store: Arc<SqliteMemoryStore>,
    config: ConsolidationConfig,
}

impl ConsolidationHandler {
    /// Construct.
    #[must_use]
    pub fn new(store: Arc<SqliteMemoryStore>, config: ConsolidationConfig) -> Self {
        Self { store, config }
    }

    /// Return `true` iff every `record_id` in `source_ids` is still
    /// active (not tombstoned). `MemoryStore::get` returns `Ok(None)`
    /// for tombstoned rows per the trait contract.
    async fn all_sources_still_active(
        &self,
        source_ids: &[String],
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        for id_str in source_ids {
            let id = cairn_core::domain::RecordId::parse(id_str.clone())
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
            if self.store.get(&id).await?.is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn run_once(
        &self,
        payload: ConsolidationPayload,
    ) -> Result<HandlerOutcome, Box<dyn std::error::Error + Send + Sync>> {
        // Pull candidate turn headers, narrowed by the caller's bound
        // scope so a job dispatched by tenant A cannot see tenant B's
        // turn_summary rows that happen to share a session id (round-4
        // adversarial review #1).
        let candidates = self
            .store
            .list_trace_turns_scoped(
                &payload.session_id,
                payload.since_sequence,
                256,
                payload.bound_scope.as_ref(),
            )
            .await?;

        let Some(window) = pick_window(
            &candidates,
            payload.since_sequence,
            self.config.window_size_turns,
            self.config.min_turns_for_trigger,
            self.config.salience_floor,
        ) else {
            info!(session = %payload.session_id, "no eligible window; deferring");
            return Ok(HandlerOutcome::Done);
        };

        let draft = compute_rolling_summary(&window, &self.config)?;
        if matches!(draft.status, SummaryStatus::Deferred) {
            info!(
                session = %payload.session_id,
                "consolidation_deferred (enabled = false)"
            );
            return Ok(HandlerOutcome::Done);
        }

        // Pre-upsert source-liveness check. Best-effort cheap pass that
        // catches the common case where forget completed before we
        // started building the record.
        if !self.all_sources_still_active(&draft.source_record_ids).await? {
            info!(
                session = %payload.session_id,
                "source(s) tombstoned before upsert; retrying with fresh window"
            );
            return Ok(HandlerOutcome::Retry {
                reason: "source tombstoned between read and upsert".into(),
            });
        }

        let record = build_summary_record(&payload, &draft)?;
        let UpsertOutcome {
            record_id,
            content_changed,
            ..
        } = self.store.upsert(&record).await?;

        // Post-upsert source-liveness recheck (round-2 adversarial review #3):
        // The pre-check above is racy — `cairn forget --record` can
        // tombstone a source AFTER the check returned but BEFORE our upsert
        // commits, while our forget-cleanup peer already ran (or has not
        // yet seen our summary because we hadn't written it). Closing this
        // window without a transactional join requires us to recheck after
        // commit: if any source is now tombstoned, tombstone our just-
        // written summary so the contract "no live summary references a
        // forgotten source" holds at-rest, even if briefly violated
        // mid-flight. The forget-cleanup handler's next run will also
        // tombstone us as belt-and-suspenders.
        if !self.all_sources_still_active(&draft.source_record_ids).await? {
            warn!(
                session = %payload.session_id,
                record_id = record_id.as_str(),
                "source tombstoned after upsert — tombstoning own summary"
            );
            self.store
                .tombstone(&record_id, TombstoneReason::Forget)
                .await?;
            return Ok(HandlerOutcome::Done);
        }

        if content_changed {
            info!(
                session = %payload.session_id,
                record_id = record_id.as_str(),
                last_seq = draft.last_sequence,
                "rolling summary emitted"
            );
        } else {
            info!(
                session = %payload.session_id,
                record_id = record_id.as_str(),
                "rolling summary idempotent (same body hash)"
            );
        }

        Ok(HandlerOutcome::Done)
    }
}

/// Build the `reasoning/episodic/private` record that stores the summary.
///
/// The `target_id` is derived deterministically from `(session_id,
/// last_sequence)` so replaying the handler against the same window
/// produces the same `target_id`, and the store deduplicates on body hash.
fn build_summary_record(
    payload: &ConsolidationPayload,
    draft: &RollingSummaryDraft,
) -> Result<MemoryRecord, Box<dyn std::error::Error + Send + Sync>> {
    // --- IDs ---
    // A fresh ULID for the record row.
    let record_id_str = ulid::Ulid::new().to_string();
    let id = RecordId::parse(&record_id_str)?;

    // Stable target_id across replays: hash the logical key
    // "summary:{session_id}:{last_sequence}" into a deterministic ULID-shaped
    // string. We derive it with SHA-256, take the first 16 bytes as a 128-bit
    // value, and encode as uppercase Crockford base32.
    let target_id = stable_target_id(
        payload.bound_scope.as_ref(),
        &payload.session_id,
        draft.last_sequence,
    )?;

    // --- Timestamps ---
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    #[allow(
        clippy::cast_possible_wrap,
        reason = "unix epoch seconds fit in i64 until year ~292 billion"
    )]
    let now_ts = Rfc3339Timestamp::from_unix_secs(now_secs as i64)?;

    // --- Identities ---
    let agent_id = Identity::parse(WORKFLOW_AGENT_ID)?;
    let sensor_id = Identity::parse(WORKFLOW_SENSOR_ID)?;

    // --- Provenance ---
    // source_hash: SHA-256 of the record body so the provenance is
    // self-consistent. We compute it here from the draft body.
    let source_hash = sha256_hex(draft.body.as_bytes());
    let provenance = Provenance {
        source_sensor: sensor_id,
        created_at: now_ts.clone(),
        originating_agent_id: agent_id.clone(),
        source_hash: format!("sha256:{source_hash}"),
        consent_ref: SYSTEM_CONSENT_REF.to_owned(),
        llm_id_if_any: None,
    };

    // --- Actor chain (P0: single Author entry) ---
    let actor_chain = vec![ActorChainEntry {
        role: ChainRole::Author,
        identity: agent_id,
        at: now_ts.clone(),
    }];

    // --- Extra frontmatter ---
    let mut extra_frontmatter = std::collections::BTreeMap::new();
    extra_frontmatter.insert(
        "consolidation".to_owned(),
        serde_json::json!({
            "source_record_ids": draft.source_record_ids,
            "last_sequence":     draft.last_sequence,
            "summary_tokens":    draft.summary_tokens,
            "produced_by":       "cairn-workflows::ConsolidationHandler",
        }),
    );

    // --- Signature ---
    // System-generated records carry the flush-mutated sentinel: they are not
    // signed by a key-resident author. Downstream verifiers MUST treat this as
    // unsigned provenance (brief §4.2 "Signature-first rejection"). This is
    // the same sentinel used by `flush apply` (issue #289) so existing trust
    // boundaries already handle it correctly.
    let signature = Ed25519Signature::flush_mutated_sentinel();

    // Scope the emitted summary with the caller's full bound scope
    // (round-4 adversarial review #1). At single-tenant P0
    // `payload.bound_scope` is `None` and the summary scope reduces to
    // `{ session_id }`, matching prior behavior.
    let summary_scope = match payload.bound_scope.as_ref() {
        Some(base) => ScopeTuple {
            session_id: Some(payload.session_id.clone()),
            ..base.clone()
        },
        None => ScopeTuple {
            session_id: Some(payload.session_id.clone()),
            ..ScopeTuple::default()
        },
    };
    Ok(MemoryRecord {
        id,
        target_id,
        kind: MemoryKind::Reasoning,
        class: MemoryClass::Episodic,
        visibility: MemoryVisibility::Private,
        scope: summary_scope,
        body: draft.body.clone(),
        provenance,
        updated_at: now_ts,
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain,
        signature,
        tags: Vec::new(),
        extra_frontmatter,
        consent_model: None,
    })
}

/// Derive a deterministic, stable [`TargetId`] from `(scope, session_id,
/// last_sequence)`. Uses SHA-256; takes the first 16 bytes as a 128-bit
/// unsigned integer, encodes as 26-char Crockford base32. Highest 3 bits are
/// cleared so the leading character stays in `[0..=7]` (ULID overflow guard).
///
/// The bound scope is folded in via [`ScopeTuple::canonical_wire`] so two
/// scopes sharing a `session_id` at the same `last_sequence` produce
/// distinct `target_id`s and cannot collide on the store's
/// one-active-row-per-target-id invariant (round-5 adversarial review
/// #1). At single-tenant P0 `bound_scope` is `None` and the key reduces
/// to the prior `summary:{session}:{seq}` shape, preserving identity
/// for already-emitted summaries.
fn stable_target_id(
    bound_scope: Option<&ScopeTuple>,
    session_id: &str,
    last_sequence: u32,
) -> Result<TargetId, Box<dyn std::error::Error + Send + Sync>> {
    let scope_wire = bound_scope
        .map(ScopeTuple::canonical_wire)
        .unwrap_or_default();
    let key = if scope_wire.is_empty() {
        format!("summary:{session_id}:{last_sequence}")
    } else {
        format!("summary:{scope_wire}:{session_id}:{last_sequence}")
    };
    let hash = sha256_hex(key.as_bytes());
    // Take first 32 hex chars → 16 bytes → 128-bit value.
    let hi = u64::from_str_radix(&hash[..16], 16)?;
    let lo = u64::from_str_radix(&hash[16..32], 16)?;
    // Clear the top 3 bits of `hi` so the leading Crockford symbol ≤ 7.
    let hi_masked = hi & 0x1FFF_FFFF_FFFF_FFFF_u64;
    let ulid_str = encode_crockford_base32_128(hi_masked, lo);
    Ok(TargetId::parse(ulid_str)?)
}

/// SHA-256 of `bytes`, returned as lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// Encode a 128-bit value `(hi, lo)` as a 26-character Crockford base32
/// string (uppercase, no `I L O U`). Matches the ULID alphabet.
///
/// 26 × 5 = 130 bits; the 2 top bits of the 130-bit value are always zero
/// because the caller masks `hi` to at most 61 significant bits via the
/// `0x1FFF_…` mask. We work via `u128` to avoid per-group branch logic.
fn encode_crockford_base32_128(hi: u64, lo: u64) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    // Pack the 128-bit value into a single u128 (big-endian).
    let v: u128 = (u128::from(hi) << 64) | u128::from(lo);
    let mut out = [0u8; 26];
    for i in 0..26_u32 {
        // Group 0 is the most significant; groups go from bit-124 down to bit-0.
        // Each group is 5 bits; group i starts at bit (25 - i) * 5.
        let shift = (25 - i) * 5;
        let group = ((v >> shift) & 0x1F) as u8;
        out[i as usize] = ALPHABET[group as usize];
    }
    out.iter().map(|&b| char::from(b)).collect()
}

#[async_trait::async_trait]
impl JobHandler for ConsolidationHandler {
    fn kind(&self) -> JobKind {
        JobKind::new(CONSOLIDATION_KIND)
    }

    async fn handle(&self, payload_bytes: &JobPayload) -> HandlerOutcome {
        let payload = match ConsolidationPayload::from_bytes(payload_bytes) {
            Ok(p) => p,
            Err(e) => {
                return HandlerOutcome::Permanent {
                    reason: format!("payload decode: {e}"),
                };
            }
        };
        match self.run_once(payload).await {
            Ok(o) => o,
            Err(e) => {
                warn!(error = %e, "consolidation run failed");
                HandlerOutcome::Retry {
                    reason: e.to_string(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::contract::memory_store::ListArgs;
    use cairn_core::domain::taxonomy::MemoryKind;
    use cairn_test_fixtures::trace::sample_turn_summary;

    async fn make_store() -> Arc<SqliteMemoryStore> {
        Arc::new(
            cairn_store_sqlite::open_in_memory()
                .await
                .expect("open in-memory store"),
        )
    }

    // A 26-char Crockford base32 ULID that is valid for both SessionId and
    // ConsolidationPayload.session_id. ULIDs are Crockford base32 uppercase.
    const SESSION_S1: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const SESSION_S2: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    #[tokio::test]
    #[allow(
        clippy::expect_used,
        reason = "test: panics surface broken invariants immediately"
    )]
    async fn handler_emits_summary_after_threshold() {
        let store = make_store().await;
        for seq in 1..=6 {
            store
                .upsert(&sample_turn_summary(SESSION_S1, seq))
                .await
                .expect("upsert turn");
        }

        let cfg = ConsolidationConfig::default();
        let h = ConsolidationHandler::new(store.clone(), cfg);
        let payload = ConsolidationPayload {
            session_id: SESSION_S1.to_owned(),
            since_sequence: 0, bound_scope: None,
        };
        let outcome = h.handle(&payload.to_bytes().expect("encode")).await;
        assert_eq!(outcome, HandlerOutcome::Done);

        // Confirm a reasoning record was written for this session.
        let listed = store.list(&ListArgs::default()).await.expect("list");
        let any_summary = listed.records.iter().any(|r| {
            r.kind == MemoryKind::Reasoning && r.extra_frontmatter.contains_key("consolidation")
        });
        assert!(any_summary, "summary record not emitted");
    }

    #[tokio::test]
    #[allow(
        clippy::expect_used,
        reason = "test: panics surface broken invariants immediately"
    )]
    async fn handler_idempotent_on_replay() {
        let store = make_store().await;
        for seq in 1..=6 {
            store
                .upsert(&sample_turn_summary(SESSION_S2, seq))
                .await
                .expect("upsert turn");
        }

        let h = ConsolidationHandler::new(store.clone(), ConsolidationConfig::default());
        let payload = ConsolidationPayload {
            session_id: SESSION_S2.to_owned(),
            since_sequence: 0, bound_scope: None,
        };
        let bytes = payload.to_bytes().expect("encode");

        // Run twice.
        let _ = h.handle(&bytes).await;
        let _ = h.handle(&bytes).await;

        let listed = store.list(&ListArgs::default()).await.expect("list");
        let count = listed
            .records
            .iter()
            .filter(|r| r.kind == MemoryKind::Reasoning)
            .count();
        assert_eq!(
            count, 1,
            "second handler call must be a no-op via body-hash dedup"
        );
    }
}
