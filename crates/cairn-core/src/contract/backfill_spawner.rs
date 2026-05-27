//! Adapter trait for spawning a connector backfill workflow.
//!
//! `verbs::admin::connector::backfill` is the outer-layer verb; the
//! actual scheduler / handler / persistence lives behind this trait.
//! v0.2 ships the trait shape with no in-tree impl — a follow-up issue
//! adds the scheduler-side wiring + `backfill_jobs` migration.
//!
//! Brief §5 (pipeline) / §10 (workflows) / §19 (connector management).

use chrono::{DateTime, Utc};

use crate::contract::memory_store::StoreError;
use crate::domain::Identity;
use crate::domain::workflow::WorkflowId;

/// Full specification of a requested backfill, passed to [`BackfillSpawner::spawn_backfill`].
///
/// The spawner persists this and begins the workflow asynchronously.
#[derive(Debug, Clone)]
pub struct BackfillSpec {
    /// Unique identifier minted by the verb before calling the spawner.
    pub workflow_id: WorkflowId,
    /// Stable connector name (e.g. `"github"`, `"linear"`).
    pub connector_name: String,
    /// Inclusive start of the backfill time window (UTC).
    pub from: DateTime<Utc>,
    /// Inclusive end of the backfill time window (UTC).
    pub to: DateTime<Utc>,
    /// Maximum ingest rate the scheduler should honour (items per second).
    pub rate_per_sec: f64,
    /// Wall-clock time the verb accepted the request.
    pub started_at: DateTime<Utc>,
    /// Operator identity that initiated the backfill (for audit log).
    pub actor: Identity,
}

/// Adapter contract for persisting and starting a backfill workflow.
///
/// The verb calls `spawn_backfill` and returns immediately to the caller
/// with the [`WorkflowId`]; progress is reported asynchronously through
/// a [`ProgressEvent`] stream owned by the adapter.
///
/// [`ProgressEvent`]: crate::domain::workflow::ProgressEvent
pub trait BackfillSpawner: Send + Sync {
    /// Persist `spec` and enqueue the backfill workflow for async execution.
    ///
    /// Returns immediately; the adapter is responsible for emitting
    /// [`ProgressEvent`]s as the workflow runs.
    ///
    /// [`ProgressEvent`]: crate::domain::workflow::ProgressEvent
    ///
    /// # Errors
    /// Returns a `StoreError` wrapping the underlying persistence failure.
    /// Returns `StoreError` wrapping a `"connector_not_registered"` message if
    /// the adapter does not recognise the named connector.
    fn spawn_backfill(&self, spec: BackfillSpec) -> Result<(), StoreError>;
}
