//! Session identity and auto-discovery resolver (brief §8.1).
//!
//! Sessions are metadata tuples — not vault folders — that bind a sequence of
//! turns to a `(user, agent, project_root)` triple. All eight verbs accept an
//! optional `session_id`. When absent, the resolver:
//!
//! 1. Looks up the most recent active session for the caller's identity.
//! 2. If found and within the idle window, reuses it.
//! 3. Otherwise creates a new one.
//!
//! This module owns the *pure* slice of that logic: identity types, the
//! decision function, and the source-precedence rules. Persistence lives in
//! [`crate::contract::MemoryStore`]; the adapter feeds the pre-resolved
//! "last active" tuple into [`resolve_session`].

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;
use crate::domain::identity::{Identity, IdentityKind};

/// Opaque session identifier. Typically a ULID minted by the store, but the
/// type accepts any non-empty `[A-Za-z0-9._:-]+` string so callers may pass
/// harness-supplied IDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Construct a [`SessionId`]. Returns
    /// [`DomainError::InvalidSessionId`] if empty or contains characters
    /// outside `[A-Za-z0-9._:-]`.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(DomainError::InvalidSessionId {
                message: "must not be empty".to_owned(),
            });
        }
        if !raw.bytes().all(|b| {
            matches!(b,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-')
        }) {
            return Err(DomainError::InvalidSessionId {
                message: "chars must be in [A-Za-z0-9._:-]".to_owned(),
            });
        }
        Ok(Self(raw))
    }

    /// Wire-form string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

/// The `(user, agent, project_root)` triple a session is keyed by.
///
/// `project_root` is a canonicalised absolute filesystem path string —
/// the resolver treats it opaquely. Passing the same triple twice within
/// the idle window resolves to the same session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionIdentity {
    /// `usr:` identity of the human principal.
    pub user: Identity,
    /// `agt:` identity of the agent on whose behalf the verb runs.
    pub agent: Identity,
    /// Canonicalised project root path, or `None` for vault-only context.
    pub project_root: Option<String>,
}

impl SessionIdentity {
    /// Construct a [`SessionIdentity`], rejecting identity kinds that violate
    /// §8.1 (`user` must be `usr:`, `agent` must be `agt:`).
    pub fn new(
        user: Identity,
        agent: Identity,
        project_root: Option<String>,
    ) -> Result<Self, DomainError> {
        if user.kind() != IdentityKind::Human {
            return Err(DomainError::InvalidIdentity {
                message: format!("session user must be `usr:` identity, got `{user}`"),
            });
        }
        if agent.kind() != IdentityKind::Agent {
            return Err(DomainError::InvalidIdentity {
                message: format!("session agent must be `agt:` identity, got `{agent}`"),
            });
        }
        if let Some(p) = &project_root
            && p.is_empty()
        {
            return Err(DomainError::EmptyField {
                field: "project_root",
            });
        }
        Ok(Self {
            user,
            agent,
            project_root,
        })
    }
}

/// A persisted session row (brief §8.1).
///
/// Stored in `.cairn/cairn.db` by the store adapter; the type itself is
/// pure data so verbs can pass it across the trait boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Stable identifier (typically a ULID minted by the store).
    pub id: SessionId,
    /// `(user, agent, project_root)` triple this session is keyed by.
    pub identity: SessionIdentity,
    /// Human-readable title. Empty by default; backfilled by `DreamWorkflow`.
    pub title: String,
    /// Optional `channel` metadata (`"chat"`, `"voice"`, …).
    pub channel: Option<String>,
    /// Optional `priority` hint (`"high"`, `"normal"`, …).
    pub priority: Option<String>,
    /// Free-form tags. Empty when unset.
    pub tags: Vec<String>,
    /// Unix epoch milliseconds when the row was inserted.
    pub created_at_unix_ms: i64,
    /// Unix epoch milliseconds of the most recent verb call on this session.
    pub last_activity_at_unix_ms: i64,
    /// `Some` once the session has been explicitly ended or aged out.
    pub ended_at_unix_ms: Option<i64>,
}

/// The pre-resolved "last active session" the store passes to the resolver.
///
/// Populated by `MemoryStore::find_active_session`. The adapter is
/// responsible for filtering by `(user, agent, project_root)` and ordering
/// by `last_activity_at DESC`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastActiveSession {
    /// Existing session ID.
    pub id: SessionId,
    /// Seconds since `last_activity_at` of that session.
    pub idle_secs: u64,
}

/// What the resolver decided.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionDecision {
    /// Reuse this existing session — append turns and bump `last_activity_at`.
    Reuse(SessionId),
    /// No active session within the idle window. Adapter should mint a fresh
    /// ULID and insert a row.
    CreateNew,
}

