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
///    is **a safe narrowing** of the matrix value, use it. Otherwise
///    keep the matrix value — overrides cannot broaden, and Session →
///    Private is *not* a safe narrowing (see [`is_safe_narrowing`]).
/// 3. If `policy.ceiling` is set and it is a safe narrowing of the
///    current value, clamp down to it. The same Session-vs-Private
///    asymmetry applies.
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
        Some(o) if is_safe_narrowing(matrix, o) => o,
        _ => matrix,
    };
    match policy.ceiling {
        Some(c) if is_safe_narrowing(after_override, c) => c,
        _ => after_override,
    }
}

/// Predicate: is moving from `from` to `to` a safe narrowing the
/// Filter stage may apply without a consent.log entry?
///
/// `MemoryVisibility` derives `PartialOrd` over the variant order
/// `Private < Session < Project < Team < Org < Public`, which is
/// almost-but-not-quite an audience ordering: at the `Private`
/// boundary the ordering inverts in *time*. A `Session` capture is
/// turn-local — it disappears when the session ends. A `Private`
/// capture persists in the vault and is queryable by every future
/// turn the same owner runs (§6.3, "never leaves the vault without
/// explicit promotion"). The visibility module's matrix comment
/// states this directly: promoting a sensor observation from
/// `Session` to `Private` "would silently widen scope from this turn
/// to vault-wide". A vault `_policy.yaml` must therefore not be able
/// to perform that transition either — codex round 7 caught this as
/// a real bypass: a benign-looking `ceiling: Private` could quietly
/// promote every sensor observation to vault-wide persistence.
///
/// Concretely: any narrowing within `[Project, Team, Org, Public]` is
/// safe, and so is narrowing those to `Private` or `Session`. But the
/// `Session ↔ Private` step is treated as **incomparable** — the
/// Filter stage refuses to apply it in either direction without
/// consent.
fn is_safe_narrowing(from: MemoryVisibility, to: MemoryVisibility) -> bool {
    if matches!(
        (from, to),
        (MemoryVisibility::Session, MemoryVisibility::Private)
            | (MemoryVisibility::Private, MemoryVisibility::Session)
    ) {
        // Session ↔ Private is the incomparable pair — refuse.
        return false;
    }
    if from == to {
        // Same-tier clamp is a no-op; safe.
        return true;
    }
    // Outside the Session/Private boundary the derived ordering
    // (`Private < Session < Project < Team < Org < Public`) faithfully
    // tracks audience-narrowing direction.
    to < from
}

