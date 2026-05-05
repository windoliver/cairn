//! Capture → Extract dispatch driver (issue #217).
//!
//! Decides per [`CaptureEvent`] whether the payload bytes flow through
//! `squash` (lossy text compaction) or **bypass** (raw
//! bytes to Extract). The decision is derived entirely from the
//! persisted [`CapturePayload`] — no caller-supplied side input — so
//! WAL replay reproduces the same routing the dispatch driver took at
//! capture time (brief §5.2; spec
//! `docs/superpowers/specs/2026-04-27-issue-72-tool-squash-design.md`,
//! "Caller contract" §).
//!
//! P0 dispatch table (from the spec):
//!
//! | `CapturePayload` variant + context              | Decision                                   |
//! |-------------------------------------------------|--------------------------------------------|
//! | `Terminal { context: InteractiveTty }`          | [`Squash`](DispatchDecision::Squash)       |
//! | `Terminal { context: NonInteractiveOrStructured }` | `Bypass(TerminalNonInteractive)`        |
//! | `Terminal { context: None }` (pre-#218 legacy)  | `Bypass(TerminalLegacyMissingContext)`     |
//! | `Hook` / `Ide` / `Clipboard` / `Voice` /         | `Bypass(NonTerminalFamily)`                |
//! | `Screen` / `RecordingBatch` / `Cli` / `Mcp` /    |                                            |
//! | `Proactive`                                     |                                            |
//!
//! [`ToolSchemaLookup`] is the registry hook that lets a deployment
//! force a [`SourceFamily`] to bypass `squash` even when the default
//! table would route it through. The override surface is intentionally
//! **family-granular and bypass-only at P0**: the squash entry point
//! is crate-private and only accepts a [`SourceFamily::Terminal`]
//! payload with `context: InteractiveTty`, so letting a registry
//! produce [`Squash`](DispatchDecision::Squash) for non-terminal
//! payloads would dead-end at that boundary.
//! Per-tool granularity is held back deliberately so the wire-form
//! `pipeline_dispatch` advertisement (one entry per `SourceFamily`)
//! cannot diverge from the routing the dispatch driver actually
//! performs at capture time. Widening the override to per-tool keys
//! requires growing the wire schema in lockstep; that is a follow-up.
//!
//! [`DefaultRegistry`] returns `None` for every key at P0; the dispatch
//! table above is the sole authority.

use crate::domain::capture::{CaptureEvent, CapturePayload, SourceFamily, TerminalContext};
use crate::generated::status::{
    StatusResponsePipelineDispatch, StatusResponsePipelineDispatchDecision,
    StatusResponsePipelineDispatchSourceFamily,
};

/// Routing decision for a single [`CaptureEvent`] reaching the
/// Capture → Extract boundary.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DispatchDecision {
    /// Admit to `squash`. The variant carries a
    /// sealed [`SquashAdmission`] token; the caller forwards that
    /// token to
    /// `squash::UnstructuredTextBytes::try_from_terminal_event`
    /// to enter the lossy compaction path. The token is constructible
    /// only inside this module, so the squash entrypoint cannot be
    /// reached without going through [`dispatch`] first.
    Squash(SquashAdmission),
    /// Bypass `squash` — raw payload bytes flow to
    /// Extract unchanged. The variant carries [`BypassReason`] so the
    /// pipeline driver and observability can distinguish the four P0
    /// bypass causes without re-deriving them from the event.
    Bypass(BypassReason),
}

/// Capability token proving that [`dispatch`] returned
/// [`DispatchDecision::Squash`] for the event being squashed.
///
/// Has a private field so external callers cannot construct one —
/// the only producer is `dispatch()`. This makes the squash
/// entrypoint
/// (`squash::UnstructuredTextBytes::try_from_terminal_event`)
/// reachable only via the dispatch path: a caller cannot decide
/// "squash this terminal payload" without first asking the registry-
/// aware dispatch function whether it is squash-eligible.
///
/// **Not** `Copy` / `Clone`: the token is consumed by-value when the
/// caller invokes
/// `squash::UnstructuredTextBytes::try_from_terminal_event`,
/// so each squash entry requires its own [`dispatch`] call. Otherwise
/// a caller could obtain one admission from a squash-eligible event
/// and reuse it to push unrelated terminal payloads through `squash`,
/// silently bypassing a `RegistryOverride` that turned squash off for
/// that family.
///
/// Opaque so future PRs may attach metadata (per-event scope, cfg
/// overrides) without breaking the surface.
#[derive(Debug, PartialEq, Eq)]
pub struct SquashAdmission {
    _seal: (),
}

