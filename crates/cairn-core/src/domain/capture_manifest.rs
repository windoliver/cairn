//! Sensor-label manifest — the closed set of `SensorLabel` patterns Cairn's
//! P0 binary will accept on incoming `CaptureEvent`s (brief §9.1).
//!
//! A sensor that hasn't been registered cannot emit events: this enforces
//! the §9 invariant that "every sensor enables via config" and keeps
//! capture under human or harness control. The check is purely
//! syntactic — an enabled sensor still needs the runtime
//! `SensorIdentity` provisioning from issue #50 (keychain-backed keys)
//! before the resulting record can be signed.
//!
//! Patterns are matched as `<prefix>:*` — the manifest asserts that the
//! label *starts with* one of the declared roots. This lets the hook
//! sensor produce labels like `local:hook:cc-session:v1` and
//! `local:hook:codex-session:v1` without listing every harness here.

use crate::domain::{DomainError, SensorLabel};

/// The closed set of P0 sensor-label prefixes (brief §9.1 + §9.1.a + the
/// three Mode B / Mode C entry-point surfaces).
///
/// Adding a new sensor to P0 is a brief-level change — declare its prefix
/// here and update §9.1 of the design brief in the same PR.
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

/// Validate that `label` starts with one of [`P0_SENSOR_LABEL_PREFIXES`].
///
/// Returns [`DomainError::UndeclaredSensor`] otherwise. Pure function —
/// no I/O, no global state.
pub fn validate_label(label: &SensorLabel) -> Result<(), DomainError> {
    if P0_SENSOR_LABEL_PREFIXES
        .iter()
        .any(|prefix| label.as_str().starts_with(prefix))
    {
        return Ok(());
    }
    Err(DomainError::UndeclaredSensor {
        label: label.as_str().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(s: &str) -> SensorLabel {
        SensorLabel::parse(s).expect("valid label syntax")
    }

    #[test]
    fn accepts_each_declared_prefix() {
        for prefix in P0_SENSOR_LABEL_PREFIXES {
            let raw = format!("{prefix}example:v1");
            validate_label(&label(&raw)).expect("declared prefix accepted");
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
        // "scren" is not "screen".
        let err = validate_label(&label("local:scren:host:v1")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn accepts_long_body_after_prefix() {
        validate_label(&label("local:hook:cc-session:host-42:v1")).expect("valid");
    }
}
