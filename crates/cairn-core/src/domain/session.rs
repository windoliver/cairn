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
    /// Construct a [`SessionIdentity`].
    ///
    /// Validates and normalizes inputs so the persisted `(user, agent,
    /// project_root)` tuple is canonical — semantically-equivalent paths
    /// otherwise fragment auto-discovery into multiple active sessions
    /// since the store compares the raw string in `project_root IS ?` and
    /// the unique index over `COALESCE(project_root, '')`.
    ///
    /// Rules:
    /// - `user` must be a `usr:` identity, `agent` must be `agt:` (§8.1).
    /// - `project_root`, when supplied, must be a non-empty absolute path
    ///   (`starts_with('/')` on POSIX; on Windows the typed-path call site
    ///   is responsible for upstream canonicalization since `cairn-core`
    ///   stays I/O-free). Trailing `/` characters are trimmed so `/repo`
    ///   and `/repo/` resolve to the same session.
    ///
    /// Filesystem-level canonicalization (resolving symlinks, normalizing
    /// `..` segments) lives in the call site that produces the path: it
    /// requires I/O and `cairn-core` is pure. The CLI's vault resolver
    /// passes a `std::path::Path::canonicalize()` result; the SDK
    /// expects callers to canonicalize before constructing the identity.
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
        let project_root = match project_root {
            None => None,
            Some(raw) => {
                if raw.is_empty() {
                    return Err(DomainError::EmptyField {
                        field: "project_root",
                    });
                }
                let normalized = normalize_project_root(&raw)?;
                Some(normalized)
            }
        };
        Ok(Self {
            user,
            agent,
            project_root,
        })
    }

    /// Hydrate a [`SessionIdentity`] from values already persisted by the
    /// store, bypassing the absolute-path check that
    /// [`SessionIdentity::new`] enforces on write.
    ///
    /// Read paths use this constructor so a vault upgraded from an older
    /// version of the resolver (which permitted relative `project_root`
    /// values) can still hydrate its existing rows. The write path keeps
    /// the strict validator: every newly persisted identity is canonical.
    /// Identity-kind checks (`usr:` / `agt:`) are still applied because
    /// they reflect a structural invariant of the row, not a string-shape
    /// guard.
    ///
    /// Empty-string `project_root` is rejected with [`DomainError::EmptyField`]
    /// to match the database trigger that prevents the empty form from
    /// being written.
    ///
    /// # Errors
    ///
    /// - [`DomainError::InvalidIdentity`] when `user`/`agent` aren't the
    ///   expected identity kinds.
    /// - [`DomainError::EmptyField`] when `project_root` is `Some("")`.
    pub fn from_persisted(
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
        if let Some(raw) = project_root.as_deref()
            && raw.is_empty()
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

/// What kind of absolute path the input string looks like.
///
/// Classification is purely string-shape; `cairn-core` stays I/O-free.
/// This is host-OS-independent so a vault written on Windows can still
/// be opened on macOS / Linux (and vice versa) without
/// `Path::is_absolute()` rejecting the foreign form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbsoluteShape {
    /// POSIX-style absolute path: starts with a single `/`.
    Posix,
    /// Windows drive-absolute: `[A-Za-z]:` followed by `\` or `/`.
    /// Both slash forms collapse to one canonical identity.
    WindowsDrive,
    /// Windows UNC-style: starts with `\\` or `//` (two leading
    /// separators of any spelling).
    WindowsUnc,
}

/// Classify an absolute path by its leading shape, host-OS-independent.
/// Returns `None` for relative or otherwise-unrecognized inputs.
///
/// Recognized forms:
/// - POSIX: `/...` (but not `//...`, which is UNC).
/// - Windows drive: `X:\...` or `X:/...` where `X` is `[A-Za-z]`.
/// - Windows UNC: starts with `\\` or `//` (server-share follows).
fn classify_absolute(raw: &str) -> Option<AbsoluteShape> {
    let bytes = raw.as_bytes();
    // UNC: two leading separators, in either spelling. Has to come
    // before the POSIX check, otherwise `//srv/share` would classify as
    // POSIX and collapse to a different identity.
    if bytes.len() >= 2
        && (bytes[0] == b'\\' || bytes[0] == b'/')
        && (bytes[1] == b'\\' || bytes[1] == b'/')
    {
        return Some(AbsoluteShape::WindowsUnc);
    }
    // Windows drive: `X:\` or `X:/`.
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return Some(AbsoluteShape::WindowsDrive);
    }
    // Bare drive letter `X:` (no separator) is not recognized as
    // absolute — it's drive-relative on Windows and meaningless on
    // POSIX.
    if !bytes.is_empty() && bytes[0] == b'/' {
        return Some(AbsoluteShape::Posix);
    }
    None
}

