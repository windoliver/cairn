//! Atomic `(consent event + propagation job)` write surface for federation
//! verbs (brief §12.a, §14).
//!
//! Used by [`crate::verbs::propose_share`] and (later in the issue #123
//! sequence) [`crate::verbs::revoke_share`] / [`crate::verbs::accept_share`]
//! to honour the brief's "WAL + two-phase apply for every mutation"
//! invariant: a successful federation grant *must* persist a
//! [`ConsentEvent`] AND enqueue an outbound [`EnqueueRequest`] in the same
//! transaction, or neither.
//!
//! `cairn-core` cannot depend on the `SQLite` tx primitive (the
//! workspace dependency rule). This trait is therefore the narrow
//! contract that the verb layer sees. The `SQLite` adapter implements it
//! by composing `consent::append_in_tx` + a jobs-table insert inside
//! one `SqliteMemoryStore::with_tx` closure (wired in task T13 of
//! issue #123). Test fixtures implement it with a single mutex-guarded
//! `Vec` that atomically captures both writes.

use async_trait::async_trait;
use thiserror::Error;

use crate::contract::job_store::EnqueueRequest;
use crate::domain::{ConsentEvent, MemoryRecord};

/// Failures raised by [`FederationOutbox`] implementations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FederationOutboxError {
    /// `(kind, dedupe_key)` already exists in the job table.
    ///
    /// Surfaces the underlying
    /// [`crate::contract::job_store::JobStoreError::DuplicateDedupeKey`]
    /// without leaking the adapter-specific error type, because the
    /// verb's retry semantics are decoupled from the persistence layer.
    #[error("duplicate propagation job dedupe_key")]
    DuplicateJob,

    /// Backend rejected the consent event (validation failure).
    ///
    /// Carries an opaque body-free message — never raw user content,
    /// matching the brief §14 consent-journal posture.
    #[error("consent event rejected: {message}")]
    ConsentRejected {
        /// Body-free reason. Adapters must not include record bodies.
        message: String,
    },

    /// Backend I/O failure (``SQLite`` lock contention, disk failure, …).
    /// Adapter-supplied source error preserves the chain for tracing.
    #[error("federation outbox backend failure")]
    Backend {
        /// Source error from the adapter — never raw user content.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Atomic write surface for federation outbound state.
///
/// Implementations MUST treat the two side effects as a single
/// transaction: either both rows land on disk or neither does. A partial
/// write (consent event without an outbound job, or vice versa) would
/// strand the share in an audit-only state with no follow-up
/// propagation — exactly the failure mode the brief's WAL invariant
/// forbids.
#[async_trait]
pub trait FederationOutbox: Send + Sync {
    /// Atomically append `event` to the consent journal AND enqueue
    /// `job` on the workflow queue.
    ///
    /// # Errors
    ///
    /// * [`FederationOutboxError::DuplicateJob`] when the propagation
    ///   job's `(kind, dedupe_key)` already exists. The verb caller
    ///   treats this as an idempotent replay.
    /// * [`FederationOutboxError::ConsentRejected`] when the consent
    ///   event itself fails adapter-side validation (a defence-in-depth
    ///   line against verb-layer drift).
    /// * [`FederationOutboxError::Backend`] for opaque adapter I/O
    ///   failures.
    async fn record_share_grant(
        &self,
        event: &ConsentEvent,
        job: EnqueueRequest,
    ) -> Result<(), FederationOutboxError>;

    /// Atomically append `event` to the consent journal AND upsert each
    /// inbound record in `upserts` (receiver-side apply path).
    ///
    /// Implementations MUST treat all writes as a single transaction —
    /// the upserts and the [`ConsentEvent::FederationAccept`] row land
    /// together or not at all. A partial apply would orphan inbound
    /// records without an audit trail, exactly the failure mode the
    /// brief's WAL invariant forbids.
    ///
    /// # Errors
    ///
    /// * [`FederationOutboxError::ConsentRejected`] when the consent
    ///   event fails adapter-side validation.
    /// * [`FederationOutboxError::Backend`] for opaque adapter I/O
    ///   failures (record upsert collisions, disk failures, …).
    async fn record_share_accept(
        &self,
        event: &ConsentEvent,
        upserts: &[MemoryRecord],
    ) -> Result<(), FederationOutboxError>;

    /// Atomically append `event` to the consent journal AND tombstone
    /// each record id in `tombstone_ids` (receiver-side revoke path).
    ///
    /// Implementations MUST treat all writes as a single transaction.
    /// `tombstone_ids` may be empty when no prior records were applied
    /// under the revoked link; the consent event still lands so the
    /// audit trail records the revoke decision.
    ///
    /// # Errors
    ///
    /// * [`FederationOutboxError::ConsentRejected`] when the consent
    ///   event fails adapter-side validation.
    /// * [`FederationOutboxError::Backend`] for opaque adapter I/O
    ///   failures.
    async fn record_share_revoke(
        &self,
        event: &ConsentEvent,
        tombstone_ids: &[String],
    ) -> Result<(), FederationOutboxError>;
}