/// Precedence-resolved session source — what the verb dispatcher passes to
/// the store layer (§8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionSource {
    /// Caller explicitly named a session — never auto-discover.
    Explicit(SessionId),
    /// Auto-discover from `(user, agent, project_root)` within the
    /// configured idle window.
    AutoDiscover {
        /// Identity triple to look up.
        identity: SessionIdentity,
        /// Idle window in seconds. Default per brief is `86_400` (24 h).
        idle_window_secs: u64,
    },
}

/// Decide whether to reuse an existing session or create a new one.
///
/// Pure: takes only the identity, the idle window, and the store's
/// pre-resolved `LastActiveSession` lookup.
///
/// - `None` (no active session) → [`SessionDecision::CreateNew`]
/// - `Some` with `idle_secs <= idle_window_secs` → [`SessionDecision::Reuse`]
/// - `Some` with `idle_secs > idle_window_secs` → [`SessionDecision::CreateNew`]
#[must_use]
pub fn resolve_session(last: Option<LastActiveSession>, idle_window_secs: u64) -> SessionDecision {
    match last {
        Some(l) if l.idle_secs <= idle_window_secs => SessionDecision::Reuse(l.id),
        _ => SessionDecision::CreateNew,
    }
}

/// Default idle window per brief §8.1: 24 hours.
pub const DEFAULT_IDLE_WINDOW_SECS: u64 = 86_400;

#[cfg(test)]
mod tests {
    use super::*;

    fn ident_user() -> Identity {
        Identity::parse("usr:alice").expect("valid")
    }

    fn ident_agent() -> Identity {
        Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid")
    }

    #[test]
    fn session_id_parses_alnum() {
        let id = SessionId::parse("01HF8R6EZQK7XJ8M0V3WQNB4Z9").expect("valid");
        assert_eq!(id.as_str(), "01HF8R6EZQK7XJ8M0V3WQNB4Z9");
    }

    #[test]
    fn session_id_rejects_empty() {
        let err = SessionId::parse("").unwrap_err();
        assert!(matches!(err, DomainError::InvalidSessionId { .. }));
    }

    #[test]
    fn session_id_rejects_bad_chars() {
        let err = SessionId::parse("has space").unwrap_err();
        assert!(matches!(err, DomainError::InvalidSessionId { .. }));
    }

    #[test]
    fn session_id_round_trips_through_json() {
        let id = SessionId::parse("01HF8R6EZQK7XJ8M0V3WQNB4Z9").expect("valid");
        let s = serde_json::to_string(&id).expect("ser");
        let back: SessionId = serde_json::from_str(&s).expect("de");
        assert_eq!(back, id);
    }

    #[test]
    fn identity_requires_user_and_agent_kinds() {
        let err = SessionIdentity::new(ident_agent(), ident_user(), None).unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn identity_accepts_canonical_triple() {
        let id =
            SessionIdentity::new(ident_user(), ident_agent(), Some("/repo".into())).expect("valid");
        assert_eq!(id.project_root.as_deref(), Some("/repo"));
    }

    #[test]
    fn identity_rejects_empty_project_root() {
        let err =
            SessionIdentity::new(ident_user(), ident_agent(), Some(String::new())).unwrap_err();
        assert!(matches!(err, DomainError::EmptyField { .. }));
    }

    #[test]
    fn resolver_creates_new_when_none() {
        assert_eq!(
            resolve_session(None, DEFAULT_IDLE_WINDOW_SECS),
            SessionDecision::CreateNew,
        );
    }

    #[test]
    fn resolver_reuses_within_window() {
        let last = LastActiveSession {
            id: SessionId::parse("S1").expect("valid"),
            idle_secs: 3600,
        };
        assert_eq!(
            resolve_session(Some(last.clone()), DEFAULT_IDLE_WINDOW_SECS),
            SessionDecision::Reuse(last.id),
        );
    }

    #[test]
    fn resolver_creates_new_after_window() {
        let last = LastActiveSession {
            id: SessionId::parse("S1").expect("valid"),
            idle_secs: DEFAULT_IDLE_WINDOW_SECS + 1,
        };
        assert_eq!(
            resolve_session(Some(last), DEFAULT_IDLE_WINDOW_SECS),
            SessionDecision::CreateNew,
        );
    }

    #[test]
    fn resolver_reuses_at_exact_window_boundary() {
        let last = LastActiveSession {
            id: SessionId::parse("S1").expect("valid"),
            idle_secs: DEFAULT_IDLE_WINDOW_SECS,
        };
        assert_eq!(
            resolve_session(Some(last.clone()), DEFAULT_IDLE_WINDOW_SECS),
            SessionDecision::Reuse(last.id),
        );
    }
}
