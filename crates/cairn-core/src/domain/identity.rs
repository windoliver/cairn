//! Identity newtype and discriminator (brief §4.2).
//!
//! Three identity kinds — `HumanIdentity`, `AgentIdentity`, `SensorIdentity`
//! — share one wire form: `<prefix>:<body>` where `prefix ∈ {agt, usr, snr}`
//! and `body` matches `[A-Za-z0-9._:-]+`. The pattern matches the
//! `Identity` schema in `crates/cairn-idl/schema/common/primitives.json`.

use serde::{Deserialize, Serialize};

use crate::domain::DomainError;

/// Which of the three identity kinds an [`Identity`] denotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdentityKind {
    /// `usr:` prefix — a human principal.
    Human,
    /// `agt:` prefix — an agent (model + harness + role + revision).
    Agent,
    /// `snr:` prefix — a sensor (family + name + host + revision).
    Sensor,
}

/// A typed identity reference. Construction validates the prefix and body
/// per §4.2. Wire form is the underlying string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Identity(String);

impl Identity {
    /// Construct an [`Identity`] from a wire-form string. Returns
    /// [`DomainError::InvalidIdentity`] if the prefix is unknown, the body is
    /// empty, or the body contains characters outside `[A-Za-z0-9._:-]`.
    pub fn parse(raw: impl Into<String>) -> Result<Self, DomainError> {
        let raw = raw.into();
        let body = if let Some(b) = raw.strip_prefix("agt:") {
            b
        } else if let Some(b) = raw.strip_prefix("usr:") {
            b
        } else if let Some(b) = raw.strip_prefix("snr:") {
            b
        } else {
            return Err(DomainError::InvalidIdentity {
                message: "must start with one of [agt:, usr:, snr:]".to_owned(),
            });
        };
        if body.is_empty() {
            return Err(DomainError::InvalidIdentity {
                message: "body after prefix must not be empty".to_owned(),
            });
        }
        if !body.bytes().all(|b| {
            matches!(b,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b':' | b'-')
        }) {
            return Err(DomainError::InvalidIdentity {
                message: "body chars must be in [A-Za-z0-9._:-]".to_owned(),
            });
        }
        Ok(Self(raw))
    }

    /// Discriminator for which of the three kinds this identity is.
    #[must_use]
    pub fn kind(&self) -> IdentityKind {
        if self.0.starts_with("agt:") {
            IdentityKind::Agent
        } else if self.0.starts_with("usr:") {
            IdentityKind::Human
        } else {
            IdentityKind::Sensor
        }
    }

    /// Wire-form string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Identity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent() {
        let id = Identity::parse("agt:claude-code:opus-4-7:reviewer:v1").expect("valid");
        assert_eq!(id.kind(), IdentityKind::Agent);
    }

    #[test]
    fn parse_human() {
        let id = Identity::parse("usr:tafeng").expect("valid");
        assert_eq!(id.kind(), IdentityKind::Human);
    }

    #[test]
    fn parse_sensor() {
        let id = Identity::parse("snr:local:screen:host:v1").expect("valid");
        assert_eq!(id.kind(), IdentityKind::Sensor);
    }

    #[test]
    fn rejects_unknown_prefix() {
        let err = Identity::parse("bot:foo").unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn rejects_empty_body() {
        let err = Identity::parse("agt:").unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn rejects_bad_chars() {
        let err = Identity::parse("agt:has space").unwrap_err();
        assert!(matches!(err, DomainError::InvalidIdentity { .. }));
    }

    #[test]
    fn round_trips_through_json() {
        let id = Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid");
        let s = serde_json::to_string(&id).expect("ser");
        let back: Identity = serde_json::from_str(&s).expect("de");
        assert_eq!(back, id);
    }

    #[test]
    fn deserialize_rejects_invalid() {
        let res: Result<Identity, _> = serde_json::from_str("\"bot:nope\"");
        assert!(res.is_err());
    }
}
