//! [`ScopePolicy`] — vault-anchored allow-list for envelope `scope`
//! checking. Constructed once at adapter startup; passed by reference
//! into every [`super::EnvelopeVerifier`] instance.
//!
//! P0: `(tenant, workspace)` come from a hard-coded adapter default
//! (`tenant = "default"`, `workspace = vault.name`) until a follow-up
//! issue extends [`crate::config::VaultConfig`] with explicit fields.
//! `allowed_tiers` defaults to all six tiers; verbs may narrow later.

use std::collections::HashSet;

use crate::domain::DomainError;
use crate::generated::envelope::SignedIntentScopeTier;

/// Vault-level allow-list for envelope scope dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopePolicy {
    /// Tenant string the vault accepts. Compared exactly.
    pub tenant: String,
    /// Workspace string the vault accepts. Compared exactly.
    pub workspace: String,
    /// Tiers the vault accepts. Subset of [`SignedIntentScopeTier`].
    pub allowed_tiers: HashSet<SignedIntentScopeTier>,
}

impl ScopePolicy {
    /// Construct a policy with explicit tenant/workspace strings and an
    /// allow-list of tiers.
    ///
    /// # Errors
    /// Returns [`DomainError::ScopeDenied`] if `tenant` or `workspace` is
    /// empty, or if `allowed_tiers` is empty.
    pub fn new(
        tenant: impl Into<String>,
        workspace: impl Into<String>,
        allowed_tiers: HashSet<SignedIntentScopeTier>,
    ) -> Result<Self, DomainError> {
        let tenant = tenant.into();
        if tenant.is_empty() {
            return Err(DomainError::ScopeDenied {
                message: "tenant must not be empty".into(),
            });
        }
        let workspace = workspace.into();
        if workspace.is_empty() {
            return Err(DomainError::ScopeDenied {
                message: "workspace must not be empty".into(),
            });
        }
        if allowed_tiers.is_empty() {
            return Err(DomainError::ScopeDenied {
                message: "allowed_tiers must not be empty".into(),
            });
        }
        Ok(Self {
            tenant,
            workspace,
            allowed_tiers,
        })
    }

    /// All-tiers allow-list useful at P0 and in tests.
    #[must_use]
    pub fn all_tiers() -> HashSet<SignedIntentScopeTier> {
        let mut s = HashSet::new();
        s.insert(SignedIntentScopeTier::Private);
        s.insert(SignedIntentScopeTier::Session);
        s.insert(SignedIntentScopeTier::Project);
        s.insert(SignedIntentScopeTier::Team);
        s.insert(SignedIntentScopeTier::Org);
        s.insert(SignedIntentScopeTier::Public);
        s
    }
}