impl SquashAdmission {
    /// Mint a fresh admission token. **Module-private** so only
    /// [`dispatch`] inside this file can produce one; no other
    /// `cairn-core` module — present or future — can construct an
    /// admission and bypass the registry-aware decision. Tests that
    /// need a token use the `cfg(test)`-gated `Self::for_test` helper
    /// below.
    fn new() -> Self {
        Self { _seal: () }
    }

    /// Test-only token mint. `pub(crate)` so squash/fuzz/bench tests
    /// can synthesise an admission for fixture work; gated behind
    /// `cfg(test)` so production code (within or outside this crate)
    /// cannot reach it.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self { _seal: () }
    }
}

/// Why the dispatch driver routed an event around `squash`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BypassReason {
    /// `SourceFamily` is not `Terminal`. P0 unconditionally bypasses
    /// every `Hook`, `Ide`, `Clipboard`, `Voice`, `Screen`,
    /// `RecordingBatch`, `Cli`, `Mcp`, and `Proactive` payload (spec
    /// dispatch table).
    NonTerminalFamily,
    /// `Terminal` payload whose
    /// [`context`](crate::domain::capture::TerminalContext) is
    /// [`NonInteractiveOrStructured`](crate::domain::capture::TerminalContext::NonInteractiveOrStructured).
    /// Lossy compaction would corrupt machine-readable bytes (JSON,
    /// CSV, structured-output flags); the dispatch driver bypasses
    /// unconditionally.
    TerminalNonInteractive,
    /// `Terminal` payload from a pre-#218 writer whose `context` field
    /// is `None`. Distinct from [`TerminalNonInteractive`] so the
    /// pipeline driver / replay path can either migrate the event or
    /// surface a `needs migration` signal rather than silently treating
    /// the legacy data as deliberately structured.
    ///
    /// [`TerminalNonInteractive`]: BypassReason::TerminalNonInteractive
    TerminalLegacyMissingContext,
    /// A [`ToolSchemaLookup`] override forced this [`SourceFamily`] to
    /// bypass `squash`. Used when a deployment
    /// disables squash for an otherwise squash-eligible family (today,
    /// `Terminal`).
    RegistryOverride,
    /// `event.validate()` failed: the envelope's `source_family`
    /// disagrees with the payload variant, the actor chain is empty,
    /// hashes mismatch, etc. Bypass is the only safe routing because
    /// the squash entrypoint would reject the same event with a
    /// validation error and minting an admission for a malformed
    /// envelope would split the routing decision from the safety
    /// check. Extract retains its own malformed-envelope handling.
    MalformedEnvelope,
}

/// Per-[`SourceFamily`] bypass override registry.
///
/// **P0 surface is family-granular and bypass-only.** Implementations
/// may return `Some(reason)` to force every event of a given
/// [`SourceFamily`] to bypass `squash`; they cannot
/// force a payload *into* `squash`, because the only verified squash
/// entry point
/// (`squash::UnstructuredTextBytes::try_from_terminal_event`)
/// validates `CapturePayload::Terminal { context: InteractiveTty }`.
///
/// Per-tool granularity is held back deliberately. The wire-form
/// `pipeline_dispatch` advertisement on `cairn status` is keyed on
/// `SourceFamily` only, so a per-tool override would let the routing
/// the dispatch driver performs at capture time silently diverge from
/// what `status` advertises (the `(Terminal, "Bash") → Bypass`
/// scenario flagged in review). Promoting the contract to per-tool
/// requires growing the wire schema in lockstep with the trait; that
/// is a #217 follow-up.
///
/// The trait stays object-safe so callers can hold a
/// `&dyn ToolSchemaLookup` if they need to compose registries at
/// runtime.
pub trait ToolSchemaLookup {
    /// Force a [`Bypass`](DispatchDecision::Bypass) decision for every
    /// event of this [`SourceFamily`]. Return `None` to defer to the
    /// default dispatch table; return `Some(reason)` to bypass with
    /// that labelled reason.
    fn bypass_override(&self, family: SourceFamily) -> Option<BypassReason>;
}

