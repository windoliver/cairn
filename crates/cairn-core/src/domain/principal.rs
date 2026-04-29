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
    /// Verified session id for `session`-tier authorization. Must be set
    /// out-of-band by the verb layer from a trusted session token; the
    /// store treats it as authoritative.
    #[serde(default)]
    session_id: Option<String>,
    /// Verified project id for `project`-tier authorization.
    #[serde(default)]
    project_id: Option<String>,
    /// Verified team memberships for `team`-tier authorization. Each
    /// entry must be a team id the verb layer has cryptographically
    /// confirmed the principal belongs to.
    #[serde(default)]
    team_ids: Vec<String>,
    /// Verified org memberships for `org`-tier authorization.
    #[serde(default)]
    org_ids: Vec<String>,
}

impl Principal {
    /// Construct a normal interactive principal from a verified identity.
    #[must_use]
    pub fn from_identity(identity: Identity) -> Self {
        Self {
            identity: Some(identity),
            is_system: false,
            session_id: None,
            project_id: None,
            team_ids: Vec::new(),
            org_ids: Vec::new(),
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
            session_id: None,
            project_id: None,
            team_ids: Vec::new(),
            org_ids: Vec::new(),
        }
    }

    /// Attach verified session/project/team/org context. Each setter
    /// trusts its caller — the verb layer must validate against a
    /// session token or membership service before forwarding into
    /// the store. Returning `Self` keeps construction fluent.
    #[must_use]
    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Attach a verified project id.
    #[must_use]
    pub fn with_project(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }

    /// Attach verified team memberships.
    #[must_use]
    pub fn with_teams<I, S>(mut self, team_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.team_ids = team_ids.into_iter().map(Into::into).collect();
        self
    }

    /// Attach verified org memberships.
    #[must_use]
    pub fn with_orgs<I, S>(mut self, org_ids: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.org_ids = org_ids.into_iter().map(Into::into).collect();
        self
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

    /// Verified session id, if any.
    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Verified project id, if any.
    #[must_use]
    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    /// Verified team memberships.
    #[must_use]
    pub fn team_ids(&self) -> &[String] {
        &self.team_ids
    }

    /// Verified org memberships.
    #[must_use]
    pub fn org_ids(&self) -> &[String] {
        &self.org_ids
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
        let forged = r#"{"identity":null,"is_system":true,"session_id":null,"project_id":null,"team_ids":[],"org_ids":[]}"#;
        let p: Principal = serde_json::from_str(forged).expect("deserializes");
        assert!(!p.is_system(), "system bit must be ignored on deserialize");
    }
}
