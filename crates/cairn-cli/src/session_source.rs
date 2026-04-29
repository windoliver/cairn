//! Session-source precedence resolver for the CLI verb dispatcher
//! (brief §8.1).
//!
//! Maps the four input channels —
//! 1. explicit `--session` / `--session-id` flag,
//! 2. `CAIRN_SESSION_ID` environment variable,
//! 3. harness-supplied id (e.g., a hook payload echoed via env),
//! 4. fallback to auto-discover from `(user, agent, project_root)` —
//!
//! into a single [`SessionSource`]. Pure: takes an explicit `env: &dyn EnvLookup`
//! shim so tests can swap in a fake without touching `std::env`.
//!
//! Once a verb actually runs (#46), it calls
//! [`crate::session_source::resolve`] with the parsed args + identity, then
//! hands the resulting [`SessionSource`] to the store layer
//! ([`SessionSource::Explicit`] short-circuits the lookup;
//! [`SessionSource::AutoDiscover`] feeds `find_active_session` →
//! [`cairn_core::domain::session::resolve_session`]).

use cairn_core::domain::session::{
    DEFAULT_IDLE_WINDOW_SECS, SessionId, SessionIdentity, SessionSource,
};

/// Errors raised by the precedence resolver.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionSourceError {
    /// Both `--session` flag and `CAIRN_SESSION_ID` were set with **different**
    /// values. The CLI fails closed rather than silently picking one.
    #[error(
        "ambiguous session: --session={flag:?} and CAIRN_SESSION_ID={env:?} \
         disagree — pass only one or set them to the same value"
    )]
    Ambiguous {
        /// Value passed via `--session`.
        flag: String,
        /// Value read from `CAIRN_SESSION_ID`.
        env: String,
    },
    /// A supplied session id failed [`SessionId::parse`].
    #[error("invalid session id from {origin}: {message}")]
    InvalidId {
        /// Where the bad id came from (`"--session"`, `"CAIRN_SESSION_ID"`,
        /// `"harness"`).
        origin: &'static str,
        /// The underlying parse error message.
        message: String,
    },
}

/// Trait the resolver uses to look up environment variables. Production
/// passes [`StdEnv`]; tests pass a [`HashMap`]-backed fake.
pub trait EnvLookup {
    /// Read `key` from the environment. Returns `None` if unset.
    fn get(&self, key: &str) -> Option<String>;
}

/// Adapter over `std::env::var`.
pub struct StdEnv;

impl EnvLookup for StdEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Resolve the session source from the four input channels.
///
/// Precedence (§8.1):
/// `--session` flag > `CAIRN_SESSION_ID` env > harness id > auto-discover.
///
/// When both the flag and env are set with the **same** value, that value is
/// used and resolution succeeds — only divergence is rejected as
/// [`SessionSourceError::Ambiguous`].
///
/// `harness_supplied` is the optional id a hook adapter (e.g., the Claude
/// Code session-start hook) echoed via its own channel. The fallback —
/// auto-discover — uses `identity` and `idle_window_secs`.
///
/// # Errors
///
/// - [`SessionSourceError::Ambiguous`] if `--session` and `CAIRN_SESSION_ID`
///   disagree.
/// - [`SessionSourceError::InvalidId`] if any non-empty supplied id fails
///   [`SessionId::parse`].
pub fn resolve(
    flag: Option<&str>,
    env: &dyn EnvLookup,
    harness_supplied: Option<&str>,
    identity: SessionIdentity,
    idle_window_secs: u64,
) -> Result<SessionSource, SessionSourceError> {
    let env_value = env.get("CAIRN_SESSION_ID");

    // Treat empty strings as "unset" — common when a hook leaves the var
    // exported but blank rather than unsetting it.
    let flag = flag.filter(|s| !s.is_empty());
    let env_value = env_value.as_deref().filter(|s| !s.is_empty());

    if let (Some(f), Some(e)) = (flag, env_value)
        && f != e
    {
        return Err(SessionSourceError::Ambiguous {
            flag: f.to_owned(),
            env: e.to_owned(),
        });
    }

    if let Some(raw) = flag {
        let id = SessionId::parse(raw).map_err(|e| SessionSourceError::InvalidId {
            origin: "--session",
            message: e.to_string(),
        })?;
        return Ok(SessionSource::Explicit(id));
    }

    if let Some(raw) = env_value {
        let id = SessionId::parse(raw).map_err(|e| SessionSourceError::InvalidId {
            origin: "CAIRN_SESSION_ID",
            message: e.to_string(),
        })?;
        return Ok(SessionSource::Explicit(id));
    }

    if let Some(raw) = harness_supplied.filter(|s| !s.is_empty()) {
        let id = SessionId::parse(raw).map_err(|e| SessionSourceError::InvalidId {
            origin: "harness",
            message: e.to_string(),
        })?;
        return Ok(SessionSource::Explicit(id));
    }

    Ok(SessionSource::AutoDiscover {
        identity,
        idle_window_secs,
    })
}