/// P0 default registry — every family returns `None`, so [`dispatch`]
/// falls back to the spec's hard-coded table.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultRegistry;

impl ToolSchemaLookup for DefaultRegistry {
    fn bypass_override(&self, _family: SourceFamily) -> Option<BypassReason> {
        None
    }
}

/// Dispatch a single [`CaptureEvent`].
///
/// Pure function: same `event` always returns the same decision.
/// Reads only `event.payload`; never inspects the bytes referenced by
/// `event.payload_ref` (those flow to `squash` or to
/// Extract after the decision).
///
/// The registry only intercepts the would-be-[`Squash`](DispatchDecision::Squash)
/// path. Intrinsic bypass reasons —
/// [`TerminalNonInteractive`](BypassReason::TerminalNonInteractive),
/// [`TerminalLegacyMissingContext`](BypassReason::TerminalLegacyMissingContext),
/// [`NonTerminalFamily`](BypassReason::NonTerminalFamily) — are
/// preserved verbatim so a family-wide override cannot mask the
/// migration signal a pre-#218 terminal payload carries (its
/// `context: None`) or relabel a deliberately structured payload as a
/// generic policy decision. The override surface is family-granular
/// and bypass-only at P0; a registry cannot promote a non-terminal
/// payload to [`Squash`](DispatchDecision::Squash).
#[must_use]
pub fn dispatch<R: ToolSchemaLookup + ?Sized>(
    event: &CaptureEvent,
    registry: &R,
) -> DispatchDecision {
    // Validate the envelope before minting any decision so the
    // squash admission cannot be issued for a malformed event whose
    // safety invariants the squash binder will later reject. Keeping
    // the validation co-located with the routing decision means the
    // dispatch result is authoritative for replay paths too.
    if event.validate().is_err() {
        return DispatchDecision::Bypass(BypassReason::MalformedEnvelope);
    }
    let family = event.payload.source_family();
    match &event.payload {
        CapturePayload::Terminal { context, .. } => match context {
            // Only the squash-eligible terminal path defers to the
            // registry. Bypassing here loses no signal because the
            // alternative was Squash, not a labelled bypass reason.
            Some(TerminalContext::InteractiveTty) => match registry.bypass_override(family) {
                Some(reason) => DispatchDecision::Bypass(reason),
                None => DispatchDecision::Squash(SquashAdmission::new()),
            },
            // Intrinsic terminal-state reasons take precedence over
            // any registry policy: a family-wide override that bypasses
            // Terminal must NOT erase the legacy-context migration
            // signal or the structured-output reason.
            Some(TerminalContext::NonInteractiveOrStructured) => {
                DispatchDecision::Bypass(BypassReason::TerminalNonInteractive)
            }
            None => DispatchDecision::Bypass(BypassReason::TerminalLegacyMissingContext),
        },
        // Non-terminal families always bypass; the registry override
        // here would not change the decision, so we keep
        // `NonTerminalFamily` as the honest reason.
        _ => DispatchDecision::Bypass(BypassReason::NonTerminalFamily),
    }
}

