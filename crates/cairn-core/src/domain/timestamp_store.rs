//! `Timestamp` — a store-layer timestamp type distinct from the domain's
//! strict `Rfc3339Timestamp`.
//!
//! Store columns (`created_at`, `tombstoned_at`, `expired_at`, etc.) use
//! ISO-8601 / RFC3339 strings, but the WAL executor and rowmap helpers
//! need a type that:
//! - Can be constructed from raw strings read back from `SQLite`.
//! - Has a `Default` (used when a column is NULL or unparseable in
//!   non-critical paths — tests, fallback deserialization).
//! - Is distinct from `Rfc3339Timestamp` which refuses to parse anything
//!   that isn't well-formed RFC3339.
//!
//! `Timestamp` is permissive: `parse_iso8601` returns the string as-is
//! (after basic non-empty check). The strict validation gate at write time
//! is `Rfc3339Timestamp`; `Timestamp` exists solely to ferry strings
//! through audit columns without re-validating them.

use serde::{Deserialize, Serialize};

/// Permissive ISO-8601 / RFC3339 string for audit column round-trips.
///
/// Default is the empty string (sentinel for NULL columns). Callers
/// rendering timestamps to users should prefer `Rfc3339Timestamp`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Timestamp(String);

impl Timestamp {
    /// Wrap a raw string from a `SQLite` audit column. No validation.
    #[must_use]
    pub fn parse_iso8601(raw: impl Into<String>) -> Option<Self> {
        let s = raw.into();
        if s.is_empty() { None } else { Some(Self(s)) }
    }

    /// Underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Construct from a known-good `Rfc3339Timestamp` string.
    #[must_use]
    pub fn from_rfc3339(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&crate::domain::timestamp::Rfc3339Timestamp> for Timestamp {
    fn from(ts: &crate::domain::timestamp::Rfc3339Timestamp) -> Self {
        Self(ts.as_str().to_owned())
    }
}

impl From<crate::domain::timestamp::Rfc3339Timestamp> for Timestamp {
    fn from(ts: crate::domain::timestamp::Rfc3339Timestamp) -> Self {
        Self(ts.as_str().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_returns_none() {
        assert!(Timestamp::parse_iso8601("").is_none());
    }

    #[test]
    fn non_empty_wraps() {
        let ts = Timestamp::parse_iso8601("2026-04-22T14:02:11Z").unwrap();
        assert_eq!(ts.as_str(), "2026-04-22T14:02:11Z");
    }

    #[test]
    fn default_is_empty() {
        let ts = Timestamp::default();
        assert_eq!(ts.as_str(), "");
    }
}
