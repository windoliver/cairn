//! Actor reference — a compact, opaque identity string used in audit
//! columns (`tombstoned_by`, `purged_by`, `created_by`, etc.).
//!
//! `ActorRef` is deliberately unvalidated: it round-trips whatever the
//! WAL executor writes. For write paths the executor passes the
//! wire-form identity string; for read paths the adapter returns it as-is.
//! Strict validation lives in [`crate::domain::identity::Identity`].

use serde::{Deserialize, Serialize};

/// An opaque actor reference stored in audit columns.
///
/// Wire form is the raw string (typically the wire-form of an
/// [`crate::domain::identity::Identity`], e.g. `"agt:claude-code:v1"`).
/// No prefix validation is applied; callers are responsible for passing
/// a meaningful string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActorRef(String);

impl ActorRef {
    /// Construct from any string. No prefix validation.
    #[must_use]
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Underlying string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ActorRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ActorRef {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ActorRef {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let r = ActorRef::from_string("agt:claude-code:opus-4-7:main:v1");
        assert_eq!(r.as_str(), "agt:claude-code:opus-4-7:main:v1");
        let s = serde_json::to_string(&r).expect("ser");
        let back: ActorRef = serde_json::from_str(&s).expect("de");
        assert_eq!(back, r);
    }
}