/// Build the `pipeline_dispatch` array advertised by `cairn status`
/// (issue #217). One entry per [`SourceFamily`], `tool_id: None`.
/// Each entry's `decision` is derived from the same
/// `registry.bypass_override(family)` call [`dispatch`] would make for
/// that family:
///
/// * `Some(_)` → advertised as
///   [`Bypass`](StatusResponsePipelineDispatchDecision::Bypass)
///   (the override forces the family-default away from squash).
/// * `None` → advertised as the spec's hard-coded default for that
///   family —
///   [`SquashWhenInteractiveTty`](StatusResponsePipelineDispatchDecision::SquashWhenInteractiveTty)
///   for `Terminal`,
///   [`Bypass`](StatusResponsePipelineDispatchDecision::Bypass) for
///   every other family.
///
/// Because [`ToolSchemaLookup`] is family-granular at P0, the
/// advertisement is **always exact** — no `(family, tool)` override
/// can hide behind a family-default entry. Promoting either surface
/// to per-tool keys requires growing the other in lockstep.
///
/// Entries are sorted lexicographically by `(source_family,
/// tool_id ?? "")` so the wire response is byte-stable across an
/// incarnation (brief §8.0.a).
#[must_use]
pub fn pipeline_dispatch_advertisement<R: ToolSchemaLookup + ?Sized>(
    registry: &R,
) -> Vec<StatusResponsePipelineDispatch> {
    use StatusResponsePipelineDispatchSourceFamily as F;

    // Wire-form (snake_case) lexicographic order matches the IDL
    // sort contract on `(source_family, tool_id ?? "")` — every
    // entry below has tool_id == None, so source_family alone
    // determines order.
    let families = [
        (F::Cli, SourceFamily::Cli),
        (F::Clipboard, SourceFamily::Clipboard),
        (F::Hook, SourceFamily::Hook),
        (F::Ide, SourceFamily::Ide),
        (F::Mcp, SourceFamily::Mcp),
        (F::Proactive, SourceFamily::Proactive),
        (F::RecordingBatch, SourceFamily::RecordingBatch),
        (F::Screen, SourceFamily::Screen),
        (F::Terminal, SourceFamily::Terminal),
        (F::Voice, SourceFamily::Voice),
    ];
    families
        .into_iter()
        .map(|(wire_family, domain_family)| {
            // Same lookup `dispatch()` performs.
            let decision = if registry.bypass_override(domain_family).is_some() {
                StatusResponsePipelineDispatchDecision::Bypass
            } else {
                default_decision_for_family(wire_family)
            };
            StatusResponsePipelineDispatch {
                decision,
                source_family: wire_family,
                tool_id: None,
            }
        })
        .collect()
}

