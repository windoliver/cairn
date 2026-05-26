//! Shared identity guard used by every admin verb.

use crate::contract::admin_state::AdminStateStore;
use crate::domain::admin::{AdminContext, AdminError, AdminRole};

/// Verify that the caller holds `needed` role, returning [`AdminError::NotAuthorized`]
/// if not.
///
/// # Errors
/// - [`AdminError::NotAuthorized`] — caller does not hold `needed`.
/// - [`AdminError::Store`] — the `has_role` adapter call failed.
pub fn require_role(
    ctx: &AdminContext,
    admin: &dyn AdminStateStore,
    needed: AdminRole,
) -> Result<(), AdminError> {
    if !admin
        .has_role(&ctx.actor, needed)
        .map_err(AdminError::Store)?
    {
        return Err(AdminError::NotAuthorized {
            actor: ctx.actor.clone(),
            needed,
        });
    }
    Ok(())
}
