//! `ConsentLookup` contract — read-only access to the consent timeline.
//!
//! Brief §14, Issue #253. Adapter implementations live in
//! `cairn-store-sqlite::consent_timeline`. Used by the lint `consent`
//! sub-check matrix to resolve the covering grant for a record's
//! `provenance.consent_ref`.
//!
//! Object-safe (dyn-compatible) so verb-layer code can pass a
//! `&dyn ConsentLookup` through `LintInputs` without leaking adapter
//! types into core.

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::consent_timeline::{ConsentTimelineEvent, CoveringGrant};
use crate::domain::federation::{DedupKey, SignedRevocation};
use crate::domain::sharing::ShareLinkPayload;
use crate::domain::{Rfc3339Timestamp, SensorLabel};

/// Errors raised by [`ConsentLookup`] implementations.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConsentLookupError {
    /// Underlying store I/O failure (DB unavailable, connection lost, …).
    /// Adapter-supplied source error preserves the chain for tracing /
    /// `Display`-of-cause; verb-layer code surfaces a redacted summary.
    #[error("consent lookup backend error")]
    Backend {
        /// Source error from the adapter — never raw user content.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Prior receiver-side `accept_share` apply, surfaced for idempotent
/// replay (brief §12.a). The journal lookup returns the original
/// `applied_records` so a duplicate envelope can reproduce the first
/// reply byte-for-byte without re-running the apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FederationAcceptRecord {
    /// Stable share-link id of the previously applied envelope.
    pub link_id: String,
    /// Record ids the original apply upserted, in commit order.
    pub applied_records: Vec<String>,
}

/// Persisted share-link grant looked up by `revoke_share` (brief §12.a).
///
/// `cairn-core` only needs the issuer's own original payload + the
/// outbound peer (when one was recorded on the propose row) to rebuild
/// the revocation envelope. Adapter implementations source this from the
/// consent journal projection materialised by T12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredShareLink {
    /// Stable share-link id (`share-<operation_id>`).
    pub link_id: String,
    /// Body-free original share-link payload — every field the verb
    /// needs to construct the revocation lives here (`operation_id` +
    /// issuer `key_version`, plus `issuer` for body-free defence in
    /// depth).
    pub payload: ShareLinkPayload,
    /// Symbolic outbound peer captured on the original propose row, if
    /// any. The revocation propagation job inherits this so the
    /// transport routes the revoke to the same destination.
    pub peer: Option<String>,
}

/// Persisted revocation looked up by `revoke_share` (brief §12.a).
///
/// Returned by [`ConsentLookup::find_revocation`] when a previous call
/// already committed a [`crate::domain::ConsentKind::FederationRevoke`]
/// row for the requested link. The verb replays the same
/// `operation_id` + signed revocation back to the caller so retries are
/// byte-for-byte idempotent.
///
/// `Eq` is intentionally not derived: the inner [`SignedRevocation`] is
/// a codegenerated `PartialEq`-only wire type (it carries a `String`
/// signature; structural `Eq` over the wire form is not part of the
/// contract).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRevocation {
    /// WAL operation id of the original revoke.
    pub operation_id: String,
    /// The signed revocation that was emitted on first commit.
    pub revocation: SignedRevocation,
}

