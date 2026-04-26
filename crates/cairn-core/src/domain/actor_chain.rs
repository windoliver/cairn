//! Actor chain entries (brief §4.2).
//!
//! Every record carries a chain of `{role, identity, at}` entries. P0
//! requires only a single `author` entry signed by the authoring identity;
//! `principal`, `delegator`, and `sensor` entries arrive at P2 once
//! multi-agent delegation lands. Chain ordering at P2 is `principal →
//! delegator* → author → sensor*`; we enforce that ordering at parse time
//! so adapter layers don't need to re-implement the rule.

use serde::{Deserialize, Serialize};

use crate::domain::{DomainError, Identity, Rfc3339Timestamp};

/// Role tag for a chain entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ChainRole {
    /// The user / principal who initiated the work.
    Principal,
    /// An intermediate agent that forwarded the request.
    Delegator,
    /// The agent (or human) that actually authored the record.
    Author,
    /// The sensor that captured the source bytes.
    Sensor,
}

/// One entry in `actor_chain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorChainEntry {
    /// Role of this entry.
    pub role: ChainRole,
    /// Identity that played this role.
    pub identity: Identity,
    /// Wall-clock instant the role was executed.
    pub at: Rfc3339Timestamp,
}

/// Validate chain ordering and required entries.
///
/// Rules (§4.2):
/// - At least one entry.
/// - Exactly one `Author` entry.
/// - Order is `Principal* → Delegator* → Author → Sensor*` — a single
///   author bracketed by zero-or-more principals/delegators before and
///   sensors after.
pub fn validate_chain(entries: &[ActorChainEntry]) -> Result<(), DomainError> {
    if entries.is_empty() {
        return Err(DomainError::MissingSignature {
            message: "actor_chain must contain at least one entry".to_owned(),
        });
    }

    let author_count = entries
        .iter()
        .filter(|e| e.role == ChainRole::Author)
        .count();
    if author_count != 1 {
        return Err(DomainError::MissingSignature {
            message: format!(
                "actor_chain must contain exactly one `author` entry, found {author_count}"
            ),
        });
    }

    let mut seen_author = false;
    let mut seen_delegator = false;
    for entry in entries {
        match entry.role {
            ChainRole::Principal => {
                if seen_delegator || seen_author {
                    return Err(DomainError::MissingSignature {
                        message: "`principal` entries must precede delegator/author/sensor"
                            .to_owned(),
                    });
                }
            }
            ChainRole::Delegator => {
                if seen_author {
                    return Err(DomainError::MissingSignature {
                        message: "`delegator` entries must precede author/sensor".to_owned(),
                    });
                }
                seen_delegator = true;
            }
            ChainRole::Author => {
                seen_author = true;
            }
            ChainRole::Sensor => {
                if !seen_author {
                    return Err(DomainError::MissingSignature {
                        message: "`sensor` entries must follow author".to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(role: ChainRole, id: &str) -> ActorChainEntry {
        ActorChainEntry {
            role,
            identity: Identity::parse(id).expect("valid"),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }
    }

    #[test]
    fn single_author_ok() {
        let chain = vec![entry(ChainRole::Author, "agt:claude-code:opus-4-7:main:v1")];
        validate_chain(&chain).expect("valid");
    }

    #[test]
    fn full_p2_chain_ok() {
        let chain = vec![
            entry(ChainRole::Principal, "usr:tafeng"),
            entry(ChainRole::Delegator, "agt:claude-code:opus-4-7:main:v3"),
            entry(ChainRole::Author, "agt:claude-code:opus-4-7:reviewer:v1"),
            entry(ChainRole::Sensor, "snr:local:hook:cc-session:v1"),
        ];
        validate_chain(&chain).expect("valid");
    }

    #[test]
    fn empty_rejected() {
        let err = validate_chain(&[]).unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn missing_author_rejected() {
        let chain = vec![entry(ChainRole::Principal, "usr:tafeng")];
        let err = validate_chain(&chain).unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn duplicate_author_rejected() {
        let chain = vec![
            entry(ChainRole::Author, "agt:claude-code:opus-4-7:main:v1"),
            entry(ChainRole::Author, "agt:claude-code:opus-4-7:reviewer:v1"),
        ];
        let err = validate_chain(&chain).unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn out_of_order_rejected() {
        let chain = vec![
            entry(ChainRole::Author, "agt:claude-code:opus-4-7:main:v1"),
            entry(
                ChainRole::Delegator,
                "agt:claude-code:opus-4-7:supervisor:v1",
            ),
        ];
        let err = validate_chain(&chain).unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }

    #[test]
    fn sensor_before_author_rejected() {
        let chain = vec![
            entry(ChainRole::Sensor, "snr:local:hook:cc-session:v1"),
            entry(ChainRole::Author, "agt:claude-code:opus-4-7:main:v1"),
        ];
        let err = validate_chain(&chain).unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }
}
