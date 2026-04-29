//! Caller principal type for rebac-gated store reads (brief §4 row 1,
//! lines 2557/3287/4136).
//!
//! A `Principal` represents the calling identity that drives per-row
//! visibility decisions at the `MemoryStore` layer. The full `ReBAC` rule
//! set lives in `cairn-core::rebac` (separate issue); this module only
//! defines the type and the system-principal sentinel used by the WAL
//! executor and tests.

use serde::{Deserialize, Serialize};

use crate::domain::identity::Identity;

/// A resolved caller identity presented to store read methods.
///
/// Store methods gate every row against this principal; rows the
/// principal cannot read are dropped before the result is returned
/// (brief lines 2557/3287/4136 mandate "non-readable rows never surface").
///
/// Two construction paths:
/// - [`Principal::from_identity`] — normal interactive callers identified
///   by a verified [`Identity`].
/// - [`Principal::system`] — privileged WAL-executor sentinel that bypasses
///   scope filtering (brief line 1361 flags these reads with `trust:
///   "unverified"` in the response envelope; the store passes the mode
///   through unchanged; the verb layer surfaces the trust marker).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    /// The underlying identity, if any. `None` for the system sentinel.
    identity: Option<Identity>,
    /// Whether this principal bypasses rebac filtering.
    ///
    /// Deserialization always forces this to `false`: the system sentinel
    /// must only be mintable in-process via [`Principal::system`], which
    /// requires an [`ApplyToken`](crate::wal::ApplyToken). Allowing
    /// `is_system: true` on the wire would be a trivial REBAC bypass.
    #[serde(skip_deserializing)]
    is_system: bool,
}

impl Principal {
    /// Construct a normal interactive principal from a verified identity.
    #[must_use]
    pub fn from_identity(identity: Identity) -> Self {
        Self {
            identity: Some(identity),
            is_system: false,
        }
    }

    /// Privileged system principal. Bypasses rebac scope filtering.
    ///
    /// Construction requires an [`ApplyToken`](crate::wal::ApplyToken),
    /// which only `cairn_core::wal` can mint (and `test_apply_token`
    /// behind `cfg(test)`/`feature = "test-util"`). User-facing code
    /// paths cannot fabricate one, preventing in-process callers from
    /// bypassing rebac.
    #[must_use]
    pub fn system(_token: &crate::wal::ApplyToken) -> Self {
        Self {
            identity: None,
            is_system: true,
        }
    }

    /// Whether this is the WAL-executor system sentinel.
    #[must_use]
    pub fn is_system(&self) -> bool {
        self.is_system
    }

    /// The underlying identity, if not the system sentinel.
    #[must_use]
    pub fn identity(&self) -> Option<&Identity> {
        self.identity.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_is_privileged() {
        let p = Principal::system(&crate::wal::test_apply_token());
        assert!(p.is_system());
        assert!(p.identity().is_none());
    }

    #[test]
    fn from_identity_not_system() {
        let id = Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid");
        let p = Principal::from_identity(id);
        assert!(!p.is_system());
        assert!(p.identity().is_some());
    }

    #[test]
    fn deserialize_cannot_forge_system_principal() {
        // Regression: an attacker-controlled JSON payload that sets
        // `is_system: true` must not produce a privileged principal,
        // because `principal_can_read` short-circuits on `is_system()`
        // and would otherwise bypass all rebac scope filtering.
        let forged = r#"{"identity":null,"is_system":true}"#;
        let p: Principal = serde_json::from_str(forged).expect("deserializes");
        assert!(!p.is_system(), "system bit must be ignored on deserialize");
    }
}
