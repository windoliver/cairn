//! Scope tuple (brief §6, §4.2).
//!
//! Every record carries a scope tuple so the ranker, consolidator, promoter,
//! and expirer can branch on `(tenant, workspace, project, session_id,
//! entity, user, agent)`. Each field is optional individually, but at least
//! one must be set — an unscoped record cannot be retrieved or governed.
//!
//! Field names align with the IDL `ScopeFilter`
//! (`crates/cairn-idl/schema/common/scope_filter.json`) so a record's scope
//! is addressable by the same query keys callers use against
//! `search` / `retrieve` / `forget`. The one IDL gap is `project` — it is a
//! valid record-side scope dimension but the IDL filter does not yet expose
//! a `project` predicate; queries narrow on `project` indirectly via
//! `tags` or `entity` until that filter ships.

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
    /// Project tree this record belongs to. Domain-only dimension — no
    /// IDL filter predicate yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// Session id (ULID) when the record is session-scoped. Field name
    /// matches the IDL `ScopeFilter.session_id` predicate.
    ///
    /// No legacy alias is provided. Cairn is pre-release (0.0.1) with no
    /// persisted records to migrate, and a deserialize-time alias would
    /// rewrite the canonical signed bytes (`session` → `session_id`)
    /// during hydration — breaking any future signature canonicalization
    /// scheme. If a migration is ever needed, it ships as an explicit,
    /// versioned re-sign step (brief §4.2 "Signature-first rejection").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
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
    /// Validate that at least one IDL-addressable dimension is present, no
    /// present component is empty, and `project` is not set.
    ///
    /// `project` is a brief-§6 scope dimension but the IDL `ScopeFilter`
    /// does not yet expose a `project` predicate. Even when paired with
    /// another dimension, a record scoped on `project` cannot be exactly
    /// retrieved or forgotten through the public grammar — only narrowed
    /// indirectly. Until the IDL adds the predicate (tracked as a
    /// follow-up issue) the safest behavior is to reject the field at
    /// validation time, leaving the struct field for forward compatibility
    /// without admitting unaddressable records.
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.project.is_some() {
            return Err(DomainError::MalformedScope {
                message: "`project` scope dimension is not yet supported — the IDL `ScopeFilter` has no `project` predicate, so project-scoped records cannot be exactly retrieved or forgotten. Track via the IDL ScopeFilter issue.".to_owned(),
            });
        }
        let any_idl_addressable = self.tenant.is_some()
            || self.workspace.is_some()
            || self.session_id.is_some()
            || self.entity.is_some()
            || self.user.is_some()
            || self.agent.is_some();
        if !any_idl_addressable {
            return Err(DomainError::MalformedScope {
                message:
                    "at least one of tenant, workspace, session_id, entity, user, agent is required"
                        .to_owned(),
            });
        }
        for (name, value) in [
            ("tenant", &self.tenant),
            ("workspace", &self.workspace),
            ("project", &self.project),
            ("session_id", &self.session_id),
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
            user: Some(String::new()),
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

    #[test]
    fn project_field_rejected_until_idl_predicate_lands() {
        let scope_alone = ScopeTuple {
            project: Some("cairn".to_owned()),
            ..ScopeTuple::default()
        };
        assert!(matches!(
            scope_alone.validate().unwrap_err(),
            DomainError::MalformedScope { .. }
        ));

        let scope_with_user = ScopeTuple {
            project: Some("cairn".to_owned()),
            user: Some("tafeng".to_owned()),
            ..ScopeTuple::default()
        };
        assert!(
            matches!(
                scope_with_user.validate().unwrap_err(),
                DomainError::MalformedScope { .. }
            ),
            "project is rejected even alongside an IDL-addressable dimension until ScopeFilter exposes a `project` predicate"
        );
    }

    /// No legacy alias is exposed — see the field doc on `session_id`.
    /// This test pins the rejection so a future contributor can't add
    /// `#[serde(alias = ...)]` without considering the
    /// signature-canonicalization implications.
    #[test]
    fn legacy_session_key_rejected() {
        let json = r#"{"session": "01HQZ"}"#;
        let res: Result<ScopeTuple, _> = serde_json::from_str(json);
        assert!(res.is_err(), "legacy `session` key must not deserialize");
    }

    /// Field names that overlap with the IDL `ScopeFilter` predicate set
    /// must serialize to the same key — otherwise a record's scope cannot
    /// be addressed by an IDL-shaped query.
    #[test]
    fn aligns_with_idl_filter_keys() {
        let scope = ScopeTuple {
            tenant: Some("acme".to_owned()),
            workspace: Some("ws".to_owned()),
            entity: Some("ent".to_owned()),
            session_id: Some("01HQZ".to_owned()),
            user: Some("tafeng".to_owned()),
            agent: Some("agt".to_owned()),
            ..ScopeTuple::default()
        };
        let json = serde_json::to_value(&scope).expect("ser");
        let obj = json.as_object().expect("object");
        for key in [
            "tenant",
            "workspace",
            "entity",
            "session_id",
            "user",
            "agent",
        ] {
            assert!(obj.contains_key(key), "missing IDL-aligned key `{key}`");
        }
    }
}