/// Normalize a `project_root` string for the active-session uniqueness
/// invariant.
///
/// - Requires the path to look absolute by string shape — host-OS
///   independent. Accepts POSIX (`/repo`), Windows drive (`C:\repo`,
///   `C:/repo`), and Windows UNC (`\\server\share`, `//server/share`).
///   Relative paths are rejected because two callers in different CWDs
///   would otherwise share the same relative string and collapse into
///   one session. Using string-shape classification rather than
///   [`std::path::Path::is_absolute`] means a vault created on Windows
///   stays openable on macOS / Linux without losing its rows behind a
///   host-platform validator.
/// - For Windows-shaped paths (drive or UNC), all forward slashes are
///   normalized to backslashes so `C:\repo`, `C:/repo`, `C:\repo\`,
///   and `C:/repo/` collapse to one identity. The lookup query and
///   unique index key on the raw string, so storing two slash spellings
///   would otherwise fork one repo into two active sessions.
/// - Trims trailing separators after slash-canonicalization. On POSIX
///   only `/` is a separator — `\` is a regular filename character, so
///   `/repo\` and `/repo` are distinct directories and must keep
///   distinct identities. On Windows-shaped paths both spellings get
///   trimmed (already collapsed to `\`). A lone `/` (POSIX root) or
///   `C:\` (Windows drive root) is preserved.
/// - Rejects whitespace-only paths.
///
/// Filesystem canonicalization (symlink resolution, `..` collapse) is the
/// caller's responsibility — `cairn-core` is I/O-free. This function only
/// performs string-level normalization that does not require touching disk.
fn normalize_project_root(raw: &str) -> Result<String, DomainError> {
    if raw.trim().is_empty() {
        return Err(DomainError::EmptyField {
            field: "project_root",
        });
    }
    let shape = classify_absolute(raw).ok_or_else(|| DomainError::InvalidProjectRoot {
        message: format!("project_root must be an absolute path, got `{raw}`"),
    })?;
    // Step 1: slash-canonicalize Windows-shaped paths so `C:\repo` and
    // `C:/repo` collapse to one identity. POSIX paths leave `\` alone
    // because it's a regular filename character there.
    let canonical: String = match shape {
        AbsoluteShape::Posix => raw.to_owned(),
        AbsoluteShape::WindowsDrive | AbsoluteShape::WindowsUnc => raw
            .chars()
            .map(|c| if c == '/' { '\\' } else { c })
            .collect(),
    };
    // Step 2: trim trailing separators that would otherwise fork
    // `/repo` from `/repo/`. POSIX trims `/` only; Windows-shaped paths
    // trim `\` (the canonical form after step 1).
    let trim_chars: &[char] = match shape {
        AbsoluteShape::Posix => &['/'],
        AbsoluteShape::WindowsDrive | AbsoluteShape::WindowsUnc => &['\\'],
    };
    let trimmed = canonical.trim_end_matches(trim_chars);
    // Re-classify so trimming a canonical root (`/` → `` or `C:\` →
    // `C:`) doesn't accidentally make the path non-absolute.
    if trimmed.is_empty() || classify_absolute(trimmed).is_none() {
        Ok(canonical)
    } else {
        Ok(trimmed.to_owned())
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
    ///
    /// Carries the caller's expected `SessionIdentity`. The store-layer
    /// resolver checks the persisted row's `(user, agent, project_root)`
    /// against this identity and rejects the call if they disagree —
    /// otherwise a leaked / copied / harness-supplied session id could
    /// hijack writes from a different identity (cross-session mixing).
    Explicit {
        /// The session id the caller named.
        id: SessionId,
        /// The identity the caller expects this session to belong to.
        /// Sourced from the same `(user, agent, project_root)` resolution
        /// the auto-discover branch uses.
        expected_identity: SessionIdentity,
    },
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
    fn identity_normalizes_trailing_slash() {
        let with_slash = SessionIdentity::new(ident_user(), ident_agent(), Some("/repo/".into()))
            .expect("valid");
        let without_slash =
            SessionIdentity::new(ident_user(), ident_agent(), Some("/repo".into())).expect("valid");
        assert_eq!(with_slash.project_root, without_slash.project_root);
        assert_eq!(with_slash.project_root.as_deref(), Some("/repo"));
    }

    #[test]
    fn identity_normalizes_multiple_trailing_slashes() {
        let id = SessionIdentity::new(ident_user(), ident_agent(), Some("/repo///".into()))
            .expect("valid");
        assert_eq!(id.project_root.as_deref(), Some("/repo"));
    }

    #[test]
    fn identity_rejects_relative_path() {
        let err = SessionIdentity::new(ident_user(), ident_agent(), Some("relative/path".into()))
            .unwrap_err();
        assert!(matches!(err, DomainError::InvalidProjectRoot { .. }));
    }

    #[test]
    fn identity_canonicalizes_windows_slash_variants_to_backslash() {
        // `C:\repo` and `C:/repo` are the same directory on Windows but
        // would key on different raw strings — forking one repo into two
        // active sessions. Canonicalize both to `C:\repo` regardless of
        // host OS, so a vault stays consistent across machines.
        let with_back = SessionIdentity::new(ident_user(), ident_agent(), Some(r"C:\repo".into()))
            .expect("valid backslash drive path");
        let with_fwd = SessionIdentity::new(ident_user(), ident_agent(), Some("C:/repo".into()))
            .expect("valid forward-slash drive path");
        assert_eq!(with_back.project_root, with_fwd.project_root);
        assert_eq!(with_back.project_root.as_deref(), Some(r"C:\repo"));

        // Mixed slashes inside the path also collapse.
        let mixed =
            SessionIdentity::new(ident_user(), ident_agent(), Some(r"C:\foo/bar\baz".into()))
                .expect("valid mixed-slash drive path");
        assert_eq!(mixed.project_root.as_deref(), Some(r"C:\foo\bar\baz"));
    }

    #[test]
    fn identity_accepts_windows_paths_on_any_host() {
        // `Path::is_absolute()` is host-OS-aware, which would reject
        // `C:\repo` when the same vault is opened from macOS / Linux.
        // Our string-shape classifier accepts it everywhere so a
        // cross-OS vault keeps resolving its sessions.
        let win = SessionIdentity::new(ident_user(), ident_agent(), Some(r"C:\repo".into()))
            .expect("Windows drive path must hydrate on any host");
        assert_eq!(win.project_root.as_deref(), Some(r"C:\repo"));

        let unc = SessionIdentity::new(ident_user(), ident_agent(), Some(r"\\srv\share".into()))
            .expect("Windows UNC must hydrate on any host");
        assert_eq!(unc.project_root.as_deref(), Some(r"\\srv\share"));

        // Forward-slash UNC also classifies as UNC and canonicalizes.
        let unc_fwd = SessionIdentity::new(ident_user(), ident_agent(), Some("//srv/share".into()))
            .expect("forward-slash UNC must hydrate on any host");
        assert_eq!(unc_fwd.project_root.as_deref(), Some(r"\\srv\share"));
    }

    #[test]
    fn identity_rejects_drive_relative_path() {
        // Bare `C:` (no separator) is drive-relative on Windows and
        // meaningless elsewhere — must not be accepted as absolute.
        let err =
            SessionIdentity::new(ident_user(), ident_agent(), Some(r"C:repo".into())).unwrap_err();
        assert!(matches!(err, DomainError::InvalidProjectRoot { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn identity_preserves_trailing_backslash_in_basename_on_posix() {
        // On POSIX `\` is a regular filename character, not a separator.
        // `/repo\` and `/repo` are different directories and must keep
        // distinct identities — otherwise two unrelated projects can
        // collapse into the same session row.
        let with_bs = SessionIdentity::new(ident_user(), ident_agent(), Some("/repo\\".into()))
            .expect("valid POSIX path with trailing \\\\ in basename");
        let plain =
            SessionIdentity::new(ident_user(), ident_agent(), Some("/repo".into())).expect("valid");
        assert_ne!(
            with_bs.project_root, plain.project_root,
            "POSIX must preserve trailing backslash since it's a filename character there",
        );
        assert_eq!(with_bs.project_root.as_deref(), Some("/repo\\"));
    }

    #[test]
    fn identity_preserves_lone_root_slash() {
        let id =
            SessionIdentity::new(ident_user(), ident_agent(), Some("/".into())).expect("valid");
        assert_eq!(id.project_root.as_deref(), Some("/"));
    }

    #[test]
    fn from_persisted_accepts_relative_path_for_legacy_rows() {
        let id = SessionIdentity::from_persisted(
            ident_user(),
            ident_agent(),
            Some("relative/path".into()),
        )
        .expect("legacy hydration should not enforce is_absolute");
        assert_eq!(id.project_root.as_deref(), Some("relative/path"));
    }

    #[test]
    fn from_persisted_still_rejects_empty_project_root() {
        let err = SessionIdentity::from_persisted(ident_user(), ident_agent(), Some(String::new()))
            .unwrap_err();
        assert!(matches!(err, DomainError::EmptyField { .. }));
    }

    #[test]
    fn from_persisted_still_validates_identity_kinds() {
        let err = SessionIdentity::from_persisted(ident_agent(), ident_user(), None).unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
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
