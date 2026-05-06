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
    /// Canonical wire encoding of this scope tuple.
    ///
    /// Comma-separated `key=value` pairs sorted alphabetically by key,
    /// with `None` dimensions elided. Empty tuples serialize to the
    /// empty string. Examples:
    ///
    ///   * `{user: "hmn:tafeng"}`              → `"user=hmn:tafeng"`
    ///   * `{tenant: "acme", user: "alice"}`   → `"tenant=acme,user=alice"`
    ///
    /// Stable across processes, byte-equal for equal scopes, and intended
    /// as the join key between records and consent grants (Issue #253,
    /// brief §14). The output stays inside the
    /// `[A-Za-z0-9._:=,-]` character class used by
    /// [`crate::domain::consent::ConsentEvent::scope`] so the same
    /// representation can flow through both the receipt timeline and the
    /// existing consent journal without an encoding split. Brace / quote
    /// characters from a JSON encoding would have collided with that
    /// grammar.
    ///
    /// Validation by [`ScopeTuple::validate`] rejects values containing
    /// `=`, `,`, or other reserved characters, so this serializer cannot
    /// produce ambiguous output for any value-input that survived
    /// validation.
    #[must_use]
    pub fn canonical_wire(&self) -> String {
        let mut parts: Vec<(&'static str, &str)> = Vec::with_capacity(7);
        if let Some(v) = &self.tenant {
            parts.push(("tenant", v));
        }
        if let Some(v) = &self.workspace {
            parts.push(("workspace", v));
        }
        if let Some(v) = &self.project {
            parts.push(("project", v));
        }
        if let Some(v) = &self.session_id {
            parts.push(("session_id", v));
        }
        if let Some(v) = &self.entity {
            parts.push(("entity", v));
        }
        if let Some(v) = &self.user {
            parts.push(("user", v));
        }
        if let Some(v) = &self.agent {
            parts.push(("agent", v));
        }
        parts.sort_by_key(|(k, _)| *k);
        parts
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Iterate the six bind dimensions in the canonical order used by
    /// `GraphQueries::scope_match_clause`. Yields `(name, Option<&str>)`
    /// so each call site can bind exactly six placeholders per tuple.
    ///
    /// `project` is intentionally omitted — it has no IDL filter
    /// predicate (see field doc) and is not part of the scope-by-
    /// provenance match clause.
    pub fn dimension_iter(&self) -> impl Iterator<Item = (&'static str, Option<&str>)> + '_ {
        [
            ("tenant", self.tenant.as_deref()),
            ("workspace", self.workspace.as_deref()),
            ("session_id", self.session_id.as_deref()),
            ("entity", self.entity.as_deref()),
            ("user", self.user.as_deref()),
            ("agent", self.agent.as_deref()),
        ]
        .into_iter()
    }

    /// Validate that at least one IDL-addressable dimension is present, no
    /// present component is empty, no component carries reserved
    /// characters, and `project` is not set.
    ///
    /// **Reserved characters (`=` and `,`) are rejected** because
    /// [`Self::canonical_wire`] uses them as the field-separator and
    /// pair-separator in the consent-grant join key (§14, Issue #253).
    /// Without this check, two distinct scopes could serialize to the
    /// same wire string (e.g. `{tenant: "a,b", user: "c"}` and
    /// `{tenant: "a", user: "b,c"}` both → `tenant=a,b,user=c` /
    /// `tenant=a,user=b,c`), letting a receipt-timeline grant for one
    /// scope appear to cover another. Migration 0024's
    /// `consent_timeline_grant_immutable` is not a substitute: it
    /// pins one `(sensor_id, scope)` per `consent_ref`, but it cannot
    /// stop two distinct tuples from colliding to the same wire
    /// string under different `consent_ref` values. Components are
    /// restricted to `[A-Za-z0-9._:-]` so the canonical form stays
    /// injective and inside the consent grant scope grammar
    /// `[a-z0-9._:=,-]` (`crate::domain::consent::validate_scope`)
    /// under any case.
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
            if let Some(v) = value {
                if v.is_empty() {
                    return Err(DomainError::MalformedScope {
                        message: format!("`{name}` must not be empty if present"),
                    });
                }
                if let Some(bad) = v.chars().find(|c| !is_scope_component_char(*c)) {
                    return Err(DomainError::MalformedScope {
                        message: format!(
                            "`{name}` contains reserved character {bad:?}; allowed: \
                             ASCII alphanumerics and `._:-` (no `=` or `,`, which \
                             would make canonical_wire ambiguous and let a grant \
                             for one scope cover a record under a different scope)"
                        ),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Allowed ASCII set for a single scope component: alphanumerics plus
/// `.`, `_`, `:`, `-`. Excludes `=` and `,` so [`ScopeTuple::canonical_wire`]
/// is unambiguous, and excludes whitespace / control chars so the wire
/// form is safe to log and round-trip through line-oriented stores.
///
/// Round 5 reverts round 4's widening: admitting `=` and `,` would let
/// two distinct scopes serialize to the same wire string and silently
/// authorize each other. Migration 0024's
/// `consent_timeline_grant_immutable` only enforces that one
/// `consent_ref` keeps one `(sensor_id, scope)` tuple; it cannot
/// distinguish two tuples whose `canonical_wire` already collides
/// under different `consent_ref` values, so it is not a substitute for
/// an injective wire encoding. Cairn is pre-release 0.0.1 with no
/// persisted vaults to migrate (see scope.rs:38), so the global
/// rejection is acceptable.
fn is_scope_component_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_iter() {
        let scope = ScopeTuple {
            tenant: Some("acme".to_owned()),
            workspace: Some("ws".to_owned()),
            session_id: Some("01HQZ".to_owned()),
            entity: Some("ent".to_owned()),
            user: Some("alice".to_owned()),
            agent: Some("agt".to_owned()),
            ..ScopeTuple::default()
        };
        let dims: Vec<(&'static str, Option<&str>)> = scope.dimension_iter().collect();
        assert_eq!(dims.len(), 6, "must yield exactly six dimensions");
        let names: Vec<&str> = dims.iter().map(|(k, _)| *k).collect();
        assert_eq!(
            names,
            [
                "tenant",
                "workspace",
                "session_id",
                "entity",
                "user",
                "agent"
            ],
            "canonical order must match scope_match_clause"
        );
        assert_eq!(dims[0].1, Some("acme"));
        assert_eq!(dims[1].1, Some("ws"));
        assert_eq!(dims[2].1, Some("01HQZ"));
        assert_eq!(dims[3].1, Some("ent"));
        assert_eq!(dims[4].1, Some("alice"));
        assert_eq!(dims[5].1, Some("agt"));
    }

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

    #[test]
    fn canonical_wire_emits_sorted_kv_pairs_inside_consent_grammar() {
        let scope = ScopeTuple {
            user: Some("hmn:tafeng".to_owned()),
            tenant: Some("acme".to_owned()),
            ..ScopeTuple::default()
        };
        // Sorted alphabetically by key, comma-separated, no JSON braces or
        // quotes — stays inside the consent grant scope grammar
        // `[a-z0-9._:=,-]`.
        assert_eq!(scope.canonical_wire(), "tenant=acme,user=hmn:tafeng");
    }

    #[test]
    fn canonical_wire_single_dimension_round_trip() {
        let scope = ScopeTuple {
            user: Some("hmn:tafeng".to_owned()),
            ..ScopeTuple::default()
        };
        assert_eq!(scope.canonical_wire(), "user=hmn:tafeng");
    }

    #[test]
    fn canonical_wire_empty_tuple_is_empty_string() {
        assert_eq!(ScopeTuple::default().canonical_wire(), "");
    }

    /// Round 5: `=` and `,` must be rejected so `canonical_wire` is
    /// injective. The migration 0024 trigger pins one
    /// `(sensor_id, scope)` per `consent_ref` but cannot stop two
    /// distinct tuples from colliding to the same wire string under
    /// different `consent_ref` values, so it is not a substitute for
    /// an injective wire encoding.
    #[test]
    fn validate_rejects_equals_in_component() {
        let scope = ScopeTuple {
            user: Some("a=b".to_owned()),
            ..ScopeTuple::default()
        };
        let err = scope.validate().unwrap_err();
        let DomainError::MalformedScope { message } = err else {
            panic!("expected MalformedScope");
        };
        assert!(
            message.contains("reserved character") && message.contains("'='"),
            "expected reserved-character error, got: {message}"
        );
    }

    #[test]
    fn validate_rejects_comma_in_component() {
        let scope = ScopeTuple {
            tenant: Some("a,b".to_owned()),
            ..ScopeTuple::default()
        };
        let err = scope.validate().unwrap_err();
        assert!(
            matches!(err, DomainError::MalformedScope { .. }),
            "expected MalformedScope"
        );
    }

    #[test]
    fn validate_rejects_whitespace_in_component() {
        let scope = ScopeTuple {
            user: Some("a b".to_owned()),
            ..ScopeTuple::default()
        };
        assert!(matches!(
            scope.validate().unwrap_err(),
            DomainError::MalformedScope { .. }
        ));
    }

    #[test]
    fn validate_accepts_alphanum_and_dot_underscore_colon_dash() {
        let scope = ScopeTuple {
            tenant: Some("acme-corp".to_owned()),
            workspace: Some("ws_1.dev".to_owned()),
            user: Some("hmn:tafeng".to_owned()),
            session_id: Some("01HQZ".to_owned()),
            ..ScopeTuple::default()
        };
        scope.validate().expect("alphanum + ._:- should validate");
    }

    /// Pin the no-collision invariant: any two scopes that differ on
    /// at least one dimension produce different `canonical_wire`
    /// strings, because every component is restricted to a character
    /// set that cannot contain the `=` field-separator or the `,`
    /// pair-separator.
    #[test]
    fn canonical_wire_is_injective_for_validated_scopes() {
        let a = ScopeTuple {
            tenant: Some("acme".to_owned()),
            user: Some("alice".to_owned()),
            ..ScopeTuple::default()
        };
        let b = ScopeTuple {
            tenant: Some("acme".to_owned()),
            user: Some("bob".to_owned()),
            ..ScopeTuple::default()
        };
        a.validate().expect("a valid");
        b.validate().expect("b valid");
        assert_ne!(a.canonical_wire(), b.canonical_wire());
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
