//! Scope tuple (brief §6, §4.2).
//!
//! Every record carries a scope tuple so the ranker, consolidator, promoter,
//! and expirer can branch on `(tenant, workspace, project, session, entity,
//! user, agent)`. Each field is optional individually, but at least one must
//! be set — an unscoped record cannot be retrieved or governed.

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// Seven-dimensional scope tuple. Empty strings are rejected; absent values
/// are encoded as `None`. Wire form omits `None` fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeTuple {
    /// Top-level tenant boundary (e.g., `acme`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// Workspace within the tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Project tree this record belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Session id (ULID) when the record is session-scoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Named entity (person, org, system) the record is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity: Option<String>,
    /// Human user id this record is attributed to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Agent identity that owns this record's lifecycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

impl ScopeTuple {
    /// Validate that at least one dimension is present and no present
    /// component is empty.
    pub fn validate(&self) -> Result<(), DomainError> {
        let any_present = self.tenant.is_some()
            || self.workspace.is_some()
            || self.project.is_some()
            || self.session.is_some()
            || self.entity.is_some()
            || self.user.is_some()
            || self.agent.is_some();
        if !any_present {
            return Err(DomainError::MalformedScope {
                message: "at least one of tenant, workspace, project, session, entity, user, agent is required".to_owned(),
            });
        }
        for (name, value) in [
            ("tenant", &self.tenant),
            ("workspace", &self.workspace),
            ("project", &self.project),
            ("session", &self.session),
            ("entity", &self.entity),
            ("user", &self.user),
            ("agent", &self.agent),
        ] {
            if let Some(v) = value
                && v.is_empty()
            {
                return Err(DomainError::MalformedScope {
                    message: format!("`{name}` must not be empty if present"),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_scope_rejected() {
        let err = ScopeTuple::default().validate().unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn one_dimension_accepted() {
        let scope = ScopeTuple {
            user: Some("tafeng".to_owned()),
            ..ScopeTuple::default()
        };
        scope.validate().expect("valid");
    }

    #[test]
    fn empty_component_rejected() {
        let scope = ScopeTuple {
            project: Some(String::new()),
            ..ScopeTuple::default()
        };
        let err = scope.validate().unwrap_err();
        assert!(matches!(err, DomainError::MalformedScope { .. }));
    }

    #[test]
    fn omits_none_in_json() {
        let scope = ScopeTuple {
            user: Some("tafeng".to_owned()),
            ..ScopeTuple::default()
        };
        let json = serde_json::to_string(&scope).expect("ser");
        assert_eq!(json, r#"{"user":"tafeng"}"#);
    }
}
