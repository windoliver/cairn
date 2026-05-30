//! Admin caller context for `cairn.admin.v1` verbs.

use crate::domain::Identity;

/// The role a caller asserts when invoking an admin verb.
///
/// `#[non_exhaustive]` — future roles (e.g. `Auditor`) will be added
/// without breaking existing match arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AdminRole {
    /// Full operator privileges: can read and mutate admin state.
    Operator,
}

/// Authenticated caller context threaded through every `cairn.admin.v1` verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminContext {
    /// The verified identity of the calling principal.
    pub actor: Identity,
    /// The role the caller is asserting for this invocation.
    pub requested_role: AdminRole,
}

impl AdminContext {
    /// Construct an [`AdminContext`] from a verified identity and role.
    #[must_use]
    pub fn new(actor: Identity, requested_role: AdminRole) -> Self {
        Self {
            actor,
            requested_role,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_context_constructor_roundtrip() {
        // `Identity::parse` is fallible; `.expect` is intentional in tests.
        #[allow(clippy::expect_used)]
        let actor = Identity::parse("hmn:test").expect("test fixture: known-valid identity");
        let ctx = AdminContext::new(actor.clone(), AdminRole::Operator);
        assert_eq!(ctx.actor, actor);
        assert_eq!(ctx.requested_role, AdminRole::Operator);
    }
}