/// Hard-coded matrix used by [`default_visibility`] before policy
/// overrides apply.
///
/// Mode-driven rules (brief §5.0.a / §6.3):
/// - **Auto** + a real sensor family (`hook`, `ide`, `terminal`,
///   `clipboard`, `voice`, `screen`, `recording_batch`) **emitted by
///   a sensor identity** → `Session`. Sensor observations are turn-
///   local; promoting them to `Private` would silently widen scope
///   from "this turn" to "vault-wide".
/// - **Auto** without a sensor family (`cli`, `mcp`, `proactive`) →
///   `Private`. Auto from these channels means the harness fired ingest
///   without explicit user action; treat as agent state.
/// - **Explicit** (Mode B) emitted by a human identity → `Private`.
///   The user said "remember this"; the user must also say "share
///   this" later.
/// - **Proactive** (Mode C) emitted by an agent identity → `Private`.
///   Agent-generated facts must clear consent before they leave the
///   vault.
///
/// **Fail-closed on impossible triples (§14).** Identity kind is not
/// just informational — the matrix enforces the §5.0.a attribution
/// pairing here too, so a malformed call (e.g. `(Human, Auto, Hook)`
/// or `(Agent, Auto, Screen)`) cannot accidentally land at the legal
/// sensor-default tier because some upstream caller forgot to
/// validate. Codex round 8 caught a related case: collapsing every
/// malformed triple to `Private` actually broadens the lifetime of
/// sensor-shaped data from "this turn" to "vault-wide" (Session and
/// Private are incomparable, see [`is_safe_narrowing`]). For
/// malformed triples we therefore preserve the *lifetime* property of
/// the source family: sensor sources stay at `Session` (turn-local),
/// non-sensor sources fail closed at `Private`.
//
// The legal `(Sensor, Auto)` arm and the malformed-triple wildcard
// share the same `sensor_default(source)` body intentionally: legal
// triples land at the matrix value, and malformed triples land at
// the same lifetime-preserving fallback. Both arms are kept for
// documentation rather than collapsing into the wildcard.
#[allow(clippy::match_same_arms)]
fn matrix_lookup(
    identity_kind: IdentityKind,
    mode: CaptureMode,
    source: SourceFamily,
) -> MemoryVisibility {
    match (identity_kind, mode) {
        (IdentityKind::Sensor, CaptureMode::Auto) => sensor_default(source),
        // Legal §5.0.a pairings: Explicit must come from a human
        // identity; Proactive from an agent identity. Both default
        // to Private — the user (B) or agent (C) must clear consent
        // to promote.
        (IdentityKind::Human, CaptureMode::Explicit)
        | (IdentityKind::Agent, CaptureMode::Proactive) => MemoryVisibility::Private,
        // Impossible identity/mode triples (e.g. Human+Auto+Hook,
        // Sensor+Explicit+Hook). Preserve the lifetime bound for
        // sensor-shaped sources: clamping a sensor-source capture to
        // Private would silently turn turn-local data into vault-wide
        // persistence. Non-sensor sources keep the Private fallback.
        _ => sensor_default(source),
    }
}

