//! `GoldenCheck` — pluggable contract for the minimum-path evaluation
//! sweeps (issue #91, brief §15).
//!
//! Each check returns a deterministic [`CheckOutcome`] for a given
//! vault snapshot; replays against the same state must produce the
//! same outcome bytes so brief §15's release-gating contract holds.

use std::sync::Arc;

use async_trait::async_trait;
use cairn_core::contract::memory_store::MemoryStore;

/// Outcome of one golden check execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Check passed — no findings.
    Passed,
    /// Check failed. `details` is a single short human-readable line.
    Failed {
        /// Human-readable diagnosis (kept short — emitted into the
        /// report record's Markdown body).
        details: String,
    },
}

impl CheckOutcome {
    /// `true` when this outcome is [`CheckOutcome::Passed`].
    #[must_use]
    pub const fn is_pass(&self) -> bool {
        matches!(self, Self::Passed)
    }
}

/// One golden check. Implementations must be deterministic for a
/// given store snapshot — no wall-clock reads, no random IDs, no
/// dependencies on `MemoryStore` mutation order.
#[async_trait]
pub trait GoldenCheck: Send + Sync {
    /// Stable identifier (e.g. `"orphan"`, `"tombstone_consistency"`).
    /// Used to look up checks from
    /// `EvaluationConfig::checks` and to label outcomes in the report
    /// record.
    fn id(&self) -> &str;

    /// Execute the check against the supplied store.
    ///
    /// # Errors
    /// Implementations propagate any `MemoryStore` failure as a
    /// boxed error so the handler can return
    /// [`HandlerOutcome::Retry`](crate::scheduler::HandlerOutcome::Retry)
    /// without misclassifying a transient store error as a failed
    /// check.
    async fn run(
        &self,
        store: &dyn MemoryStore,
    ) -> Result<CheckOutcome, Box<dyn std::error::Error + Send + Sync>>;
}

/// P0 starter check: every active record has at least one
/// `Provenance.source_ids` entry (i.e. no synthesized record was
/// emitted without a self-link). The trust-boundary invariant that
/// brief §6.5 enforces — surfacing it as a runnable check lets CI
/// catch regressions early.
pub struct OrphanCheck;

#[async_trait]
impl GoldenCheck for OrphanCheck {
    fn id(&self) -> &str {
        "orphan"
    }
    async fn run(
        &self,
        store: &dyn MemoryStore,
    ) -> Result<CheckOutcome, Box<dyn std::error::Error + Send + Sync>> {
        let args = cairn_core::contract::memory_store::ListArgs {
            limit: 10_000,
            ..cairn_core::contract::memory_store::ListArgs::default()
        };
        let page = store.list(&args).await?;
        let mut bad_ids: Vec<String> = Vec::new();
        for r in &page.records {
            if r.provenance.source_ids.is_empty() {
                bad_ids.push(r.id.as_str().to_owned());
            }
        }
        if bad_ids.is_empty() {
            Ok(CheckOutcome::Passed)
        } else {
            bad_ids.sort();
            Ok(CheckOutcome::Failed {
                details: format!("orphan records ({}): {}", bad_ids.len(), bad_ids.join(", ")),
            })
        }
    }
}

/// P0 starter check: every active record's `tombstone_reason` is
/// `None`. `MemoryStore::list` filters tombstoned rows on the read
/// path, so any tombstoned record showing up here would mean the
/// adapter's read-side filter is broken — a regression worth gating
/// CI on.
pub struct TombstoneConsistencyCheck;

#[async_trait]
impl GoldenCheck for TombstoneConsistencyCheck {
    fn id(&self) -> &str {
        "tombstone_consistency"
    }
    async fn run(
        &self,
        store: &dyn MemoryStore,
    ) -> Result<CheckOutcome, Box<dyn std::error::Error + Send + Sync>> {
        let args = cairn_core::contract::memory_store::ListArgs {
            limit: 10_000,
            ..cairn_core::contract::memory_store::ListArgs::default()
        };
        let stored = store.list_active_stored(&args).await?;
        // The trait contract already guarantees we never see
        // tombstoned rows here; we re-assert by checking the
        // version's `tombstoned` flag is false for every returned
        // row. Mismatches indicate an adapter-level bug.
        for rec in &stored {
            let history = store.versions(&rec.record.target_id).await?;
            if let Some(v) = history.iter().rev().find(|v| v.record_id == rec.record.id) {
                if v.tombstoned {
                    return Ok(CheckOutcome::Failed {
                        details: format!(
                            "active list returned tombstoned record {}",
                            rec.record.id.as_str()
                        ),
                    });
                }
            }
        }
        Ok(CheckOutcome::Passed)
    }
}

/// Bundle the P0 starter checks. Callers that don't pin a specific
/// list pass this into [`super::EvaluationHandler::new`].
#[must_use]
pub fn default_checks() -> Vec<Arc<dyn GoldenCheck>> {
    vec![Arc::new(OrphanCheck), Arc::new(TombstoneConsistencyCheck)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_is_pass_helper() {
        assert!(CheckOutcome::Passed.is_pass());
        assert!(
            !CheckOutcome::Failed {
                details: "x".into()
            }
            .is_pass()
        );
    }

    #[test]
    fn default_checks_contains_starters() {
        let checks = default_checks();
        let ids: Vec<&str> = checks.iter().map(|c| c.id()).collect();
        assert!(ids.contains(&"orphan"));
        assert!(ids.contains(&"tombstone_consistency"));
    }
}
