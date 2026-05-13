//! Typed source identifiers for immutable `sources/` artifacts.

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// Stable identifier for a source artifact under `sources/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct SourceId(String);

impl SourceId {
    /// Parse a source identifier from its wire form.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(DomainError::MissingProvenance {
                field: "source_ids",
            });
        }
        Ok(Self(raw))
    }

    /// Borrow the wire-form identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SourceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}