/// Default tier for a source family treated as a *sensor* shape:
/// `Hook`, `Ide`, `Terminal`, `Clipboard`, `Voice`, `Screen`,
/// `RecordingBatch` → `Session`. Non-sensor (`Cli`, `Mcp`,
/// `Proactive`) → `Private`. Used by [`matrix_lookup`] for the legal
/// `(Sensor, Auto, …)` arm and the malformed-triple fallback so
/// sensor-shaped data preserves its turn-local lifetime even when
/// upstream validation has failed.
const fn sensor_default(source: SourceFamily) -> MemoryVisibility {
    match source {
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
        // ceiling=Private on Auto+Hook (default Session) is *not* a
        // safe narrowing — the codex round-7 finding: Session is
        // turn-local, Private is vault-wide-persistent, so this swap
        // would silently broaden the lifetime even though it narrows
        // the audience. The clamp is rejected and Auto+Hook stays
        // Session. ceiling=Private on Explicit+Cli (already Private)
        // is a same-tier no-op.
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
            MemoryVisibility::Session,
            "Session→Private clamp must be rejected (lifetime broadens)"
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
    fn ceiling_private_on_session_capture_is_rejected() {
        // §14: a vault `_policy.yaml` cannot promote a session-scoped
        // sensor observation into vault-wide persistence by setting
        // `ceiling: Private`. The Filter stage refuses the clamp.
        let policy = VisibilityPolicy {
            ceiling: Some(MemoryVisibility::Private),
            ..Default::default()
        };
        for source in [
            SourceFamily::Hook,
            SourceFamily::Ide,
            SourceFamily::Terminal,
            SourceFamily::Clipboard,
            SourceFamily::Voice,
            SourceFamily::Screen,
            SourceFamily::RecordingBatch,
        ] {
            assert_eq!(
                default_visibility(IdentityKind::Sensor, CaptureMode::Auto, source, &policy),
                MemoryVisibility::Session,
                "ceiling=Private illegally clamped Auto+{source:?} to Private"
            );
        }
    }

    #[test]
    fn override_session_to_private_is_rejected() {
        // The Session ↔ Private incomparability also applies to the
        // per-source override path.
        let mut overrides = HashMap::new();
        overrides.insert(SourceFamily::Hook, MemoryVisibility::Private);
        let policy = VisibilityPolicy {
            override_for_source: overrides,
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
            "override Session→Private must be rejected"
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
        // *not* a safe narrowing per the Session/Private incomparability
        // (codex round 7) — Private would broaden lifetime from
        // turn-local to vault-wide. The override is rejected. An
        // override to Project would broaden audience and is also
        // rejected.
        let mut overrides_to_private = HashMap::new();
        overrides_to_private.insert(SourceFamily::Clipboard, MemoryVisibility::Private);
        let policy = VisibilityPolicy {
            override_for_source: overrides_to_private,
            ..Default::default()
        };
        assert_eq!(
            default_visibility(
                IdentityKind::Sensor,
                CaptureMode::Auto,
                SourceFamily::Clipboard,
                &policy
            ),
            MemoryVisibility::Session,
            "override Session→Private must be rejected (lifetime broadens)"
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
    fn source_override_actually_narrows_for_project_capture() {
        // A capture that lands above Private/Session in the matrix
        // (e.g. promoted Project tier) can be narrowed by an override
        // to Project / Team / Org / Public going downward, and the
        // narrowing is honored.
        let mut overrides = HashMap::new();
        overrides.insert(SourceFamily::Cli, MemoryVisibility::Private);
        let policy = VisibilityPolicy {
            override_for_source: overrides,
            ..Default::default()
        };
        // Explicit+Cli defaults to Private; the override Private→Private
        // is a same-tier no-op.
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
    fn override_and_ceiling_both_narrow_compose() {
        // Override Hook (matrix=Session) down to Project — Project is
        // broader, override is ignored. Then ceiling Private would
        // also be rejected because Session→Private is incomparable.
        // Net effect: Session is preserved.
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
            MemoryVisibility::Session,
            "Session→Private clamp must be rejected via either override or ceiling"
        );
    }

    // ── Fail-closed on impossible identity/mode triples ─────────────

    #[test]
    fn malformed_identity_mode_pairs_preserve_lifetime_bound() {
        // The §5.0.a attribution rules pair Auto with Sensor,
        // Explicit with Human, Proactive with Agent. Any other pair
        // is malformed. Codex round 8: collapsing every malformed
        // triple to Private silently broadens lifetime for
        // sensor-shaped sources from turn-local to vault-wide.
        // Sensor-shaped sources stay at Session; non-sensor sources
        // fail closed at Private.
        let policy = VisibilityPolicy::default();
        let sensor_sources = [
            SourceFamily::Hook,
            SourceFamily::Ide,
            SourceFamily::Terminal,
            SourceFamily::Clipboard,
            SourceFamily::Voice,
            SourceFamily::Screen,
            SourceFamily::RecordingBatch,
        ];
        let non_sensor_sources = [
            SourceFamily::Cli,
            SourceFamily::Mcp,
            SourceFamily::Proactive,
        ];
        for (k, m) in [
            (IdentityKind::Human, CaptureMode::Auto),
            (IdentityKind::Agent, CaptureMode::Auto),
            (IdentityKind::Sensor, CaptureMode::Explicit),
            (IdentityKind::Agent, CaptureMode::Explicit),
            (IdentityKind::Sensor, CaptureMode::Proactive),
            (IdentityKind::Human, CaptureMode::Proactive),
        ] {
            for s in sensor_sources {
                assert_eq!(
                    default_visibility(k, m, s, &policy),
                    MemoryVisibility::Session,
                    "malformed triple {k:?},{m:?},{s:?} should stay session-scoped \
                     for sensor-shaped sources, not promote to vault-wide Private"
                );
            }
            for s in non_sensor_sources {
                assert_eq!(
                    default_visibility(k, m, s, &policy),
                    MemoryVisibility::Private,
                    "malformed triple {k:?},{m:?},{s:?} should fail closed at Private \
                     for non-sensor sources"
                );
            }
        }
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
