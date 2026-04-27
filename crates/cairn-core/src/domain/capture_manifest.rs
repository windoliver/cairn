//! Sensor-label manifest — the closed set of `SensorLabel`s Cairn's P0
//! binary will accept on incoming `CaptureEvent`s (brief §9.1).
//!
//! A sensor that hasn't been registered cannot emit events: this
//! enforces the §9 invariant that "every sensor enables via config"
//! and keeps capture under human or harness control.
//!
//! ## What this layer is
//!
//! [`P0_CANONICAL_LABELS`] is a *closed allowlist* of full sensor
//! labels — every label Cairn's P0 binary admits without any runtime
//! provisioning. [`validate_label`] rejects anything not in that list.
//!
//! ## What this layer is NOT
//!
//! The schema-level allowlist is a *defence in depth*, not the
//! authoritative trust boundary. Per-instance sensor authentication
//! (keychain-backed Ed25519 signing) lives at the `SignedIntent`
//! layer and lands with issue #50. When #50 ships, the runtime
//! registry can extend [`P0_CANONICAL_LABELS`] with provisioned
//! entries; until then, only the canonical labels are allowed and
//! arbitrary instance names like `local:hook:evil-host:v1` are
//! rejected.

use crate::domain::{DomainError, SensorLabel};

/// The closed set of P0 sensor families (brief §9.1 + §9.1.a + the three
/// Mode B / Mode C entry-point surfaces).
///
/// Adding a new sensor to P0 is a brief-level change — declare its
/// family here and update §9.1 of the design brief in the same PR.
pub const P0_SENSOR_FAMILIES: &[&str] = &[
    "hook",
    "ide",
    "terminal",
    "clipboard",
    "voice",
    "screen",
    "neuroskill",
    "recording",
    "cli",
    "mcp",
    "proactive",
];

/// `local:<family>:` prefixes derived from [`P0_SENSOR_FAMILIES`].
/// Retained as a public view for callers that want family-level
/// reasoning; full-label admission goes through
/// [`P0_CANONICAL_LABELS`].
pub const P0_SENSOR_LABEL_PREFIXES: &[&str] = &[
    "local:hook:",
    "local:ide:",
    "local:terminal:",
    "local:clipboard:",
    "local:voice:",
    "local:screen:",
    "local:neuroskill:",
    "local:recording:",
    "local:cli:",
    "local:mcp:",
    "local:proactive:",
];

/// Closed allowlist of full sensor labels accepted by P0 without any
/// runtime registration. Adding a label is a brief-level change —
/// declare it here and update §9.1 of the design brief in the same PR.
///
/// Per-harness hook + neuroskill sensors are listed for the three
/// cooperating harnesses (Claude Code, Codex, Gemini); proactive
/// surfaces are listed per agent. Generic local sensors (IDE, terminal,
/// clipboard, voice, screen, recording, CLI, MCP) use a `default`
/// instance.
pub const P0_CANONICAL_LABELS: &[&str] = &[
    "local:hook:cc-session:v1",
    "local:hook:codex-session:v1",
    "local:hook:gemini-session:v1",
    "local:neuroskill:cc-session:v1",
    "local:neuroskill:codex-session:v1",
    "local:neuroskill:gemini-session:v1",
    "local:ide:default:v1",
    "local:terminal:default:v1",
    "local:clipboard:default:v1",
    "local:voice:default:v1",
    "local:screen:default:v1",
    "local:recording:batch:v1",
    "local:cli:default:v1",
    "local:mcp:default:v1",
    "local:proactive:claude-code:v1",
    "local:proactive:codex:v1",
    "local:proactive:gemini:v1",
];

/// Validate `label` against the closed [`P0_CANONICAL_LABELS`] list.
/// Returns [`DomainError::UndeclaredSensor`] on any miss. Pure function
/// — no I/O, no global state.
pub fn validate_label(label: &SensorLabel) -> Result<(), DomainError> {
    if P0_CANONICAL_LABELS.contains(&label.as_str()) {
        Ok(())
    } else {
        Err(DomainError::UndeclaredSensor {
            label: label.as_str().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(s: &str) -> SensorLabel {
        SensorLabel::parse(s).expect("valid label syntax")
    }

    #[test]
    fn accepts_every_canonical_label() {
        for canonical in P0_CANONICAL_LABELS {
            validate_label(&label(canonical)).expect("canonical label accepted");
        }
    }

    #[test]
    fn rejects_undeclared_root() {
        let err = validate_label(&label("remote:slack:default:v1")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn rejects_bare_local() {
        let err = validate_label(&label("local:")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn rejects_typo_in_family() {
        let err = validate_label(&label("local:scren:default:v1")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn rejects_arbitrary_instance_under_known_family() {
        // Known family, unknown instance — pre-#50 the closed list is
        // authoritative.
        let err = validate_label(&label("local:hook:evil-host:v1")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn rejects_arbitrary_suffix_without_version() {
        let err = validate_label(&label("local:hook:anything")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn rejects_unknown_proactive_agent() {
        let err = validate_label(&label("local:proactive:other-agent:v1")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn rejects_version_drift() {
        // Canonical entries are pinned at v1; v2 is a brief change away.
        let err = validate_label(&label("local:hook:cc-session:v2")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }
}
