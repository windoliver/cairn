//! Default-visibility resolution (brief §6.3, §14).
//!
//! Every record entering the Filter stage gets a starting visibility tier
//! derived from a deterministic matrix over the capture's identity kind,
//! capture mode, source family, and per-vault [`VisibilityPolicy`].
//! Promotions above the default require an explicit `consent.log` entry
//! and live outside this module.
//!
//! The matrix is a closed lookup, not a heuristic — every `(IdentityKind,
//! CaptureMode, SourceFamily)` triple resolves to exactly one
//! [`MemoryVisibility`] before policy overrides apply.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::domain::{CaptureMode, IdentityKind, MemoryVisibility, SourceFamily};

/// Per-vault overrides for the default visibility matrix.
///
/// Stored under `_policy.yaml` per brief §3.0; the parser lives in
/// `crate::config` (this module accepts the parsed struct only). All fields
/// default to `None`, in which case [`default_visibility`] returns the
/// hard-coded matrix value.
///
/// `floor` raises the default to at least the given tier (clamped upward,
/// never demoted). `override_for_source` replaces the default for a
/// specific [`SourceFamily`]; the override is applied **after** the
/// matrix lookup but **before** the floor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisibilityPolicy {
    /// Optional tier the default is clamped up to (never down). Use to
    /// force a more-private floor for a vault — e.g. `Private` blocks any
    /// path that would have started at `Session`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<MemoryVisibility>,

    /// Per-source overrides. A present entry replaces the matrix default
    /// for that source family, prior to the floor clamp.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub override_for_source: HashMap<SourceFamily, MemoryVisibility>,
}

/// Resolve the default [`MemoryVisibility`] for a capture (§6.3).
///
/// The resolution is:
///
/// 1. Look up the matrix entry for `(identity_kind, mode, source)`.
/// 2. If `policy.override_for_source` has an entry for `source`, use it
///    instead.
/// 3. Clamp the result up to `policy.floor` (never down).
///
/// **Defaults (§14 deny-by-default):** every triple lands at `Private`
/// unless it is an automatic sensor capture (Mode A from a real sensor
/// family), in which case it lands at `Session` so the turn that
/// produced it can read its own observations without elevating to
/// project tier. Explicit (Mode B) and Proactive (Mode C) writes always
/// start at `Private`; the user (B) or agent (C) must explicitly promote
/// later, which goes through `consent.log`.
#[must_use]
pub fn default_visibility(
    identity_kind: IdentityKind,
    mode: CaptureMode,
    source: SourceFamily,
    policy: &VisibilityPolicy,
) -> MemoryVisibility {
    let matrix = matrix_lookup(identity_kind, mode, source);
    let after_override = policy
        .override_for_source
        .get(&source)
        .copied()
        .unwrap_or(matrix);
    match policy.floor {
        Some(floor) if after_override < floor => floor,
        _ => after_override,
    }
}