/// Convenience wrapper around [`resolve`] using [`DEFAULT_IDLE_WINDOW_SECS`].
///
/// # Errors
///
/// Same as [`resolve`].
pub fn resolve_default(
    flag: Option<&str>,
    env: &dyn EnvLookup,
    harness_supplied: Option<&str>,
    identity: SessionIdentity,
) -> Result<SessionSource, SessionSourceError> {
    resolve(
        flag,
        env,
        harness_supplied,
        identity,
        DEFAULT_IDLE_WINDOW_SECS,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use cairn_core::domain::Identity;

    use super::*;

    struct MapEnv(HashMap<&'static str, String>);

    impl EnvLookup for MapEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    fn empty_env() -> MapEnv {
        MapEnv(HashMap::new())
    }

    fn env_with(k: &'static str, v: &str) -> MapEnv {
        let mut m = HashMap::new();
        m.insert(k, v.to_owned());
        MapEnv(m)
    }

    fn ident() -> SessionIdentity {
        SessionIdentity::new(
            Identity::parse("usr:alice").expect("user"),
            Identity::parse("agt:cli:opus-4-7:main:v1").expect("agent"),
            Some("/repo".into()),
        )
        .expect("identity")
    }

    #[test]
    fn flag_wins_when_only_flag_set() {
        let got = resolve_default(Some("S1"), &empty_env(), None, ident()).expect("ok");
        assert!(matches!(got, SessionSource::Explicit(id) if id.as_str() == "S1"));
    }

    #[test]
    fn env_used_when_flag_absent() {
        let got =
            resolve_default(None, &env_with("CAIRN_SESSION_ID", "S2"), None, ident()).expect("ok");
        assert!(matches!(got, SessionSource::Explicit(id) if id.as_str() == "S2"));
    }

    #[test]
    fn flag_overrides_env_when_equal() {
        let got = resolve_default(
            Some("S1"),
            &env_with("CAIRN_SESSION_ID", "S1"),
            None,
            ident(),
        )
        .expect("ok");
        assert!(matches!(got, SessionSource::Explicit(id) if id.as_str() == "S1"));
    }

    #[test]
    fn flag_and_env_disagree_is_ambiguous() {
        let err = resolve_default(
            Some("S1"),
            &env_with("CAIRN_SESSION_ID", "S2"),
            None,
            ident(),
        )
        .unwrap_err();
        assert!(matches!(err, SessionSourceError::Ambiguous { .. }));
    }

    #[test]
    fn harness_used_when_flag_and_env_absent() {
        let got = resolve_default(None, &empty_env(), Some("HARNESS-1"), ident()).expect("ok");
        assert!(matches!(got, SessionSource::Explicit(id) if id.as_str() == "HARNESS-1"));
    }

    #[test]
    fn flag_overrides_harness() {
        let got =
            resolve_default(Some("S1"), &empty_env(), Some("HARNESS-1"), ident()).expect("ok");
        assert!(matches!(got, SessionSource::Explicit(id) if id.as_str() == "S1"));
    }

    #[test]
    fn env_overrides_harness() {
        let got = resolve_default(
            None,
            &env_with("CAIRN_SESSION_ID", "S2"),
            Some("HARNESS-1"),
            ident(),
        )
        .expect("ok");
        assert!(matches!(got, SessionSource::Explicit(id) if id.as_str() == "S2"));
    }

    #[test]
    fn falls_back_to_auto_discover() {
        let got = resolve_default(None, &empty_env(), None, ident()).expect("ok");
        assert!(matches!(got, SessionSource::AutoDiscover { .. }));
    }

    #[test]
    fn empty_string_is_treated_as_unset() {
        // Both flag and env empty → auto-discover, not ambiguous.
        let got = resolve_default(Some(""), &env_with("CAIRN_SESSION_ID", ""), None, ident())
            .expect("ok");
        assert!(matches!(got, SessionSource::AutoDiscover { .. }));
    }

    #[test]
    fn invalid_flag_value_is_rejected() {
        let err = resolve_default(Some("has space"), &empty_env(), None, ident()).unwrap_err();
        assert!(matches!(
            err,
            SessionSourceError::InvalidId {
                origin: "--session",
                ..
            },
        ));
    }

    #[test]
    fn invalid_env_value_is_rejected() {
        let err = resolve_default(
            None,
            &env_with("CAIRN_SESSION_ID", "bad/char"),
            None,
            ident(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SessionSourceError::InvalidId {
                origin: "CAIRN_SESSION_ID",
                ..
            },
        ));
    }

    #[test]
    fn auto_discover_carries_idle_window() {
        let got = resolve(None, &empty_env(), None, ident(), 3600).expect("ok");
        if let SessionSource::AutoDiscover {
            idle_window_secs, ..
        } = got
        {
            assert_eq!(idle_window_secs, 3600);
        } else {
            panic!("expected AutoDiscover");
        }
    }
}