fn default_decision_for_family(
    family: StatusResponsePipelineDispatchSourceFamily,
) -> StatusResponsePipelineDispatchDecision {
    match family {
        StatusResponsePipelineDispatchSourceFamily::Terminal => {
            StatusResponsePipelineDispatchDecision::SquashWhenInteractiveTty
        }
        _ => StatusResponsePipelineDispatchDecision::Bypass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::actor_chain::{ActorChainEntry, ChainRole};
    use crate::domain::capture::{
        CaptureEvent, CaptureEventId, CaptureMode, CaptureRefs, PayloadHash,
    };
    use crate::domain::identity::Identity;
    use crate::domain::timestamp::Rfc3339Timestamp;
    use sha2::{Digest, Sha256};

    fn ts() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-04-27T00:00:00Z").expect("valid timestamp")
    }

    fn payload_hash_of(bytes: &[u8]) -> PayloadHash {
        let digest = Sha256::digest(bytes);
        PayloadHash::parse(format!("sha256:{digest:x}")).expect("valid hash")
    }

    fn terminal_event_with_context(ctx: Option<TerminalContext>, bytes: &[u8]) -> CaptureEvent {
        CaptureEvent {
            event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
            sensor_id: Identity::parse("snr:local:terminal:cli:v1").unwrap(),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("snr:local:terminal:cli:v1").unwrap(),
                at: ts(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some("sess".into()),
                turn_id: Some("turn".into()),
                tool_id: None,
            }),
            payload_hash: payload_hash_of(bytes),
            payload_ref: "sources/terminal/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into(),
            captured_at: ts(),
            payload: CapturePayload::Terminal {
                command: "echo hi".into(),
                exit_code: Some(0),
                context: ctx,
            },
            source_family: SourceFamily::Terminal,
        }
    }

    fn hook_event(bytes: &[u8]) -> CaptureEvent {
        CaptureEvent {
            event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").unwrap(),
            sensor_id: Identity::parse("snr:local:hook:cc-session:v1").unwrap(),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("snr:local:hook:cc-session:v1").unwrap(),
                at: ts(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some("sess".into()),
                turn_id: Some("turn".into()),
                tool_id: None,
            }),
            payload_hash: payload_hash_of(bytes),
            payload_ref: "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.json".into(),
            captured_at: ts(),
            payload: CapturePayload::Hook {
                hook_name: "PostToolUse".into(),
                tool_name: Some("Read".into()),
            },
            source_family: SourceFamily::Hook,
        }
    }

    #[test]
    fn terminal_interactive_tty_routes_to_squash() {
        let bytes = b"hello\n";
        let evt = terminal_event_with_context(Some(TerminalContext::InteractiveTty), bytes);
        assert!(matches!(
            dispatch(&evt, &DefaultRegistry),
            DispatchDecision::Squash(_)
        ));
    }

    #[test]
    fn terminal_non_interactive_bypasses_with_specific_reason() {
        let bytes = b"{\"k\":\"v\"}\n";
        let evt =
            terminal_event_with_context(Some(TerminalContext::NonInteractiveOrStructured), bytes);
        assert_eq!(
            dispatch(&evt, &DefaultRegistry),
            DispatchDecision::Bypass(BypassReason::TerminalNonInteractive)
        );
    }

    #[test]
    fn terminal_legacy_none_bypasses_with_specific_reason() {
        let bytes = b"hello\n";
        let evt = terminal_event_with_context(None, bytes);
        assert_eq!(
            dispatch(&evt, &DefaultRegistry),
            DispatchDecision::Bypass(BypassReason::TerminalLegacyMissingContext)
        );
    }

    #[test]
    fn hook_payload_bypasses_as_non_terminal_family() {
        let bytes = b"{}";
        let evt = hook_event(bytes);
        assert_eq!(
            dispatch(&evt, &DefaultRegistry),
            DispatchDecision::Bypass(BypassReason::NonTerminalFamily)
        );
    }

    #[test]
    fn registry_override_can_force_bypass_on_terminal_interactive() {
        struct ForceBypass;
        impl ToolSchemaLookup for ForceBypass {
            fn bypass_override(&self, _family: SourceFamily) -> Option<BypassReason> {
                Some(BypassReason::RegistryOverride)
            }
        }
        let bytes = b"hello\n";
        let evt = terminal_event_with_context(Some(TerminalContext::InteractiveTty), bytes);
        // Without the override the terminal+interactive_tty event
        // would route to Squash; the override flips it to Bypass.
        assert_eq!(
            dispatch(&evt, &ForceBypass),
            DispatchDecision::Bypass(BypassReason::RegistryOverride)
        );
    }

    /// Regression: a family-wide bypass override on `Terminal` must
    /// NOT mask the legacy/non-interactive bypass reasons. Operators
    /// rely on `TerminalLegacyMissingContext` to detect pre-#218
    /// payloads that need migration during replay; relabeling them as
    /// `RegistryOverride` would silently lose that signal.
    #[test]
    fn registry_override_on_terminal_preserves_intrinsic_reasons() {
        struct BypassTerminal;
        impl ToolSchemaLookup for BypassTerminal {
            fn bypass_override(&self, family: SourceFamily) -> Option<BypassReason> {
                (family == SourceFamily::Terminal).then_some(BypassReason::RegistryOverride)
            }
        }
        let legacy = terminal_event_with_context(None, b"hi");
        assert_eq!(
            dispatch(&legacy, &BypassTerminal),
            DispatchDecision::Bypass(BypassReason::TerminalLegacyMissingContext),
            "legacy missing-context bypass reason must survive a family override"
        );
        let structured =
            terminal_event_with_context(Some(TerminalContext::NonInteractiveOrStructured), b"{}");
        assert_eq!(
            dispatch(&structured, &BypassTerminal),
            DispatchDecision::Bypass(BypassReason::TerminalNonInteractive),
            "structured-output bypass reason must survive a family override"
        );
    }

    /// Regression for review-loop round 4 finding: an override on one
    /// family must not affect dispatch of a different family — the
    /// `pipeline_dispatch` advertisement and the routing the dispatch
    /// driver performs stay in lockstep per-family.
    #[test]
    fn registry_override_is_scoped_to_its_family() {
        struct BypassTerminalOnly;
        impl ToolSchemaLookup for BypassTerminalOnly {
            fn bypass_override(&self, family: SourceFamily) -> Option<BypassReason> {
                (family == SourceFamily::Terminal).then_some(BypassReason::RegistryOverride)
            }
        }
        let term = terminal_event_with_context(Some(TerminalContext::InteractiveTty), b"hi");
        assert_eq!(
            dispatch(&term, &BypassTerminalOnly),
            DispatchDecision::Bypass(BypassReason::RegistryOverride)
        );
        let hook = hook_event(b"{}");
        assert_eq!(
            dispatch(&hook, &BypassTerminalOnly),
            DispatchDecision::Bypass(BypassReason::NonTerminalFamily)
        );
    }

    #[test]
    fn default_registry_returns_none_for_every_family() {
        let reg = DefaultRegistry;
        for family in [
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
        ] {
            assert!(reg.bypass_override(family).is_none());
        }
    }

    #[test]
    fn default_advertisement_covers_every_family_exactly_once() {
        let adv = pipeline_dispatch_advertisement(&DefaultRegistry);
        assert_eq!(adv.len(), 10, "one entry per SourceFamily variant");
        let mut families: Vec<_> = adv.iter().map(|e| e.source_family).collect();
        families.sort_by_key(|f| format!("{f:?}"));
        families.dedup_by_key(|f| format!("{f:?}"));
        assert_eq!(families.len(), 10, "no duplicate families");
    }

    #[test]
    fn default_advertisement_terminal_is_squash_others_bypass() {
        let adv = pipeline_dispatch_advertisement(&DefaultRegistry);
        for entry in &adv {
            let want = if matches!(
                entry.source_family,
                StatusResponsePipelineDispatchSourceFamily::Terminal
            ) {
                StatusResponsePipelineDispatchDecision::SquashWhenInteractiveTty
            } else {
                StatusResponsePipelineDispatchDecision::Bypass
            };
            assert_eq!(
                entry.decision, want,
                "{:?} should advertise {:?}",
                entry.source_family, want
            );
            assert!(entry.tool_id.is_none(), "P0 advertises family-wide only");
        }
    }

    #[test]
    fn default_advertisement_is_sorted_lexicographically_by_wire_family() {
        let adv = pipeline_dispatch_advertisement(&DefaultRegistry);
        let wire_keys: Vec<String> = adv
            .iter()
            .map(|e| {
                serde_json::to_value(e.source_family)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let mut sorted = wire_keys.clone();
        sorted.sort();
        assert_eq!(
            wire_keys, sorted,
            "advertisement must be sorted by wire-form source_family for byte-stable status (brief §8.0.a)"
        );
    }

    /// Regression for review-loop round 1 finding: status
    /// advertisement is derived from the same registry call
    /// `dispatch()` makes. A bypass override on `Terminal` MUST flip
    /// its advertised decision from `SquashWhenInteractiveTty` to
    /// `Bypass`.
    #[test]
    fn advertisement_reflects_registry_bypass_override_on_terminal() {
        struct BypassTerminal;
        impl ToolSchemaLookup for BypassTerminal {
            fn bypass_override(&self, family: SourceFamily) -> Option<BypassReason> {
                (family == SourceFamily::Terminal).then_some(BypassReason::RegistryOverride)
            }
        }
        let adv = pipeline_dispatch_advertisement(&BypassTerminal);
        let terminal_entry = adv
            .iter()
            .find(|e| e.source_family == StatusResponsePipelineDispatchSourceFamily::Terminal)
            .expect("terminal entry");
        assert_eq!(
            terminal_entry.decision,
            StatusResponsePipelineDispatchDecision::Bypass,
            "bypass override on Terminal must flip the advertised decision"
        );
        // Other families fall through to defaults.
        let hook_entry = adv
            .iter()
            .find(|e| e.source_family == StatusResponsePipelineDispatchSourceFamily::Hook)
            .expect("hook entry");
        assert_eq!(
            hook_entry.decision,
            StatusResponsePipelineDispatchDecision::Bypass
        );
    }

    /// Regression: a malformed envelope (here, `source_family`
    /// disagrees with the payload variant) must NOT receive a squash
    /// admission. Routing the event onto the lossy path and then
    /// rejecting it inside the squash binder would split the safety
    /// check from the decision; dispatch must reject such events at
    /// the boundary.
    #[test]
    fn malformed_envelope_bypasses_with_specific_reason() {
        let bytes = b"hello\n";
        let mut evt = terminal_event_with_context(Some(TerminalContext::InteractiveTty), bytes);
        // Intentional contradiction: payload says Terminal, envelope
        // says Hook. `event.validate()` rejects this.
        evt.source_family = SourceFamily::Hook;
        assert!(evt.validate().is_err(), "fixture must be invalid");
        assert_eq!(
            dispatch(&evt, &DefaultRegistry),
            DispatchDecision::Bypass(BypassReason::MalformedEnvelope)
        );
    }

    /// Lock in the default-table dispatch invariant: same event always
    /// produces the same decision (replay determinism).
    #[test]
    fn dispatch_is_deterministic() {
        let bytes = b"hello\n";
        let evt = terminal_event_with_context(Some(TerminalContext::InteractiveTty), bytes);
        let a = dispatch(&evt, &DefaultRegistry);
        let b = dispatch(&evt, &DefaultRegistry);
        assert_eq!(a, b);
    }
}
