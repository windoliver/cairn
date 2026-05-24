//! `ConnectorConsentJournal` — write + lookup access to the
//! `connector_consent` portion of the consent journal. The framework
//! in `cairn-connectors-core` calls this on every emit; persistent
//! impls land alongside `cairn-store-sqlite` work in a follow-up.
//!
//! Issue #130, brief §14 + §19 v0.3.
//!
//! The read-only `ConsentJournalReader` (issue #257) and the existing
//! `ConsentLookup` trait remain distinct; their consumers are
//! forget-scoped and federation-scoped respectively. This trait is
//! connector-scoped and write-capable.

use std::collections::BTreeSet;

use crate::domain::Identity;

/// Opaque consent-grant identifier. Stable across process restarts;
/// formatted as `gnt:<connector>:<ulid>` by the persistent impl.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConsentGrantId(String);

impl ConsentGrantId {
    /// Create a new `ConsentGrantId` from any string value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the grant id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ConsentGrantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Result of a [`ConnectorConsentJournal::lookup`] call.
///
/// `Revoked` is the closed-fail default: if the journal has no live
/// grant for `(connector, scope_key)` the framework rejects the emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorConsentLookup {
    /// A live consent grant exists for the queried connector + scope.
    Granted,
    /// No live grant exists; the connector framework must reject the emit.
    Revoked,
}

/// Persistent grant record (brief §14). The framework writes this
/// when `ConnectorRegistry::enable` runs; the consent journal stores
/// it; `lookup` resolves against it on every emit.
///
/// `#[non_exhaustive]` allows adding fields in future versions without
/// a breaking change. Use [`ConsentGrant::new`] to construct values.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConsentGrant {
    /// Connector name this grant covers (matches `ConnectorManifest::name`).
    pub connector: String,
    /// SHA-256 hash of the connector manifest at grant time (brief §14).
    /// Drift from this value causes the framework to surface
    /// `ConnectorError::ConsentRevoked`.
    pub manifest_hash: String,
    /// Labels this grant permits the connector to emit.
    pub allowed_labels: BTreeSet<String>,
    /// Scope glob patterns the grant is valid for (e.g. `"project:*"`).
    pub scope_patterns: Vec<String>,
    /// Grant timestamp as Unix seconds (UTC).
    pub granted_at: i64,
    /// Identity of the human or agent that authorised this grant.
    pub grantor: Identity,
}

impl ConsentGrant {
    /// Construct a new `ConsentGrant`. This constructor exists because
    /// `#[non_exhaustive]` prevents struct-literal construction outside
    /// the defining crate; callers should use this method.
    #[must_use]
    pub fn new(
        connector: impl Into<String>,
        manifest_hash: impl Into<String>,
        allowed_labels: BTreeSet<String>,
        scope_patterns: Vec<String>,
        granted_at: i64,
        grantor: Identity,
    ) -> Self {
        Self {
            connector: connector.into(),
            manifest_hash: manifest_hash.into(),
            allowed_labels,
            scope_patterns,
            granted_at,
            grantor,
        }
    }
}

/// Write + lookup surface needed by the connector framework (brief §14,
/// §19 v0.3). Distinct from `ConsentJournalReader` (forget-scoped) and
/// `ConsentLookup` (federation-scoped); consolidating later is possible
/// but out of scope for issue #130.
///
/// Implementations must be `Send + Sync` so the framework can share
/// them across async tasks in the registry. The trait is object-safe:
/// `Box<dyn ConnectorConsentJournal>` and
/// `Arc<dyn ConnectorConsentJournal>` both compile.
#[async_trait::async_trait]
pub trait ConnectorConsentJournal: Send + Sync {
    /// Persist a new consent grant and return its opaque identifier.
    ///
    /// The framework calls this during `ConnectorRegistry::enable`. The
    /// returned [`ConsentGrantId`] is stored in the registry entry for
    /// use by [`Self::revoke`].
    ///
    /// # Errors
    /// Returns `Err(String)` on backend I/O failure. The persistent
    /// implementation will refine this to a typed error in a follow-up.
    async fn put_grant(&self, grant: ConsentGrant) -> Result<ConsentGrantId, String>;

    /// Check whether a live consent grant exists for `(connector, scope_key)`.
    ///
    /// `scope_key` is the `"<kind>:<value>"` string produced by
    /// `ConnectorScope::lookup_key`. The implementation is responsible
    /// for matching `scope_key` against stored `scope_patterns`.
    ///
    /// Returns [`ConnectorConsentLookup::Revoked`] if no matching live
    /// grant is found (fail-closed).
    ///
    /// # Errors
    /// Returns `Err(String)` on backend I/O failure.
    async fn lookup(
        &self,
        connector: &str,
        scope_key: &str,
    ) -> Result<ConnectorConsentLookup, String>;

    /// Revoke an existing consent grant by id.
    ///
    /// After a successful revoke, subsequent `lookup` calls for the
    /// same `(connector, scope_key)` must return
    /// [`ConnectorConsentLookup::Revoked`].
    ///
    /// # Errors
    /// Returns `Err(String)` on backend I/O failure or if the id is
    /// unknown. The persistent implementation may choose to treat an
    /// unknown id as a no-op (idempotent revoke).
    async fn revoke(&self, id: &ConsentGrantId) -> Result<(), String>;
}
