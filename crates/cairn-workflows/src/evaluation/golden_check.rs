//! `GoldenCheck` — pluggable contract for the minimum-path evaluation
//! sweeps (issue #91, brief §15).
//!
//! Each check returns a deterministic [`CheckOutcome`] for a given
//! vault snapshot; replays against the same state must produce the
//! same outcome bytes so brief §15's release-gating contract holds.

use std::sync::Arc;

use async_trait::async_trait;
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::ScopeTuple;
use cairn_core::domain::record::MemoryRecord;

/// Maximum records a single sweep traverses before it bails with a
/// `Failed` outcome. Brief §15 release gating prefers a loud failure
/// over a silent "first page only" green light (round-2 adversarial
/// review #4). Bump the cap when vaults grow.
const CHECK_PAGINATION_CAP: usize = 100_000;
const CHECK_PAGE_SIZE: usize = 1_000;

/// Walk every active record matching `scope`, paginating through the
/// `MemoryStore::list` cursor. Returns `Err` when the cap is reached
/// so the caller can downgrade to `Failed` rather than silently
/// report `Passed` on truncated input.
async fn collect_all_records(
    store: &dyn MemoryStore,
    scope: Option<&ScopeTuple>,
) -> Result<Vec<MemoryRecord>, Box<dyn std::error::Error + Send + Sync>> {
    let mut out: Vec<MemoryRecord> = Vec::new();
    let mut cursor = None;
    loop {
        let args = ListArgs {
            limit: CHECK_PAGE_SIZE,
            scope: scope.cloned(),
            cursor: cursor.clone(),
            ..ListArgs::default()
        };
        let page = store.list(&args).await?;
        out.extend(page.records);
        if out.len() > CHECK_PAGINATION_CAP {
            return Err(format!(
                "golden-check pagination cap exceeded ({CHECK_PAGINATION_CAP} records)"
            )
            .into());
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(out),
        }
    }
}

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

    /// Execute the check against the supplied store, narrowed by
    /// `scope` when present. Implementations MUST pass `scope` into
    /// every store read so a multi-tenant sweep cannot leak records
    /// across the binding (round-1 adversarial review #4).
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
        scope: Option<&ScopeTuple>,
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
    fn id(&self) -> &'static str {
        "orphan"
    }
    async fn run(
        &self,
        store: &dyn MemoryStore,
        scope: Option<&ScopeTuple>,
    ) -> Result<CheckOutcome, Box<dyn std::error::Error + Send + Sync>> {
        let records = collect_all_records(store, scope).await?;
        let mut bad_ids: Vec<String> = Vec::new();
        for r in &records {
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
    fn id(&self) -> &'static str {
        "tombstone_consistency"
    }
    async fn run(
        &self,
        store: &dyn MemoryStore,
        scope: Option<&ScopeTuple>,
    ) -> Result<CheckOutcome, Box<dyn std::error::Error + Send + Sync>> {
        // The trait contract already guarantees `list` never returns
        // tombstoned rows; we re-assert by checking the version's
        // `tombstoned` flag is false for every returned row.
        // Paginated to keep large vaults from silently passing on
        // first-page-only data (round-2 adversarial review #4).
        let records = collect_all_records(store, scope).await?;
        for rec in &records {
            let history = store.versions(&rec.target_id).await?;
            if let Some(v) = history.iter().rev().find(|v| v.record_id == rec.id)
                && v.tombstoned
            {
                return Ok(CheckOutcome::Failed {
                    details: format!(
                        "active list returned tombstoned record {}",
                        rec.id.as_str()
                    ),
                });
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
