//! [`BodyHash`] — `blake3:` + 64 lowercase hex chars over a record's body.
//!
//! Drives the idempotent-upsert decision in
//! [`crate::contract::memory_store::MemoryStore`]: identical hash → no
//! version bump. Computation is centralized in [`BodyHash::compute`] so
//! producers and verifiers can never disagree.

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// Wire-form `blake3:<64 lowercase hex>` hash over a record's UTF-8 body.
///
/// Centralizing computation in [`BodyHash::compute`] guarantees that every
/// producer (the verb layer) and every verifier (the store) derives the
/// same digest from the same bytes — the prerequisite for idempotent
/// upsert and supersession-without-spurious-version-bumps.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct BodyHash(String);

impl BodyHash {
    /// Parse a wire-form `blake3:<64 lowercase hex>` string.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let Some(tail) = raw.strip_prefix("blake3:") else {
            return Err(DomainError::EmptyField { field: "body_hash" });
        };
        if tail.len() != 64 {
            return Err(DomainError::EmptyField { field: "body_hash" });
        }
        if !tail.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(DomainError::EmptyField { field: "body_hash" });
        }
        Ok(Self(raw))
    }

    /// Compute over a UTF-8 body string.
    #[must_use]
    pub fn compute(body: &str) -> Self {
        let hash = blake3::hash(body.as_bytes());
        Self(format!("blake3:{}", hash.to_hex()))
    }

    /// Underlying wire-form string slice (`blake3:<hex>`).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for BodyHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for BodyHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_is_deterministic() {
        let a = BodyHash::compute("hello world");
        let b = BodyHash::compute("hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn compute_differs_on_different_input() {
        let a = BodyHash::compute("alpha");
        let b = BodyHash::compute("beta");
        assert_ne!(a, b);
    }

    #[test]
    fn parses_well_formed_hash() {
        let raw = format!("blake3:{}", "a".repeat(64));
        BodyHash::parse(raw).expect("valid");
    }

    #[test]
    fn rejects_missing_prefix() {
        let raw = "a".repeat(64);
        let err = BodyHash::parse(raw).unwrap_err();
        assert!(matches!(
            err,
            DomainError::EmptyField { field: "body_hash" }
        ));
    }

    #[test]
    fn rejects_uppercase_hex() {
        let raw = format!("blake3:{}", "A".repeat(64));
        let err = BodyHash::parse(raw).unwrap_err();
        assert!(matches!(
            err,
            DomainError::EmptyField { field: "body_hash" }
        ));
    }

    #[test]
    fn rejects_wrong_length() {
        let raw = format!("blake3:{}", "a".repeat(63));
        let err = BodyHash::parse(raw).unwrap_err();
        assert!(matches!(
            err,
            DomainError::EmptyField { field: "body_hash" }
        ));
    }

    #[test]
    fn computed_hash_is_parseable() {
        let h = BodyHash::compute("anything");
        BodyHash::parse(h.as_str().to_owned()).expect("compute → parse roundtrip");
    }
}
