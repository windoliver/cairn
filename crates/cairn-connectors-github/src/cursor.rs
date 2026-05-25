//! `CursorState` — JSON map of per-resource sub-cursors.
//!
//! Persisted as the opaque cursor string handed back by `PollOutcome::next_cursor`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::GhError;

/// Per-resource cursor within the connector-level cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ResourceCursor {
    /// REST `since=` timestamp. Issues + PRs use this.
    pub since: Option<DateTime<Utc>>,
    /// REST page number for the current `since` window. Issues + PRs.
    pub page: Option<u32>,
    /// SHA of the last commit observed (commits resource only).
    pub last_sha: Option<String>,
    /// Branch the commit walk is targeting (commits resource only).
    pub branch: Option<String>,
}

/// Connector-level cursor: per-resource map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CursorState {
    /// Schema version. Always 1 for v0; bumped on breaking changes.
    #[serde(default = "default_version")]
    pub v: u32,
    /// Issues sub-cursor.
    #[serde(default)]
    pub issues: ResourceCursor,
    /// Pull-requests sub-cursor.
    #[serde(default)]
    pub prs: ResourceCursor,
    /// Commits sub-cursor.
    #[serde(default)]
    pub commits: ResourceCursor,
}

fn default_version() -> u32 {
    1
}

impl CursorState {
    /// Deserialize from the substrate's opaque cursor string. `None` and
    /// empty input both yield `Default::default()` (full backfill).
    pub fn decode(s: Option<&str>) -> Result<Self, GhError> {
        match s {
            None | Some("") => Ok(Self::default()),
            Some(raw) => Ok(serde_json::from_str(raw)?),
        }
    }

    /// Serialize to the substrate's opaque cursor string.
    pub fn encode(&self) -> Result<String, GhError> {
        Ok(serde_json::to_string(self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_none_returns_default() {
        let c = CursorState::decode(None).unwrap();
        assert_eq!(c.issues, ResourceCursor::default());
        assert_eq!(c.prs, ResourceCursor::default());
        assert_eq!(c.commits, ResourceCursor::default());
    }

    #[test]
    fn decode_empty_returns_default() {
        let c = CursorState::decode(Some("")).unwrap();
        assert_eq!(c.issues, ResourceCursor::default());
    }

    #[test]
    fn round_trip_preserves_fields() {
        let c = CursorState {
            v: 1,
            issues: ResourceCursor {
                since: DateTime::parse_from_rfc3339("2026-05-25T12:00:00Z")
                    .ok()
                    .map(|d| d.with_timezone(&Utc)),
                page: Some(3),
                ..Default::default()
            },
            commits: ResourceCursor {
                last_sha: Some("abc123".into()),
                branch: Some("main".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let s = c.encode().unwrap();
        let back = CursorState::decode(Some(&s)).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn accepts_unknown_top_level_fields_for_forward_compat() {
        let future =
            r#"{"v":2,"issues":{},"prs":{},"commits":{},"unknown_field":42,"another":"x"}"#;
        let c = CursorState::decode(Some(future)).expect("unknown fields tolerated");
        assert_eq!(c.v, 2);
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn cursor_round_trip(
            sha in proptest::option::of("[a-f0-9]{7,40}"),
            page in proptest::option::of(0u32..=10_000),
            branch in proptest::option::of("[a-zA-Z0-9_/-]{1,32}"),
        ) {
            let c = CursorState {
                v: 1,
                issues: ResourceCursor { page, ..Default::default() },
                commits: ResourceCursor {
                    last_sha: sha,
                    branch,
                    ..Default::default()
                },
                ..Default::default()
            };
            let s = c.encode().unwrap();
            let back = CursorState::decode(Some(&s)).unwrap();
            prop_assert_eq!(c, back);
        }
    }
}