/// Read-only access to the `consent_timeline`. Implementations must be
/// safe to call from the verb layer (no hidden global state, no panics).
#[async_trait]
pub trait ConsentLookup: Send + Sync {
    /// Return the full event list for `consent_ref`, in any order.
    /// Empty vec when the ref is unknown.
    ///
    /// # Errors
    /// Returns [`ConsentLookupError::Backend`] on adapter I/O failure.
    async fn timeline(
        &self,
        consent_ref: &str,
    ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError>;

    /// Resolve the covering grant for `(sensor, scope)` at instant `at`.
    /// Default impl walks `timeline(consent_ref)` and delegates to
    /// [`CoveringGrant::resolve`]. Adapters with a one-shot SQL query
    /// should override.
    ///
    /// # Errors
    /// Propagates [`ConsentLookupError`] from `timeline`.
    async fn covering_grant(
        &self,
        consent_ref: &str,
        sensor: &SensorLabel,
        scope: &str,
        at: &Rfc3339Timestamp,
    ) -> Result<Option<CoveringGrant>, ConsentLookupError> {
        let events = self.timeline(consent_ref).await?;
        Ok(CoveringGrant::resolve(&events, sensor, scope, at))
    }

    /// Idempotency lookup for `accept_share` (brief §12.a).
    ///
    /// Returns `Some(record)` when a [`crate::domain::ConsentKind::FederationAccept`]
    /// row has already been committed for the `(issuer_key_id, link_id)`
    /// tuple in `dedup`. Returns `None` for a first-seen envelope.
    ///
    /// The `link_id` is already nonce-bound (each propose mints a fresh
    /// `link_id` + nonce together), so the nonce is not needed as a
    /// separate dedup component. `FederationAccept` consent rows do not
    /// persist the nonce, making nonce-based dedup unimplementable.
    ///
    /// The verb layer treats a hit as a duplicate Ack: it replies with
    /// the original `applied_records` and emits no new consent event.
    ///
    /// The default impl returns `None` so a runtime that does not yet
    /// wire the federation timeline projection (T12) advertises no
    /// dedup state — the capability gate keeps `accept_share` from
    /// being called against such a runtime in production.
    ///
    /// # Errors
    /// Returns [`ConsentLookupError::Backend`] on adapter I/O failure.
    async fn find_federation_accept(
        &self,
        _dedup: DedupKey<'_>,
    ) -> Result<Option<FederationAcceptRecord>, ConsentLookupError> {
        Ok(None)
    }

    /// Revocation check for `accept_share` (brief §12.a).
    ///
    /// Returns `true` when a [`crate::domain::ConsentKind::FederationRevoke`]
    /// row has been committed for `link_id`. The verb layer rejects a
    /// subsequent propose envelope as
    /// [`crate::error::federation::FederationError::Revoked`].
    ///
    /// The default impl returns `false` so a runtime that does not yet
    /// wire the federation timeline projection (T12) does not spuriously
    /// reject inbound envelopes.
    ///
    /// # Errors
    /// Returns [`ConsentLookupError::Backend`] on adapter I/O failure.
    async fn is_link_revoked(&self, _link_id: &str) -> Result<bool, ConsentLookupError> {
        Ok(false)
    }

    /// Original share-link lookup for `revoke_share` (brief §12.a).
    ///
    /// Returns `Some(link)` when the issuer-side consent journal still
    /// holds a [`crate::domain::ConsentKind::FederationGrant`] row for
    /// `link_id`. The verb layer treats a `None` reply as
    /// [`crate::error::federation::FederationError::UnknownLink`] — the
    /// caller asked to revoke a link the runtime never minted.
    ///
    /// The default impl returns `None` so a runtime that does not yet
    /// wire the federation timeline projection (T12) cannot accidentally
    /// satisfy a revoke against a fake link id.
    ///
    /// # Errors
    /// Returns [`ConsentLookupError::Backend`] on adapter I/O failure.
    async fn find_share_link(
        &self,
        _link_id: &str,
    ) -> Result<Option<StoredShareLink>, ConsentLookupError> {
        Ok(None)
    }

    /// Idempotency lookup for `revoke_share` (brief §12.a).
    ///
    /// Returns `Some(record)` when a prior call already committed a
    /// [`crate::domain::ConsentKind::FederationRevoke`] row for
    /// `link_id`. The verb replays the original `operation_id` and
    /// [`SignedRevocation`] so retries are byte-for-byte stable.
    ///
    /// The default impl returns `None` for the same wiring reason as
    /// [`Self::find_share_link`].
    ///
    /// # Errors
    /// Returns [`ConsentLookupError::Backend`] on adapter I/O failure.
    async fn find_revocation(
        &self,
        _link_id: &str,
    ) -> Result<Option<StoredRevocation>, ConsentLookupError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::consent_timeline::ConsentTimelineEventKind;
    use std::collections::HashMap;

    struct StaticLookup {
        by_ref: HashMap<String, Vec<ConsentTimelineEvent>>,
    }

    #[async_trait]
    impl ConsentLookup for StaticLookup {
        async fn timeline(
            &self,
            consent_ref: &str,
        ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError> {
            Ok(self.by_ref.get(consent_ref).cloned().unwrap_or_default())
        }
    }

    fn ev(consent_ref: &str, sensor: &str, scope: &str) -> ConsentTimelineEvent {
        ConsentTimelineEvent {
            consent_ref: consent_ref.to_owned(),
            seq: 1,
            kind: ConsentTimelineEventKind::Issued,
            sensor_id: SensorLabel::parse(sensor).expect("invariant: valid sensor label"),
            scope: scope.to_owned(),
            decided_at: Rfc3339Timestamp::parse("2026-01-01T00:00:00Z")
                .expect("invariant: valid timestamp"),
            expires_at: None,
        }
    }

    #[tokio::test]
    async fn default_covering_grant_delegates_to_timeline() {
        let mut by_ref = HashMap::new();
        by_ref.insert(
            "c:1".to_owned(),
            vec![ev("c:1", "local:screen:h:v1", "private")],
        );
        let lk = StaticLookup { by_ref };

        let g = lk
            .covering_grant(
                "c:1",
                &SensorLabel::parse("local:screen:h:v1").expect("invariant: valid label"),
                "private",
                &Rfc3339Timestamp::parse("2026-06-01T00:00:00Z").expect("invariant: valid ts"),
            )
            .await
            .expect("invariant: lookup succeeds");
        assert!(g.is_some());
    }

    #[tokio::test]
    async fn returns_none_when_consent_ref_unknown() {
        let lk = StaticLookup {
            by_ref: HashMap::new(),
        };
        let g = lk
            .covering_grant(
                "c:missing",
                &SensorLabel::parse("local:screen:h:v1").expect("invariant: valid label"),
                "private",
                &Rfc3339Timestamp::parse("2026-06-01T00:00:00Z").expect("invariant: valid ts"),
            )
            .await
            .expect("invariant: lookup succeeds");
        assert!(g.is_none());
    }

    #[test]
    fn trait_is_object_safe() {
        // Compiles iff `ConsentLookup` is dyn-compatible.
        fn _accept(_: &dyn ConsentLookup) {}
    }

    #[tokio::test]
    async fn default_covering_grant_propagates_backend_error_from_timeline() {
        struct FailingLookup;

        #[async_trait]
        impl ConsentLookup for FailingLookup {
            async fn timeline(
                &self,
                _consent_ref: &str,
            ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError> {
                Err(ConsentLookupError::Backend {
                    source: "synthetic adapter failure".into(),
                })
            }
        }

        let err = FailingLookup
            .covering_grant(
                "c:any",
                &SensorLabel::parse("local:s:h:v1").expect("invariant: valid sensor"),
                "private",
                &Rfc3339Timestamp::parse("2026-06-01T00:00:00Z")
                    .expect("invariant: valid timestamp"),
            )
            .await
            .expect_err("must propagate backend error");

        assert!(matches!(err, ConsentLookupError::Backend { .. }));
    }
}
