//! [`TargetId`] — supersession lineage key (brief §3, §3.0).
//!
//! Distinct from [`crate::domain::record::RecordId`]: `RecordId` identifies one
//! version row; `TargetId` identifies the lineage that supersession
//! advances. Same wire form (ULID, 26 chars, Crockford base32, uppercase,
//! no `I L O U`, leading char `0..=7`).

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// ULID-typed supersession lineage key. 26 chars, Crockford base32,
/// uppercase, no `I L O U`. Distinct from [`crate::domain::record::RecordId`]:
/// `RecordId` identifies one version row; `TargetId` identifies the
/// lineage that supersession advances across version rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct TargetId(String);

impl TargetId {
    /// Parse a wire-form ULID. Same validation as
    /// [`crate::domain::record::RecordId::parse`].
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.len() != 26 {
            return Err(DomainError::EmptyField { field: "target_id" });
        }
        let bytes = raw.as_bytes();
        if !matches!(bytes[0], b'0'..=b'7') {
            return Err(DomainError::EmptyField { field: "target_id" });
        }
        if !bytes[1..].iter().all(|b| {
            matches!(b,
                b'0'..=b'9'
                | b'A'..=b'H'
                | b'J'
                | b'K'
                | b'M'
                | b'N'
                | b'P'..=b'T'
                | b'V'..=b'Z')
        }) {
            return Err(DomainError::EmptyField { field: "target_id" });
        }
        Ok(Self(raw))
    }

    /// Underlying ULID string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for TargetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for TargetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_ulid() {
        let t = TargetId::parse("01HQZX9F5N0000000000000000").expect("valid");
        assert_eq!(t.as_str(), "01HQZX9F5N0000000000000000");
    }

    #[test]
    fn rejects_overflow_first_char() {
        let err = TargetId::parse("8ZZZZZZZZZZZZZZZZZZZZZZZZZ").unwrap_err();
        assert!(matches!(
            err,
            DomainError::EmptyField { field: "target_id" }
        ));
    }

    #[test]
    fn rejects_wrong_length() {
        let err = TargetId::parse("01HQZX9F5N").unwrap_err();
        assert!(matches!(
            err,
            DomainError::EmptyField { field: "target_id" }
        ));
    }

    #[test]
    fn rejects_lowercase_alphabet() {
        let err = TargetId::parse("01hqzx9f5n0000000000000000").unwrap_err();
        assert!(matches!(
            err,
            DomainError::EmptyField { field: "target_id" }
        ));
    }

    #[test]
    fn json_roundtrip() {
        let t = TargetId::parse("01HQZX9F5N0000000000000000").expect("valid");
        let s = serde_json::to_string(&t).expect("ser");
        assert_eq!(s, "\"01HQZX9F5N0000000000000000\"");
        let back: TargetId = serde_json::from_str(&s).expect("de");
        assert_eq!(t, back);
    }
}
