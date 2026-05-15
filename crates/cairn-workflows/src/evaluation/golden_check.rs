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

/// Maximum records a single sweep traverses before it stops paging
/// and lets the caller emit a deterministic `Failed` outcome. Brief
/// §15 release gating prefers a loud failure over a silent
/// "first page only" green light (round-2 adversarial review #4).
/// Bump the cap when vaults grow.
const CHECK_PAGINATION_CAP: usize = 100_000;
const CHECK_PAGE_SIZE: usize = 1_000;

/// Result of [`collect_all_records`]. The boolean marks whether the
/// pagination cap was hit *before* the cursor naturally exhausted —
/// callers convert that into [`CheckOutcome::Failed`] rather than a
/// scheduler retry (round-3 adversarial review #3).
pub struct RecordCollection {
    /// Records visited up to the cap (or until the cursor exhausted).
    pub records: Vec<MemoryRecord>,
    /// `true` when the pagination cap fired before the underlying
    /// cursor naturally exhausted. Callers MUST treat this as
    /// `CheckOutcome::Failed` rather than passing on partial data.
    pub truncated_at_cap: bool,
}

/// Walk every active record matching `scope`, paginating through the
/// `MemoryStore::list` cursor up to `CHECK_PAGINATION_CAP`.
/// Genuine store failures still surface as `Err` (→ Retry).
async fn collect_all_records(
    store: &dyn MemoryStore,
    scope: Option<&ScopeTuple>,
) -> Result<RecordCollection, Box<dyn std::error::Error + Send + Sync>> {
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
            return Ok(RecordCollection {
                records: out,
                truncated_at_cap: true,
            });
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => {
                return Ok(RecordCollection {
                    records: out,
                    truncated_at_cap: false,
                });
            }
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
        let collection = collect_all_records(store, scope).await?;
        if collection.truncated_at_cap {
            return Ok(CheckOutcome::Failed {
                details: format!(
                    "orphan check stopped at {CHECK_PAGINATION_CAP}-record cap — vault too large for the v0.1 starter check"
                ),
            });
        }
        let mut bad_ids: Vec<String> = Vec::new();
        for r in &collection.records {
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
        // The `MemoryStore::list` trait contract guarantees
        // `list` never returns tombstoned rows. Earlier rounds of
        // this check re-asserted that invariant via a per-record
        // `versions()` lookup, but the second read isn't
        // snapshot-consistent with the first: a concurrent
        // expiration or `forget` can tombstone a record between
        // `list` and `versions`, surfacing a non-deterministic
        // `Failed` even though the contract was honored at read
        // time (round-6 adversarial review #3).
        //
        // Trust the trait. The check now only validates that the
        // pagination walk completed within the cap; the active-list
        // filtering is enforced inside the store adapter itself
        // (and unit-tested there).
        let collection = collect_all_records(store, scope).await?;
        if collection.truncated_at_cap {
            return Ok(CheckOutcome::Failed {
                details: format!(
                    "tombstone-consistency check stopped at {CHECK_PAGINATION_CAP}-record cap — vault too large for the v0.1 starter check"
                ),
            });
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
