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

/// Per-vault narrowing overrides for the default visibility matrix.
///
/// Stored under `_policy.yaml` per brief §3.0; the parser lives in
/// `crate::config` (this module accepts the parsed struct only). All
/// fields default to `None`/empty, in which case [`default_visibility`]
/// returns the hard-coded matrix value.
///
/// **Both fields are narrowing-only.** The Filter stage never broadens
/// visibility — promotion above the matrix default requires an audited
/// `consent.log` entry per §14, and that path lives outside this
/// module. A configuration that tries to broaden via either field is
/// silently clamped back to the matrix value rather than silently
/// promoting the capture without consent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisibilityPolicy {
    /// Optional **upper bound** the resolved default is clamped down to.
    /// Use this to force a more-private ceiling on the whole vault —
    /// e.g. `ceiling: Private` collapses every capture (including
    /// Auto+sensor's `Session` default) down to `Private`. A `ceiling`
    /// broader than the matrix value is a no-op for that capture; it
    /// can never promote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<MemoryVisibility>,

    /// Per-source narrowing overrides. A present entry **lowers** the
    /// matrix default for that [`SourceFamily`] toward `Private`. An
    /// override broader than the matrix value is silently ignored — the
    /// matrix value wins. After this step, [`Self::ceiling`] applies as
    /// a final clamp toward `Private`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub override_for_source: HashMap<SourceFamily, MemoryVisibility>,
}

/// Resolve the default [`MemoryVisibility`] for a capture (§6.3, §14).
///
/// Resolution order:
///
/// 1. Look up the matrix entry for `(identity_kind, mode, source)`.
/// 2. If `policy.override_for_source` has an entry for `source` and it
///    is **stricter** (more private) than the matrix value, use it.
///    Otherwise keep the matrix value — overrides cannot broaden.
/// 3. If `policy.ceiling` is set and it is **stricter** than the
///    current value, clamp down to it. The ceiling cannot broaden.
///
/// The whole resolution is monotonically non-broadening: the output
/// is always less-or-equal-to the matrix default, never above it.
/// Promotion to broader tiers requires a consent.log entry and is
/// performed outside the Filter stage.
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
    let after_override = match policy.override_for_source.get(&source).copied() {
        Some(o) if o < matrix => o,
        _ => matrix,
    };
    match policy.ceiling {
        Some(c) if c < after_override => c,
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

    // ── Policy overrides (narrowing only) ────────────────────────────

    #[test]
    fn ceiling_clamps_downward_only() {
        // ceiling=Private: an Auto+Hook (default Session) gets clamped
        // down to Private. An Explicit+Cli (already Private) stays put.
        let policy = VisibilityPolicy {
            ceiling: Some(MemoryVisibility::Private),
            ..Default::default()
        };
        assert_eq!(
            default_visibility(
                IdentityKind::Sensor,
                CaptureMode::Auto,
                SourceFamily::Hook,
                &policy
            ),
            MemoryVisibility::Private,
        );
        assert_eq!(
            default_visibility(
                IdentityKind::Human,
                CaptureMode::Explicit,
                SourceFamily::Cli,
                &policy
            ),
            MemoryVisibility::Private,
        );
    }

    #[test]
    fn ceiling_cannot_broaden_default() {
        // ceiling=Project applied to Auto+Hook (default Session) is a
        // no-op — Session is already stricter than Project. The Filter
        // stage must never broaden visibility; promotion lives behind
        // the audited consent.log path (§14).
        let policy = VisibilityPolicy {
            ceiling: Some(MemoryVisibility::Project),
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
            "ceiling broader than matrix must not promote"
        );
        // Same for Explicit + Cli (Private). A Public ceiling would not
        // broaden it either.
        let public_ceiling = VisibilityPolicy {
            ceiling: Some(MemoryVisibility::Public),
            ..Default::default()
        };
        assert_eq!(
            default_visibility(
                IdentityKind::Human,
                CaptureMode::Explicit,
                SourceFamily::Cli,
                &public_ceiling
            ),
            MemoryVisibility::Private,
        );
    }

    #[test]
    fn source_override_only_narrows() {
        // Auto+Clipboard defaults to Session. An override to Private is
        // narrower → applies. An override to Project would be broader
        // → silently ignored, default preserved.
        let mut narrow = HashMap::new();
        narrow.insert(SourceFamily::Clipboard, MemoryVisibility::Private);
        let policy = VisibilityPolicy {
            override_for_source: narrow,
            ..Default::default()
        };
        assert_eq!(
            default_visibility(
                IdentityKind::Sensor,
                CaptureMode::Auto,
                SourceFamily::Clipboard,
                &policy
            ),
            MemoryVisibility::Private,
        );

        let mut broaden = HashMap::new();
        broaden.insert(SourceFamily::Hook, MemoryVisibility::Project);
        let policy = VisibilityPolicy {
            override_for_source: broaden,
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
            "broadening source override must be ignored"
        );
    }

    #[test]
    fn override_and_ceiling_both_narrow_compose() {
        // Override Hook (Session) down to Project — but Project is
        // broader than Session, so override is ignored. Then ceiling
        // Private clamps the matrix value (Session) down to Private.
        // Net effect: Private.
        let mut overrides = HashMap::new();
        overrides.insert(SourceFamily::Hook, MemoryVisibility::Project);
        let policy = VisibilityPolicy {
            ceiling: Some(MemoryVisibility::Private),
            override_for_source: overrides,
        };
        assert_eq!(
            default_visibility(
                IdentityKind::Sensor,
                CaptureMode::Auto,
                SourceFamily::Hook,
                &policy
            ),
            MemoryVisibility::Private,
        );
    }

    #[test]
    fn no_policy_combination_can_broaden_matrix() {
        // Property: for every (kind, mode, source) and every policy
        // combination from the six visibility tiers, the resolved
        // default is never broader than the unpoliced matrix value.
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
        let tiers = [
            MemoryVisibility::Private,
            MemoryVisibility::Session,
            MemoryVisibility::Project,
            MemoryVisibility::Team,
            MemoryVisibility::Org,
            MemoryVisibility::Public,
        ];
        let baseline = VisibilityPolicy::default();
        for k in kinds {
            for m in modes {
                for s in sources {
                    let unpoliced = default_visibility(k, m, s, &baseline);
                    for ceiling in tiers {
                        for override_tier in tiers {
                            let mut overrides = HashMap::new();
                            overrides.insert(s, override_tier);
                            let p = VisibilityPolicy {
                                ceiling: Some(ceiling),
                                override_for_source: overrides,
                            };
                            let resolved = default_visibility(k, m, s, &p);
                            assert!(
                                resolved <= unpoliced,
                                "policy broadened {k:?},{m:?},{s:?}: \
                                 unpoliced={unpoliced:?} resolved={resolved:?} \
                                 ceiling={ceiling:?} override={override_tier:?}"
                            );
                        }
                    }
                }
            }
        }
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
    fn policy_round_trips_with_ceiling_and_overrides() {
        let mut overrides = HashMap::new();
        overrides.insert(SourceFamily::Voice, MemoryVisibility::Private);
        let p = VisibilityPolicy {
            ceiling: Some(MemoryVisibility::Session),
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
