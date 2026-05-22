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
use crate::domain::federation::DedupKey;
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
    /// row has already been committed for the
    /// `(issuer_key_id, link_id, nonce)` tuple in `dedup`. Returns
    /// `None` for a first-seen envelope.
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
