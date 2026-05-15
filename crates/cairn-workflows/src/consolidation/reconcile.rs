//! Startup reconciliation: scan for sessions whose committed
//! `turn_summary` count has crossed the consolidation threshold but
//! whose `workflow_jobs` row is missing.
//!
//! Closes the round-9 adversarial review #1 crash window: the capture
//! trigger runs outside the per-turn transaction, so a process crash
//! (or transient `SQLite` / `JobStore` error) between the `turn_summary`
//! commit and the enqueue leaves no row to drain. Without this scan
//! the affected session would stay unconsolidated until an unrelated
//! capture happens to re-enqueue.
//!
//! [`reconcile_consolidation_backlog`] is called by `cairn mcp serve`
//! right after `Scheduler::start` succeeds. It is single-shot: any
//! capture that lands after reconciliation falls through the normal
//! trigger path, and follow-up windows are chain-enqueued by the
//! handler.

use std::sync::Arc;

use cairn_core::config::ConsolidationConfig;
use cairn_core::contract::job_store::JobStore;
use cairn_core::domain::ScopeTuple;
use cairn_store_sqlite::{ConsolidationBacklogEntry, SqliteMemoryStore, StoreError};
use tracing::{info, warn};

use crate::consolidation::enqueue_if_due_scoped;

/// Scan every session that has at least `min_turns_for_trigger` active
/// `turn_summary` records past its latest consolidation watermark and
/// enqueue a consolidation job for each. Returns the list of entries
/// considered (whether enqueued, deduped, or skipped) for
/// observability.
///
/// Idempotent: re-running is safe — enqueues with the same
/// `(session, since_sequence, scope)` tuple deduplicate via the
/// `workflow_jobs_dedupe_uniq` index.
///
/// # Errors
/// Returns [`StoreError`] from the backlog scan. Per-row enqueue
/// failures are logged but do not abort the loop.
pub async fn reconcile_consolidation_backlog(
    memory_store: Arc<SqliteMemoryStore>,
    job_store: Arc<dyn JobStore>,
    config: ConsolidationConfig,
    now_ms: i64,
) -> Result<Vec<ConsolidationBacklogEntry>, StoreError> {
    if !config.enabled {
        return Ok(Vec::new());
    }
    let entries = memory_store
        .list_consolidation_backlog(config.min_turns_for_trigger)
        .await?;
    for entry in &entries {
        // Normalize the persisted scope to match capture-time bound_scope
        // (round-10 adversarial review #1). Turn-summary records are
        // written with `session_id` injected into their scope, but
        // capture-time enqueues pass the verb's caller-scope which has
        // no session_id. If we reused the persisted shape here, the
        // canonical_wire fingerprint (and therefore the dedupe key +
        // stable target_id) would differ, letting reconcile create a
        // second job for an already-queued window. Strip session_id so
        // the two paths produce the same dedupe key.
        let mut bound_scope: Option<ScopeTuple> = serde_json::from_str(&entry.scope_json).ok();
        if let Some(scope) = bound_scope.as_mut() {
            scope.session_id = None;
        }
        match enqueue_if_due_scoped(
            job_store.as_ref(),
            &config,
            &entry.session_id,
            entry.since_sequence.saturating_add(entry.active_eligible),
            entry.since_sequence,
            now_ms,
            bound_scope.as_ref(),
        )
        .await
        {
            Ok(_) => info!(
                session = %entry.session_id,
                since_sequence = entry.since_sequence,
                active = entry.active_eligible,
                "reconciled missing consolidation job"
            ),
            Err(e) => warn!(
                session = %entry.session_id,
                error = %e,
                "reconcile enqueue failed"
            ),
        }
    }
    Ok(entries)
}