/// Hard-coded matrix used by [`default_visibility`] before policy
/// overrides apply.
///
/// Mode-driven rules (brief §5.0.a / §6.3):
/// - **Auto** + a real sensor family (`hook`, `ide`, `terminal`,
///   `clipboard`, `voice`, `screen`, `recording_batch`) → `Session`.
///   Sensor observations are turn-local; promoting them to `Private`
///   would silently widen scope from "this turn" to "vault-wide".
/// - **Auto** without a sensor family (`cli`, `mcp`, `proactive`) →
///   `Private`. Auto from these channels means the harness fired ingest
///   without explicit user action; treat as agent state.
/// - **Explicit** (Mode B) → always `Private`. The user said "remember
///   this"; the user must also say "share this" later.
/// - **Proactive** (Mode C) → always `Private`. Agent-generated facts
///   must clear consent before they leave the vault.
///
/// Identity kind is currently informational — the §4.2 attribution
/// rules already reject mismatches (Auto without `snr:`, Explicit
/// without `usr:`, Proactive without `agt:`) before the Filter stage,
/// so the matrix only differentiates on `mode × source`. The
/// `identity_kind` parameter is kept in the signature to make a future
/// "promote agt-authored Explicit captures to Project" policy a
/// one-line matrix change without a callsite migration.
#[allow(clippy::needless_pass_by_value)]
fn matrix_lookup(
    _identity_kind: IdentityKind,
    mode: CaptureMode,
    source: SourceFamily,
) -> MemoryVisibility {
    match mode {
        CaptureMode::Auto => match source {
            SourceFamily::Hook
            | SourceFamily::Ide
            | SourceFamily::Terminal
            | SourceFamily::Clipboard
            | SourceFamily::Voice
            | SourceFamily::Screen
            | SourceFamily::RecordingBatch => MemoryVisibility::Session,
            SourceFamily::Cli | SourceFamily::Mcp | SourceFamily::Proactive => {
                MemoryVisibility::Private
            }
        },
        CaptureMode::Explicit | CaptureMode::Proactive => MemoryVisibility::Private,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Matrix ────────────────────────────────────────────────────────

    #[test]
    fn auto_sensor_defaults_to_session() {
        for source in [
            SourceFamily::Hook,
            SourceFamily::Ide,
            SourceFamily::Terminal,
            SourceFamily::Clipboard,
            SourceFamily::Voice,
            SourceFamily::Screen,
            SourceFamily::RecordingBatch,
        ] {
            let v = default_visibility(
                IdentityKind::Sensor,
                CaptureMode::Auto,
                source,
                &VisibilityPolicy::default(),
            );
            assert_eq!(
                v,
                MemoryVisibility::Session,
                "auto+{source:?} should default to session"
            );
        }
    }

    #[test]
    fn auto_non_sensor_defaults_to_private() {
        for source in [
            SourceFamily::Cli,
            SourceFamily::Mcp,
            SourceFamily::Proactive,
        ] {
            let v = default_visibility(
                IdentityKind::Agent,
                CaptureMode::Auto,
                source,
                &VisibilityPolicy::default(),
            );
            assert_eq!(
                v,
                MemoryVisibility::Private,
                "auto+{source:?} should default to private"
            );
        }
    }

    #[test]
    fn explicit_mode_always_private() {
        let policy = VisibilityPolicy::default();
        for source in [
            SourceFamily::Cli,
            SourceFamily::Mcp,
            SourceFamily::Hook,
            SourceFamily::Ide,
            SourceFamily::Terminal,
            SourceFamily::Clipboard,
            SourceFamily::Voice,
            SourceFamily::Screen,
            SourceFamily::RecordingBatch,
            SourceFamily::Proactive,
        ] {
            assert_eq!(
                default_visibility(IdentityKind::Human, CaptureMode::Explicit, source, &policy),
                MemoryVisibility::Private,
                "explicit+{source:?} must start private"
            );
        }
    }

    #[test]
    fn proactive_mode_always_private() {
        let policy = VisibilityPolicy::default();
        for source in [
            SourceFamily::Proactive,
            SourceFamily::Cli,
            SourceFamily::Mcp,
        ] {
            assert_eq!(
                default_visibility(IdentityKind::Agent, CaptureMode::Proactive, source, &policy),
                MemoryVisibility::Private,
            );
        }
    }

    // ── Determinism ──────────────────────────────────────────────────

    #[test]
    fn matrix_is_total_and_deterministic() {
        // Every triple resolves to exactly one tier and the result never
        // varies on repeated calls.
        let kinds = [
            IdentityKind::Human,
            IdentityKind::Agent,
            IdentityKind::Sensor,
        ];
        let modes = [
            CaptureMode::Auto,
            CaptureMode::Explicit,
            CaptureMode::Proactive,
        ];
        let sources = [
            SourceFamily::Hook,
            SourceFamily::Ide,
            SourceFamily::Terminal,
            SourceFamily::Clipboard,
            SourceFamily::Voice,
            SourceFamily::Screen,
            SourceFamily::RecordingBatch,
            SourceFamily::Cli,
            SourceFamily::Mcp,
            SourceFamily::Proactive,
        ];
        let policy = VisibilityPolicy::default();
        for k in kinds {
            for m in modes {
                for s in sources {
                    let a = default_visibility(k, m, s, &policy);
                    let b = default_visibility(k, m, s, &policy);
                    assert_eq!(a, b, "{k:?},{m:?},{s:?} non-deterministic");
                }
            }
        }
    }

    // ── Policy overrides ─────────────────────────────────────────────

    #[test]
    fn policy_floor_clamps_upward_only() {
        // floor=Project: an Auto+Hook (default Session) lands at Project,
        // an Explicit+Cli (default Private) lands at Project — both
        // raised to the floor.
        let policy = VisibilityPolicy {
            floor: Some(MemoryVisibility::Project),
            ..Default::default()
        };
        assert_eq!(
            default_visibility(
                IdentityKind::Sensor,
                CaptureMode::Auto,
                SourceFamily::Hook,
                &policy
            ),
            MemoryVisibility::Project,
        );
        assert_eq!(
            default_visibility(
                IdentityKind::Human,
                CaptureMode::Explicit,
                SourceFamily::Cli,
                &policy
            ),
            MemoryVisibility::Project,
        );
    }

    #[test]
    fn policy_floor_never_demotes() {
        // floor=Private cannot lower an Auto+Hook (default Session) to Private.
        let policy = VisibilityPolicy {
            floor: Some(MemoryVisibility::Private),
            ..Default::default()
        };
        assert_eq!(
            default_visibility(
                IdentityKind::Sensor,
                CaptureMode::Auto,
                SourceFamily::Hook,
                &policy
            ),
            MemoryVisibility::Session,
            "floor must clamp upward only — never demote"
        );
    }

    #[test]
    fn source_override_replaces_matrix_default() {
        let mut overrides = HashMap::new();
        overrides.insert(SourceFamily::Clipboard, MemoryVisibility::Private);
        let policy = VisibilityPolicy {
            override_for_source: overrides,
            ..Default::default()
        };
        // Default would be Session (Auto + sensor); override forces Private.
        assert_eq!(
            default_visibility(
                IdentityKind::Sensor,
                CaptureMode::Auto,
                SourceFamily::Clipboard,
                &policy
            ),
            MemoryVisibility::Private,
        );
        // Other sources unaffected.
        assert_eq!(
            default_visibility(
                IdentityKind::Sensor,
                CaptureMode::Auto,
                SourceFamily::Hook,
                &policy
            ),
            MemoryVisibility::Session,
        );
    }

    #[test]
    fn override_then_floor_compose() {
        // Override drops Hook to Private, then floor=Project raises it to Project.
        let mut overrides = HashMap::new();
        overrides.insert(SourceFamily::Hook, MemoryVisibility::Private);
        let policy = VisibilityPolicy {
            floor: Some(MemoryVisibility::Project),
            override_for_source: overrides,
        };
        assert_eq!(
            default_visibility(
                IdentityKind::Sensor,
                CaptureMode::Auto,
                SourceFamily::Hook,
                &policy
            ),
            MemoryVisibility::Project,
        );
    }

    // ── Serde ────────────────────────────────────────────────────────

    #[test]
    fn empty_policy_round_trips_to_empty_object() {
        let p = VisibilityPolicy::default();
        let s = serde_json::to_string(&p).expect("serialize");
        assert_eq!(s, "{}");
        let back: VisibilityPolicy = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn policy_round_trips_with_floor_and_overrides() {
        let mut overrides = HashMap::new();
        overrides.insert(SourceFamily::Voice, MemoryVisibility::Private);
        let p = VisibilityPolicy {
            floor: Some(MemoryVisibility::Session),
            override_for_source: overrides,
        };
        let s = serde_json::to_string(&p).expect("serialize");
        let back: VisibilityPolicy = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, p);
    }

    #[test]
    fn policy_rejects_unknown_field() {
        let err = serde_json::from_str::<VisibilityPolicy>(r#"{"unknown": true}"#);
        assert!(err.is_err(), "unknown_fields must be denied");
    }
}
