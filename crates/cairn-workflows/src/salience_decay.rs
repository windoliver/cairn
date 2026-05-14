//! Salience decay workflow.

use cairn_core::config::SalienceConfig;
use cairn_core::contract::memory_store::{DecayPolicy, MemoryStore, StoreError};
use cairn_store_sqlite::SqliteMemoryStore;

/// Summary emitted by one salience decay workflow run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SalienceDecayReport {
    /// Number of records considered by the decay batch.
    pub records_processed: u32,
    /// Number of decay candidates evicted through the record forget path.
    pub evicted: u32,
    /// Number of processed records retained after decay.
    pub retained: u32,
}

/// Run one bounded salience decay batch and evict eligible candidates.
///
/// Candidates are selected by the store using the configured threshold, age,
/// batch, and pin guardrails. This workflow intentionally delegates deletion to
/// the store's existing `forget_record` path so eviction uses the same lineage
/// tombstoning behavior as user-requested record forgets.
///
/// # Errors
/// Returns a store error if decay or any candidate eviction fails.
pub async fn run_salience_decay(
    store: &SqliteMemoryStore,
    now_ms: i64,
    config: &SalienceConfig,
) -> Result<SalienceDecayReport, StoreError> {
    let outcome = store
        .decay_salience_batch(
            now_ms,
            DecayPolicy {
                decay_rate: config.decay_rate,
                eviction_threshold: config.eviction_threshold,
                min_age_days: config.min_age_days,
                batch_limit: config.batch_limit,
            },
        )
        .await?;

    let mut evicted = 0_u32;
    for candidate in &outcome.eviction_candidates {
        store.forget_record(&candidate.record_id).await?;
        evicted += 1;
    }

    Ok(SalienceDecayReport {
        records_processed: outcome.records_processed,
        evicted,
        retained: outcome.records_processed.saturating_sub(evicted),
    })
}
