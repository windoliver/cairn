//! Session lifecycle types and pure resolution helpers (§8.1).
//!
//! This module intentionally performs no filesystem or environment I/O. Adapter
//! layers provide candidate IDs and project metadata; stores persist and resolve
//! active sessions from those normalized inputs.

/// Default active-session idle window from design brief §8.1: 24 hours.
pub const DEFAULT_IDLE_WINDOW_MILLIS: i64 = 24 * 60 * 60 * 1_000;

/// Where a resolved session ID came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionResolutionSource {
    /// Explicit verb argument, e.g. CLI `--session` or SDK/MCP `session_id`.
    ExplicitArg,
    /// Harness-provided session identity from a hook payload.
    Harness,
    /// Environment-provided session identity.
    Environment,
    /// Existing active session found in the store.
    AutoDiscovery,
    /// New session created by the store.
    AutoCreate,
}

impl SessionResolutionSource {
    #[must_use]
    fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitArg => "explicit_arg",
            Self::Harness => "harness",
            Self::Environment => "environment",
            Self::AutoDiscovery => "auto_discovery",
            Self::AutoCreate => "auto_create",
        }
    }
}

/// Errors returned while resolving or persisting sessions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// A direct session candidate was present but empty.
    #[error("invalid {id_source} session id: must not be empty")]
    InvalidSessionId {
        /// Candidate source that supplied the invalid ID.
        id_source: SessionResolutionSource,
    },
    /// The request lacks enough context to choose one session safely.
    #[error("ambiguous session context: {reason}")]
    AmbiguousContext {
        /// Human-readable reason with remediation.
        reason: String,
    },
    /// Request-level validation failed.
    #[error("invalid session request: {reason}")]
    InvalidRequest {
        /// Human-readable validation failure.
        reason: String,
    },
    /// The active store cannot resolve sessions.
    #[error("session store `{store}` cannot resolve sessions")]
    StoreUnavailable {
        /// Store implementation name.
        store: String,
    },
    /// The active store failed while resolving sessions.
    #[error("session store error: {message}")]
    Store {
        /// Store-specific error message.
        message: String,
    },
}

impl std::fmt::Display for SessionResolutionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Optional direct session IDs in precedence order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionIdCandidates {
    /// Highest-precedence explicit argument.
    pub explicit_arg: Option<String>,
    /// Harness hook/session ID.
    pub harness: Option<String>,
    /// Environment fallback ID.
    pub environment: Option<String>,
}

impl SessionIdCandidates {
    /// Select the highest-precedence direct session ID, if any.
    ///
    /// Precedence is `explicit_arg > harness > environment`. Any present but
    /// blank candidate is rejected instead of silently falling through.
    pub fn select_direct(&self) -> Result<Option<SelectedSessionId>, SessionError> {
        for (source, value) in [
            (SessionResolutionSource::ExplicitArg, &self.explicit_arg),
            (SessionResolutionSource::Harness, &self.harness),
            (SessionResolutionSource::Environment, &self.environment),
        ] {
            if let Some(session_id) = value {
                if session_id.trim().is_empty() {
                    return Err(SessionError::InvalidSessionId { id_source: source });
                }
                return Ok(Some(SelectedSessionId {
                    session_id: session_id.clone(),
                    source,
                }));
            }
        }
        Ok(None)
    }
}

/// A direct session ID selected from [`SessionIdCandidates`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedSessionId {
    /// Session ID to use or create.
    pub session_id: String,
    /// Candidate source that won precedence.
    pub source: SessionResolutionSource,
}

/// Normalized caller context used for auto-discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionContext {
    /// Canonical user identity.
    pub user_id: String,
    /// Canonical agent identity.
    pub agent_id: String,
    /// Active vault identifier, when already resolved by the caller.
    pub vault_id: Option<String>,
    /// Stable project identifier derived by an adapter from cwd/project metadata.
    pub project_id: Option<String>,
    /// Caller cwd, if known. Stored only as metadata.
    pub cwd: Option<String>,
}

impl SessionContext {
    /// Validate the minimum identity tuple needed for §8.1 lookup.
    pub fn validate(&self) -> Result<(), SessionError> {
        let mut missing = Vec::new();
        if self.user_id.trim().is_empty() {
            missing.push("user_id");
        }
        if self.agent_id.trim().is_empty() {
            missing.push("agent_id");
        }
        if missing.is_empty() {
            return Ok(());
        }

        Err(SessionError::AmbiguousContext {
            reason: format!(
                "missing {}; provide explicit session selection or caller identity",
                missing.join(" and ")
            ),
        })
    }
}

/// Metadata attached when a new session is created.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionMetadata {
    /// Origin channel, e.g. `cli`, `mcp`, or `hook`.
    pub channel: Option<String>,
    /// Caller priority label.
    pub priority: Option<String>,
    /// Arbitrary caller tags.
    pub tags: Vec<String>,
}

/// Store request to resolve or create the active session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveSessionRequest {
    /// Direct session candidates in precedence order.
    pub candidates: SessionIdCandidates,
    /// Normalized caller context.
    pub context: SessionContext,
    /// Metadata for a created session.
    pub metadata: SessionMetadata,
    /// Active-session idle window.
    pub idle_window_millis: i64,
    /// Caller-supplied wall-clock timestamp.
    pub now_unix_millis: i64,
}

impl ResolveSessionRequest {
    /// Validate request fields before store lookup.
    pub fn validate(&self) -> Result<(), SessionError> {
        self.context.validate()?;
        self.candidates.select_direct()?;
        if self.idle_window_millis <= 0 {
            return Err(SessionError::InvalidRequest {
                reason: "idle_window_millis must be > 0".to_owned(),
            });
        }
        Ok(())
    }
}

/// Persisted session metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// Stable session ID.
    pub session_id: String,
    /// Canonical user identity.
    pub user_id: String,
    /// Canonical agent identity.
    pub agent_id: String,
    /// Active vault identifier at creation time.
    pub vault_id: Option<String>,
    /// Stable project identifier.
    pub project_id: Option<String>,
    /// Caller cwd at creation time.
    pub cwd: Option<String>,
    /// Empty until later summarization/title workflow.
    pub title: String,
    /// Origin channel.
    pub channel: Option<String>,
    /// Caller priority label.
    pub priority: Option<String>,
    /// Caller tags.
    pub tags: Vec<String>,
    /// Creation timestamp.
    pub created_at_unix_millis: i64,
    /// Latest use timestamp.
    pub last_activity_at_unix_millis: i64,
    /// End timestamp when rolled over by the idle window.
    pub ended_at_unix_millis: Option<i64>,
}

/// Result of resolving a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSession {
    /// Resolved session ID.
    pub session_id: String,
    /// Whether this call created the row.
    pub created: bool,
    /// Resolution source.
    pub source: SessionResolutionSource,
    /// Persisted session row after resolution.
    pub record: SessionRecord,
}
