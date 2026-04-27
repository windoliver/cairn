//! Capture-mode → actor-chain attribution rule (brief §5.0.a).
//!
//! Every record's `actor_chain` (§4.2) names the actual author. The
//! capture mode pins which identity kind is allowed in the `Author` slot:
//!
//! | Mode | Author identity kind | Required additional entries |
//! |------|----------------------|------------------------------|
//! | `Auto`      | [`IdentityKind::Sensor`] | none |
//! | `Explicit`  | [`IdentityKind::Human`]  | (optional) one [`IdentityKind::Agent`] delegator |
//! | `Proactive` | [`IdentityKind::Agent`]  | none |
//!
//! [`attribute`] checks the rule on a fully-formed chain and returns the
//! author entry on success.
//!
//! The chain itself is structurally validated upstream by
//! [`super::actor_chain::validate_chain`] — `attribute` assumes that has
//! already passed and only enforces the §5.0.a kind rule.
//!
//! [`IdentityKind::Sensor`]: crate::domain::IdentityKind::Sensor
//! [`IdentityKind::Human`]: crate::domain::IdentityKind::Human
//! [`IdentityKind::Agent`]: crate::domain::IdentityKind::Agent

use crate::domain::{ActorChainEntry, CaptureMode, ChainRole, DomainError, IdentityKind};

/// Resolve and validate the author of `chain` against `mode`.
///
/// Returns the author entry on success (mirrors how downstream consumers
/// often need both the rule check and the author identity). Returns
/// [`DomainError::AttributionMismatch`] if the author kind disagrees with
/// the mode, or [`DomainError::MissingSignature`] if no author is
/// present.
pub fn attribute(
    mode: CaptureMode,
    chain: &[ActorChainEntry],
) -> Result<&ActorChainEntry, DomainError> {
    let author = chain
        .iter()
        .find(|e| e.role == ChainRole::Author)
        .ok_or_else(|| DomainError::MissingSignature {
            message: "actor_chain has no `author` entry".to_owned(),
        })?;

    let author_kind = author.identity.kind();
    let required = match mode {
        CaptureMode::Auto => IdentityKind::Sensor,
        CaptureMode::Explicit => IdentityKind::Human,
        CaptureMode::Proactive => IdentityKind::Agent,
    };
    if author_kind != required {
        return Err(DomainError::AttributionMismatch {
            message: format!(
                "mode `{}` requires `{}` author, got `{}` (`{}`)",
                mode.as_str(),
                identity_kind_name(required),
                identity_kind_name(author_kind),
                author.identity.as_str()
            ),
        });
    }

    if mode == CaptureMode::Explicit {
        // Mode B may carry an optional Agent delegator (the assistant that
        // routed the user's "remember …" through the skill). If a
        // delegator is present, it must be an Agent — but
        // `validate_chain` already enforces that, so we don't re-check.
        // We *do* reject an Explicit chain that lists *only* a Sensor
        // entry — Mode B captures must trace back to a human at minimum.
        if !chain
            .iter()
            .any(|e| e.role == ChainRole::Author && e.identity.kind() == IdentityKind::Human)
        {
            return Err(DomainError::AttributionMismatch {
                message: "mode `explicit` requires a human author".to_owned(),
            });
        }
    }

    Ok(author)
}

const fn identity_kind_name(kind: IdentityKind) -> &'static str {
    match kind {
        IdentityKind::Human => "human",
        IdentityKind::Agent => "agent",
        IdentityKind::Sensor => "sensor",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Identity, Rfc3339Timestamp};

    fn entry(role: ChainRole, id: &str) -> ActorChainEntry {
        ActorChainEntry {
            role,
            identity: Identity::parse(id).expect("valid"),
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }
    }

    #[test]
    fn auto_mode_requires_sensor_author() {
        let chain = vec![entry(ChainRole::Author, "snr:local:hook:cc-session:v1")];
        let author = attribute(CaptureMode::Auto, &chain).expect("valid");
        assert_eq!(author.identity.kind(), IdentityKind::Sensor);
    }

    #[test]
    fn auto_mode_rejects_agent_author() {
        let chain = vec![entry(ChainRole::Author, "agt:claude-code:opus-4-7:main:v1")];
        let err = attribute(CaptureMode::Auto, &chain).unwrap_err();
        assert!(matches!(err, DomainError::AttributionMismatch { .. }));
    }

    #[test]
    fn auto_mode_rejects_human_author() {
        let chain = vec![entry(ChainRole::Author, "usr:tafeng")];
        let err = attribute(CaptureMode::Auto, &chain).unwrap_err();
        assert!(matches!(err, DomainError::AttributionMismatch { .. }));
    }

    #[test]
    fn explicit_mode_requires_human_author() {
        let chain = vec![entry(ChainRole::Author, "usr:tafeng")];
        let author = attribute(CaptureMode::Explicit, &chain).expect("valid");
        assert_eq!(author.identity.kind(), IdentityKind::Human);
    }

    #[test]
    fn explicit_mode_with_delegator_ok() {
        let chain = vec![
            entry(ChainRole::Delegator, "agt:claude-code:opus-4-7:main:v1"),
            entry(ChainRole::Author, "usr:tafeng"),
        ];
        attribute(CaptureMode::Explicit, &chain).expect("valid");
    }

    #[test]
    fn explicit_mode_rejects_sensor_author() {
        let chain = vec![entry(ChainRole::Author, "snr:local:hook:cc-session:v1")];
        let err = attribute(CaptureMode::Explicit, &chain).unwrap_err();
        assert!(matches!(err, DomainError::AttributionMismatch { .. }));
    }

    #[test]
    fn proactive_mode_requires_agent_author() {
        let chain = vec![entry(
            ChainRole::Author,
            "agt:claude-code:opus-4-7:reviewer:v1",
        )];
        let author = attribute(CaptureMode::Proactive, &chain).expect("valid");
        assert_eq!(author.identity.kind(), IdentityKind::Agent);
    }

    #[test]
    fn proactive_mode_rejects_human_author() {
        let chain = vec![entry(ChainRole::Author, "usr:tafeng")];
        let err = attribute(CaptureMode::Proactive, &chain).unwrap_err();
        assert!(matches!(err, DomainError::AttributionMismatch { .. }));
    }

    #[test]
    fn missing_author_rejected() {
        let chain: Vec<ActorChainEntry> = vec![];
        let err = attribute(CaptureMode::Auto, &chain).unwrap_err();
        assert!(matches!(err, DomainError::MissingSignature { .. }));
    }
}
