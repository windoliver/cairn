//! [`IssuerKeyResolver`] adapter wrapping any [`IdentityRegistry`] impl
//! (issue #51, brief §4.2).
//!
//! Maps the rich registry-side lifecycle states to the narrow set the
//! verifier consumes:
//!
//! | `ProvisioningState`        | [`KeyLifecycle`]                    |
//! |----------------------------|-------------------------------------|
//! | `Active`                   | `Active`                            |
//! | `Revoked`                  | `Revoked { effective_at }`          |
//! | `Pending`/`RevokePending`/ | `NonOperational`                    |
//! | `PurgePending`             |                                     |
//! | `Purged`                   | `Purged`                            |
//!
//! The verifier turns `NonOperational`/`Purged` into `UnknownKey` and
//! checks `Revoked.effective_at <= intent.issued_at` itself — this
//! adapter only owns the mapping.

use std::sync::Arc;

use async_trait::async_trait;

use cairn_core::contract::IdentityRegistry;
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::contract::issuer_key_resolver::{
    IssuerKeyResolver, KeyLifecycle, ResolvedKey, ResolverError,
};
use cairn_core::domain::identity::{Identity, keys::KeyVersion, records::ProvisioningState};
use cairn_core::domain::timestamp::Rfc3339Timestamp;

/// Resolver backed by an [`IdentityRegistry`]-implementing store.
pub struct SqliteIssuerKeyResolver<R: IdentityRegistry + 'static> {
    registry: Arc<R>,
}

impl<R: IdentityRegistry + 'static> SqliteIssuerKeyResolver<R> {
    /// Wrap `registry` in an [`IssuerKeyResolver`].
    #[must_use]
    pub fn new(registry: Arc<R>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl<R: IdentityRegistry + 'static> IssuerKeyResolver for SqliteIssuerKeyResolver<R> {
    async fn lookup(
        &self,
        issuer: &Identity,
        key_version: KeyVersion,
    ) -> Result<Option<ResolvedKey>, ResolverError> {
        let record = self
            .registry
            .get_identity(issuer, IdentityVisibility::Audit)
            .await
            .map_err(|e| ResolverError::Backend(Box::new(e)))?;
        let Some(record) = record else { return Ok(None) };

        let keys = self
            .registry
            .list_keys(issuer)
            .await
            .map_err(|e| ResolverError::Backend(Box::new(e)))?;
        let Some(entry) = keys.into_iter().find(|k| k.key_version == key_version) else {
            return Ok(None);
        };

        let lifecycle = match record.provisioning_state {
            ProvisioningState::Active => KeyLifecycle::Active,
            ProvisioningState::Revoked => {
                let effective_at = record
                    .revoked_at
                    .map(|t| t.to_rfc3339())
                    .unwrap_or_default();
                let effective_at = Rfc3339Timestamp::parse(effective_at)
                    .map_err(|e| ResolverError::Backend(Box::new(e)))?;
                KeyLifecycle::Revoked { effective_at }
            }
            ProvisioningState::Pending
            | ProvisioningState::RevokePending
            | ProvisioningState::PurgePending => KeyLifecycle::NonOperational,
            ProvisioningState::Purged => KeyLifecycle::Purged,
        };

        Ok(Some(ResolvedKey { public_key: entry.public_key, lifecycle }))
    }
}
