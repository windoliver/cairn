//! Sensor-label manifest — the closed set of `SensorLabel` shapes
//! Cairn's P0 binary will accept on incoming `CaptureEvent`s (brief
//! §9.1).
//!
//! A sensor that hasn't been registered cannot emit events: this enforces
//! the §9 invariant that "every sensor enables via config" and keeps
//! capture under human or harness control. The check is purely
//! syntactic — an enabled sensor still needs the runtime
//! `SensorIdentity` provisioning from issue #50 (keychain-backed keys)
//! before the resulting record can be signed.
//!
//! ## Required shape
//!
//! `local:<family>:<instance>(:<sub>)*:v<digits>` — at minimum four
//! colon-separated parts (`local`, family, one instance segment,
//! version), with optional further instance segments (host, harness)
//! between the family and the version. Family must be one of
//! [`P0_SENSOR_FAMILIES`]. Each non-version segment is non-empty and
//! restricted to `[A-Za-z0-9._-]+`. Version is `v` followed by one or
//! more ASCII digits.
//!
//! Example accepted forms:
//! - `local:hook:cc-session:v1`
//! - `local:hook:cc-session:host-42:v3`
//! - `local:proactive:claude-code:v1`
//!
//! Rejected because the suffix is structurally invalid (not because the
//! family is unknown):
//! - `local:hook:anything`            — missing version
//! - `local:hook:cc-session:vx`       — version not numeric
//! - `local:hook::v1`                 — empty instance segment

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

/// Backwards-compatibility view of [`P0_SENSOR_FAMILIES`] as
/// `local:<family>:` prefixes.
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

/// Validate `label` against the structural rule documented in the module
/// header. Returns [`DomainError::UndeclaredSensor`] for any deviation.
/// Pure function — no I/O, no global state.
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
    // Every segment between family and version is a non-empty instance
    // identifier (`[A-Za-z0-9._-]+`).
    for segment in &parts[2..parts.len() - 1] {
        if segment.is_empty() || !is_instance_segment(segment) {
            return Err(reject(s));
        }
    }
    Ok(())
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
    fn accepts_each_declared_family() {
        for family in P0_SENSOR_FAMILIES {
            let raw = format!("local:{family}:example:v1");
            validate_label(&label(&raw)).expect("declared family accepted");
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
    fn accepts_multi_segment_instance() {
        validate_label(&label("local:hook:cc-session:host-42:v1")).expect("valid");
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
    fn rejects_missing_v_prefix_on_version() {
        let err = validate_label(&label("local:hook:cc-session:1")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn rejects_empty_instance_segment() {
        // `SensorLabel::parse` accepts the syntactic shape (no empty
        // chars), so we go straight through the public manifest API.
        let err = validate_label(&label("local:hook::v1")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }

    #[test]
    fn rejects_only_three_parts() {
        // `local:hook:v1` has no instance segment.
        let err = validate_label(&label("local:hook:v1")).unwrap_err();
        assert!(matches!(err, DomainError::UndeclaredSensor { .. }));
    }
}
