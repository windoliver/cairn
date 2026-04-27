//! Sensor-label manifest — the structural rule for `SensorLabel`s Cairn
//! accepts on incoming `CaptureEvent`s (brief §9.1).
//!
//! ## What this layer is
//!
//! [`validate_label`] enforces a structural rule
//! `local:<family>:<instance>(:<sub>)*:v<digits>` — declared family,
//! version pattern, valid segment characters. This is the schema-level
//! shape gate: it rejects garbage strings and blocks non-`local:`
//! roots, but it does not enumerate which sensor *instances* are
//! authorized.
//!
//! [`P0_CANONICAL_LABELS`] is a public list of well-known default
//! sensor labels (the cooperating harnesses, the per-agent proactive
//! surfaces, and the generic local sensors). [`validate_label_in_registry`]
//! pairs the structural check with closed-list membership when a
//! caller has a fixed registry to enforce.
//!
//! ## What this layer is NOT
//!
//! Schema validation is **defence in depth**, not authorization. Real
//! sensor-instance trust comes from keychain-backed Ed25519 signing at
//! the `SignedIntent` layer, landing with issue #50. Per-deployment
//! closed-list enforcement (rejecting `local:hook:evil-host:v1`) is the
//! caller's choice — pass the configured registry to
//! [`validate_label_in_registry`] when running in a strict-deployment
//! mode.

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

/// Validate `label` against the structural rule
/// `local:<family>:<instance>(:<sub>)*:v<digits>`.
///
/// Returns [`DomainError::UndeclaredSensor`] for any deviation. Pure
/// function — no I/O, no global state.
///
/// **Schema-only.** This is shape validation, not authorization. The
/// keychain-backed identity provisioning (#50) and the deploy-time
/// configured sensor registry are the authoritative trust gates;
/// callers that need closed-list enforcement at this layer can pair
/// this with [`validate_label_in_registry`] using their configured
/// registry, or with [`P0_CANONICAL_LABELS`] for the default set.
pub fn validate_label(label: &SensorLabel) -> Result<(), DomainError> {
    let s = label.as_str();
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 4 {
        return Err(reject(s));
    }
    if parts[0] != "local" {
        return Err(reject(s));
    }
    if !P0_SENSOR_FAMILIES.contains(&parts[1]) {
        return Err(reject(s));
    }
    let version = parts[parts.len() - 1];
    if !is_version(version) {
        return Err(reject(s));
    }
    for segment in &parts[2..parts.len() - 1] {
        if segment.is_empty() || !is_instance_segment(segment) {
            return Err(reject(s));
        }
    }
    Ok(())
}

/// Stricter check: structural rule + closed-list membership against
/// `registry`. Use this when a deployment has a known sensor registry
/// (e.g., the runtime registry from #50, or a hand-curated test-time
/// list). Returns the same [`DomainError::UndeclaredSensor`] for both
/// shape and membership failures.
pub fn validate_label_in_registry(
    label: &SensorLabel,
    registry: &[&str],
) -> Result<(), DomainError> {
    validate_label(label)?;
    if registry.contains(&label.as_str()) {
        Ok(())
    } else {
        Err(reject(label.as_str()))
    }
}

fn reject(label: &str) -> DomainError {
    DomainError::UndeclaredSensor {
        label: label.to_owned(),
    }
}

fn is_version(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix('v') {
        !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit())
    } else {
        false
    }
}

fn is_instance_segment(s: &str) -> bool {
    s.bytes().all(|b| {
        matches!(b,
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-')
    })
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
    fn accepts_arbitrary_instance_with_valid_shape() {
        // Schema layer is shape-only; trust comes from #50 / signature
        // verification. Any well-shaped label passes here.
        validate_label(&label("local:hook:any-instance:v1")).expect("structurally valid");
    }

    #[test]
    fn registry_check_rejects_unregistered_instance() {
        // The stricter API enforces the closed list when callers need
        // it.
        let err =
            validate_label_in_registry(&label("local:hook:evil-host:v1"), P0_CANONICAL_LABELS)
                .unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn registry_check_accepts_canonical() {
        validate_label_in_registry(&label("local:hook:cc-session:v1"), P0_CANONICAL_LABELS)
            .expect("canonical");
    }

    #[test]
    fn rejects_arbitrary_suffix_without_version() {
        let err = validate_label(&label("local:hook:anything")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn rejects_non_numeric_version() {
        let err = validate_label(&label("local:hook:cc-session:vx")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn accepts_multi_segment_instance() {
        validate_label(&label("local:hook:cc-session:host-42:v1")).expect("valid");
    }
}
