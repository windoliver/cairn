//! Typed lock taxonomy for §5.6.
//!
//! Brief defines two scopes (Entity, Session). Cairn extends with Vault for
//! admin/maintenance ops (lint --fix-markdown, reindex, schema migration)
//! that serialize against each other but not against per-entity writes.

use std::fmt;

/// Lock scope axis. Brief §5.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LockScope {
    /// `(tenant, workspace, entity_id)` — every write acquires Exclusive.
    Entity,
    /// `(tenant, workspace, session:<id>)` — write-with-session acquires Shared;
    /// `forget_session` acquires Exclusive for full Phase A.
    Session,
    /// `(tenant, workspace)` — vault-wide admin ops (lint, reindex, migrations).
    /// Cairn extension to brief §5.6; brief is silent on admin lock scope.
    Vault,
}

impl fmt::Display for LockScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Entity => "entity",
            Self::Session => "session",
            Self::Vault => "vault",
        })
    }
}

/// Lock mode axis. Brief §5.6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LockMode {
    /// Multiple Shared holders coexist; blocked by Exclusive incumbent.
    Shared,
    /// Single Exclusive holder; blocks all other modes.
    Exclusive,
}

impl LockMode {
    /// Wire format used in `lock_holders.mode_requested` and the existing
    /// `0004_locks` compatibility triggers.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Shared => "SHARED",
            Self::Exclusive => "EXCLUSIVE",
        }
    }
}

impl fmt::Display for LockMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_db_str())
    }
}

/// Identifies one lockable resource (scope + key).
///
/// Construct via the typed builders (`entity`, `session`, `vault`) — never
/// hand-build, so resource serialization stays canonical across crates.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceKey {
    scope: LockScope,
    key: String,
}

impl ResourceKey {
    /// Per-entity lock. `entity_id` is opaque (record id, target id, etc.).
    #[must_use]
    pub fn entity(tenant: &str, workspace: &str, entity_id: &str) -> Self {
        Self {
            scope: LockScope::Entity,
            key: format!("{tenant}:{workspace}:{entity_id}"),
        }
    }

    /// Per-session lock.
    #[must_use]
    pub fn session(tenant: &str, workspace: &str, session_id: &str) -> Self {
        Self {
            scope: LockScope::Session,
            key: format!("{tenant}:{workspace}:{session_id}"),
        }
    }

    /// Vault-wide lock. `vault_id` is the canonical vault path id used by
    /// `cairn-cli::verbs::lint::vault_id`.
    #[must_use]
    pub fn vault(vault_id: &str) -> Self {
        Self {
            scope: LockScope::Vault,
            key: vault_id.to_owned(),
        }
    }

    /// Scope axis.
    #[must_use]
    pub fn scope(&self) -> LockScope {
        self.scope
    }

    /// Stable string serialization stored in `lock_holders.resource`.
    /// Format: `"{scope}:{key}"` — e.g. `"entity:t1:default:rec_abc"`.
    #[must_use]
    pub fn as_resource_str(&self) -> String {
        format!("{}:{}", self.scope, self.key)
    }
}

impl fmt::Display for ResourceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_resource_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_resource_str_is_stable() {
        let r = ResourceKey::entity("t1", "default", "rec_abc");
        assert_eq!(r.as_resource_str(), "entity:t1:default:rec_abc");
        assert_eq!(r.scope(), LockScope::Entity);
    }

    #[test]
    fn session_resource_str_is_stable() {
        let r = ResourceKey::session("t1", "default", "sess_42");
        assert_eq!(r.as_resource_str(), "session:t1:default:sess_42");
    }

    #[test]
    fn vault_resource_str_is_stable() {
        let r = ResourceKey::vault("vault_xyz");
        assert_eq!(r.as_resource_str(), "vault:vault_xyz");
    }

    #[test]
    fn lock_mode_db_str_matches_trigger_expectation() {
        // 0004_locks triggers branch on the literal strings 'SHARED' / 'EXCLUSIVE'.
        assert_eq!(LockMode::Shared.as_db_str(), "SHARED");
        assert_eq!(LockMode::Exclusive.as_db_str(), "EXCLUSIVE");
    }
}
