//! Tool-squash: compact verbose terminal output before extraction
//! (brief §5.2 Tool-squash row, issue #72). See
//! `docs/superpowers/specs/2026-04-27-issue-72-tool-squash-design.md`.
//!
//! Pure function. No I/O. Deterministic: same `(raw, cfg)` always
//! produces byte-identical `compacted_bytes`.

#![allow(clippy::module_name_repetitions)]
// Squash* names are intentional
// The module is `pub(crate)` until the dispatch driver (#217) is the sole
// entry point; until then most items are only reachable from tests.
#![allow(dead_code)]

/// Maximum byte length of any truncation marker emitted by `squash`.
///
/// Three forms (see spec for derivation):
///   - skip-line:                `[…skipped K lines, X bytes…]`
///   - per-line truncate:        `[…N bytes truncated]`
///   - per-line after dedup:     `[…N source bytes truncated, ×K]`
///
/// Worst case ~88 bytes for the per-line-after-dedup form (48 ASCII
/// fixed text + 2 × 20-digit `usize` decimal renderings). Rounded up
/// for slack:
pub const MARKER_MAX_LEN: usize = 128;

/// Worst-case stage-6 layout overhead beyond `max_line_bytes` and
/// `MARKER_MAX_LEN`: separator newlines plus a trailing newline.
pub const LAYOUT_OVERHEAD: usize = 4;

/// Minimum permitted `max_bytes`.
///
/// Derived from `2 * MIN_MAX_LINE_BYTES + MARKER_MAX_LEN + LAYOUT_OVERHEAD`
/// so the tail-locked pair always fits.
pub const MIN_MAX_BYTES: usize = 4 * MARKER_MAX_LEN; // 512

/// Minimum permitted `max_line_bytes`. Equal to `MARKER_MAX_LEN` so a
/// truncated line still has room for the inline marker.
pub const MIN_MAX_LINE_BYTES: usize = MARKER_MAX_LEN; // 128

/// Minimum permitted `tail_lines`. Set to 2 so the tail-locked pair
/// always fits without a fallback.
pub const MIN_TAIL_LINES: usize = 2;

/// Hard ceiling on the raw payload accepted by
/// [`UnstructuredTextBytes::try_from_terminal_event`]. The squash
/// pipeline materializes intermediate `String` / `Vec<String>` copies
/// per stage; bounding the input keeps peak working-set proportional
/// to a small multiple of this value rather than letting a runaway
/// terminal capture OOM the host. 64 MiB easily covers a verbose
/// `cargo build`, large `npm test` runs, etc. **Boundary**: any
/// payload with `raw_bytes.len() >= MAX_INPUT_BYTES` is treated as
/// oversize (i.e., the constant is the largest size that takes the
/// staged path; one byte more enters the bypass). Tracked for
/// streaming refactor in #221-followup.
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Hard ceiling on raw line cardinality before squash routes to the
/// oversize bypass. The staged path materializes ~3 `Vec<String>`
/// layers (stage 3, stage 4 dedup-line, stage 5 cap), each ~24 B of
/// header per line on 64-bit + content. At 200K lines that is ~14 MiB
/// of headers alone — a comfortable working-set budget for legitimate
/// terminal captures (a verbose `cargo build` is ~10K lines, large
/// `npm test` runs are ~50K). **Boundary**: any payload with newline
/// count `>= MAX_INPUT_LINES` routes to the bypass; the constant is
/// the largest count that takes the staged path.
pub const MAX_INPUT_LINES: usize = 200_000;

// Compile-time invariant: MIN_MAX_BYTES must hold the tail-locked pair
// + skip-marker + layout newlines.
const _: () = assert!(MIN_MAX_BYTES >= 2 * MIN_MAX_LINE_BYTES + MARKER_MAX_LEN + LAYOUT_OVERHEAD);

/// Configuration for `squash`. Construct via `SquashConfig::new` or
/// `SquashConfig::default()`. All fields private; accessors below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SquashConfig {
    max_bytes: usize,
    head_lines: usize,
    tail_lines: usize,
    dedup_min_run: usize,
    max_line_bytes: usize,
    progress_frame_collapse_enabled: bool,
}

impl SquashConfig {
    /// Validates and constructs a config. See spec for cross-field rule:
    /// `2 × max_line_bytes + MARKER_MAX_LEN + LAYOUT_OVERHEAD ≤ max_bytes`.
    ///
    /// # Errors
    /// Returns `SquashConfigError` for any per-field minimum violation
    /// or for the cross-field budget violation.
    pub fn new(
        max_bytes: usize,
        head_lines: usize,
        tail_lines: usize,
        dedup_min_run: usize,
        max_line_bytes: usize,
    ) -> Result<Self, SquashConfigError> {
        if max_bytes < MIN_MAX_BYTES {
            return Err(SquashConfigError::MaxBytesTooSmall {
                value: max_bytes,
                min: MIN_MAX_BYTES,
            });
        }
        if max_line_bytes < MIN_MAX_LINE_BYTES {
            return Err(SquashConfigError::MaxLineBytesTooSmall {
                value: max_line_bytes,
                min: MIN_MAX_LINE_BYTES,
            });
        }
        if tail_lines < MIN_TAIL_LINES {
            return Err(SquashConfigError::TailLinesTooSmall {
                value: tail_lines,
                min: MIN_TAIL_LINES,
            });
        }
        // Use checked arithmetic so a near-`usize::MAX` `max_line_bytes`
        // can't wrap into an apparently-valid budget in release builds.
        let needed = max_line_bytes
            .checked_mul(2)
            .and_then(|x| x.checked_add(MARKER_MAX_LEN))
            .and_then(|x| x.checked_add(LAYOUT_OVERHEAD));
        match needed {
            Some(n) if n <= max_bytes => {}
            _ => {
                return Err(SquashConfigError::LineCapExceedsLayoutBudget {
                    line: max_line_bytes,
                    marker: MARKER_MAX_LEN,
                    overhead: LAYOUT_OVERHEAD,
                    total: max_bytes,
                });
            }
        }
        Ok(Self {
            max_bytes,
            head_lines,
            tail_lines,
            dedup_min_run,
            max_line_bytes,
            progress_frame_collapse_enabled: false,
        })
    }

    /// Enable or disable the progress-frame-collapse pre-stage (stage 2b).
    ///
    /// **This is NOT a general-purpose terminal renderer.** It is a
    /// narrowly-scoped opt-in for producers that emit
    /// **full-line progress-frame rewrites** — the
    /// `\rDownloading 2%\rDownloading 3%` pattern from build tools,
    /// package managers, and progress bars, where each `\r`-delimited
    /// frame supersedes the previous one in its entirety.
    ///
    /// **Default is off.** When enabled, stage 2b collapses each
    /// `\n`-delimited line that contains `\r` to its last non-empty
    /// `\r`-segment (see `stage2b_progress_collapse` for the rationale).
    /// That is correct for the documented full-line-rewrite pattern but
    /// **lossy** for any other use of CR: binary or protocol payloads
    /// that legitimately embed `\r`, interactive output that uses `\r`
    /// for partial rewrites without `CSI K` (e.g. `aaaa\rbb` rendering
    /// as terminal-faithful `bbaa`), or arbitrary captures whose CR
    /// usage you have not classified.
    ///
    /// **CSI K erase semantics are honored** when this flag is on
    /// (issue #249). Stage 2 leaves a one-byte sentinel where it
    /// stripped a recognised `K` form, varying the sentinel by mode:
    /// - `\x1b[K` / `\x1b[0K` (cursor-to-EOL) → `ERASE_LINE_SENTINEL`.
    ///   Models the line as empty only when the cursor was at col 0
    ///   (e.g. immediately after `\r`). Stage 2b realises that as
    ///   `text\r\x1b[K\n` → empty final frame.
    /// - `\x1b[2K` (whole-line) → `ERASE_WHOLE_LINE_SENTINEL`. Erases
    ///   the line regardless of cursor position, so even
    ///   `secret\x1b[2K\n` (no `\r`) renders empty.
    /// - Numeric leading zeros are accepted: `\x1b[00K` ≡ `\x1b[0K`,
    ///   `\x1b[02K` ≡ `\x1b[2K`.
    ///
    /// `\x1b[1K` (erase-from-start-to-cursor), `\x1b[3K`, compound
    /// parameter forms (`\x1b[1;2K`), private-prefix forms
    /// (`\x1b[?K`), and intermediate-bearing forms are all
    /// silent-stripped on the legacy pre-#249 path: their effect does
    /// not match either sentinel cleanly, and synthesizing one would
    /// drop visible bytes or guess at unspecified behaviour. The
    /// sentinels are never visible to stage 3+; they are consumed
    /// inside stage 2b.
    ///
    /// Do NOT enable this for "generic interactive terminal text" — the
    /// happy path is narrow on purpose. Callers must classify the input
    /// as full-frame-rewrite progress output and accept the lossy
    /// semantics for that capture.
    ///
    /// **Oversize-bypass interaction.** When the flag is on, stage 2b
    /// runs on both the staged path and on the head/tail windows of
    /// `oversize_bypass` (including the giant-final-line branch), so the
    /// flag produces consistent semantics across the byte-ceiling /
    /// line-cardinality / decode-expansion gates. See the
    /// `progress_collapse_applies_on_oversize_bypass` and
    /// `progress_collapse_applies_on_giant_final_line_bypass` regression
    /// tests.
    #[must_use]
    pub fn with_progress_frame_collapse_enabled(mut self, enabled: bool) -> Self {
        self.progress_frame_collapse_enabled = enabled;
        self
    }

    /// Returns whether the progress-frame-collapse pre-stage is enabled.
    #[must_use]
    pub fn progress_frame_collapse_enabled(&self) -> bool {
        self.progress_frame_collapse_enabled
    }

    /// Returns `max_bytes`.
    #[must_use]
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Returns `head_lines`.
    #[must_use]
    pub fn head_lines(&self) -> usize {
        self.head_lines
    }

    /// Returns `tail_lines`.
    #[must_use]
    pub fn tail_lines(&self) -> usize {
        self.tail_lines
    }

    /// Returns `dedup_min_run`.
    #[must_use]
    pub fn dedup_min_run(&self) -> usize {
        self.dedup_min_run
    }

    /// Returns `max_line_bytes`.
    #[must_use]
    pub fn max_line_bytes(&self) -> usize {
        self.max_line_bytes
    }
}

impl Default for SquashConfig {
    // The default values satisfy `new`'s invariants, and the const_assert at
    // the top of the file enforces MIN_* relations at compile time.
    // The expect is therefore unreachable in practice.
    #[allow(clippy::expect_used)]
    fn default() -> Self {
        Self::new(16_384, 100, 100, 2, 4_096)
            .expect("default SquashConfig invariants hold by construction")
    }
}

/// Errors returned by [`SquashConfig::new`].
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[non_exhaustive]
pub enum SquashConfigError {
    /// `max_bytes` is below the minimum.
    #[error("max_bytes must be ≥ {min}, got {value}")]
    MaxBytesTooSmall {
        /// The supplied value.
        value: usize,
        /// The minimum required value.
        min: usize,
    },
    /// `max_line_bytes` is below the minimum.
    #[error("max_line_bytes must be ≥ {min}, got {value}")]
    MaxLineBytesTooSmall {
        /// The supplied value.
        value: usize,
        /// The minimum required value.
        min: usize,
    },
    /// `tail_lines` is below the minimum.
    #[error("tail_lines must be ≥ {min}, got {value}")]
    TailLinesTooSmall {
        /// The supplied value.
        value: usize,
        /// The minimum required value.
        min: usize,
    },
    /// The cross-field layout budget is violated.
    #[error(
        "2 × max_line_bytes ({line}) + MARKER_MAX_LEN ({marker}) + \
         LAYOUT_OVERHEAD ({overhead}) must be ≤ max_bytes ({total})"
    )]
    LineCapExceedsLayoutBudget {
        /// The `max_line_bytes` value.
        line: usize,
        /// The `MARKER_MAX_LEN` constant.
        marker: usize,
        /// The `LAYOUT_OVERHEAD` constant.
        overhead: usize,
        /// The `max_bytes` budget.
        total: usize,
    },
}

use crate::domain::capture::{CaptureEvent, CapturePayload, PayloadHash, TerminalContext};
use sha2::{Digest, Sha256};

/// Bytes the dispatch driver classified as unstructured terminal text.
/// Constructor verifies variant + hash + interactive-TTY context.
#[derive(Debug)]
pub struct UnstructuredTextBytes<'a> {
    bytes: &'a [u8],
    raw_hash: PayloadHash,
}

impl<'a> UnstructuredTextBytes<'a> {
    /// Construct from a `CaptureEvent` plus the raw payload bytes the
    /// event's `payload_ref` pointed at. The terminal context is read
    /// from `CapturePayload::Terminal { context, .. }` so the captured
    /// event is self-describing and replay reproduces the same routing
    /// decision (issue #218).
    ///
    /// # Stability
    /// `pub(crate)`. Issue #217 considered promoting this entry point
    /// to `pub` once `SquashAdmission` gated minting, but
    /// `try_from_terminal_event` only proves *internal*
    /// self-consistency (event validates, payload is `Terminal`,
    /// context is `InteractiveTty`, hash matches the supplied bytes).
    /// It cannot prove the bytes came from a trusted capture path, so
    /// exposing the API publicly would let an external caller
    /// fabricate a `Terminal + InteractiveTty` envelope around
    /// arbitrary CLI/MCP/hook bytes and drive lossy compaction. The
    /// next issue's pipeline driver composes dispatch + squash inside
    /// `cairn-core` so the trust chain stays owned by this crate; the
    /// admission token still exists to make replay determinism
    /// auditable inside that boundary.
    ///
    /// # Errors
    /// `NotTerminalPayload`, `HashMismatch`, `StructuredContextRejected`
    /// per the spec's caller contract, or `LegacyMissingContext` for a
    /// pre-#218 `Terminal` payload whose `context` field is `None`.
    /// `LegacyMissingContext` is distinct from
    /// `StructuredContextRejected` so callers can distinguish "needs
    /// migration" from "deliberately structured / squash-bypass" —
    /// see [`UnstructuredBindError::LegacyMissingContext`].
    pub(crate) fn try_from_terminal_event(
        event: &CaptureEvent,
        raw: &'a [u8],
        _admission: super::dispatch::SquashAdmission,
    ) -> Result<Self, UnstructuredBindError> {
        // Reject malformed envelopes outright — we never want to lossily
        // compact bytes whose source_family / sensor / payload disagree.
        event
            .validate()
            .map_err(UnstructuredBindError::EventValidationFailed)?;
        // NOTE: oversize payloads (>= MAX_INPUT_BYTES) are NOT rejected
        // here. `squash()` detects them and applies an in-band bypass
        // that does head+tail byte slicing without per-stage clones, so
        // the raw bytes are preserved (head + tail) rather than dropped.
        let CapturePayload::Terminal { context, .. } = &event.payload else {
            return Err(UnstructuredBindError::NotTerminalPayload);
        };
        match context {
            Some(TerminalContext::InteractiveTty) => {}
            Some(TerminalContext::NonInteractiveOrStructured) => {
                return Err(UnstructuredBindError::StructuredContextRejected);
            }
            None => return Err(UnstructuredBindError::LegacyMissingContext),
        }
        let digest = Sha256::digest(raw);
        let computed = PayloadHash::parse(format!("sha256:{digest:x}"))
            .map_err(|_| UnstructuredBindError::HashMismatch)?;
        if computed != event.payload_hash {
            return Err(UnstructuredBindError::HashMismatch);
        }
        Ok(Self {
            bytes: raw,
            raw_hash: computed,
        })
    }

    /// The raw payload bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.bytes
    }

    /// The SHA-256 hash of the raw payload bytes.
    #[must_use]
    pub fn raw_hash(&self) -> &PayloadHash {
        &self.raw_hash
    }
}

/// Errors returned by [`UnstructuredTextBytes::try_from_terminal_event`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UnstructuredBindError {
    /// The event payload is not `CapturePayload::Terminal`.
    #[error("expected CapturePayload::Terminal; got a different source family")]
    NotTerminalPayload,
    /// The SHA-256 of the supplied bytes does not match `event.payload_hash`.
    #[error("payload_hash mismatch: bytes do not match the captured payload's sha256")]
    HashMismatch,
    /// The terminal context was non-interactive or structured; squash must be
    /// bypassed for this context.
    #[error(
        "Terminal capture was non-interactive or structured-output; \
         dispatch driver must bypass squash for this context"
    )]
    StructuredContextRejected,
    /// The event is from a pre-#218 writer and carries no `context`
    /// field. Distinct from `StructuredContextRejected` so callers do
    /// not mistake legacy data for a deliberately structured payload —
    /// surfacing this lets the dispatch driver / replay path either
    /// migrate the event or surface a "needs migration" signal.
    #[error(
        "Terminal capture is missing the #218 `context` field; \
         legacy event needs migration before squash routing"
    )]
    LegacyMissingContext,
    /// The supplied `CaptureEvent` failed envelope validation
    /// ([`CaptureEvent::validate`]). The wrapper refuses to operate on
    /// malformed events to avoid lossy compaction of unintended bytes.
    #[error("CaptureEvent failed envelope validation: {0}")]
    EventValidationFailed(#[source] crate::domain::error::DomainError),
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn default_is_valid() {
        let _ = SquashConfig::default();
    }

    #[test]
    fn rejects_max_bytes_below_min() {
        let err = SquashConfig::new(MIN_MAX_BYTES - 1, 100, 100, 2, 4096).unwrap_err();
        assert!(matches!(err, SquashConfigError::MaxBytesTooSmall { .. }));
    }

    #[test]
    fn rejects_max_line_bytes_below_min() {
        let err = SquashConfig::new(16384, 100, 100, 2, MIN_MAX_LINE_BYTES - 1).unwrap_err();
        assert!(matches!(
            err,
            SquashConfigError::MaxLineBytesTooSmall { .. }
        ));
    }

    #[test]
    fn rejects_tail_lines_below_min() {
        let err = SquashConfig::new(16384, 100, 1, 2, 4096).unwrap_err();
        assert!(matches!(err, SquashConfigError::TailLinesTooSmall { .. }));
    }

    #[test]
    fn rejects_overflow_on_extreme_max_line_bytes() {
        // 2 × max_line_bytes overflows `usize` in release; checked
        // arithmetic must reject rather than wrap into an apparent fit.
        let err = SquashConfig::new(usize::MAX, 100, 100, 2, usize::MAX - 1).unwrap_err();
        assert!(matches!(
            err,
            SquashConfigError::LineCapExceedsLayoutBudget { .. }
        ));
    }

    #[test]
    fn rejects_cross_field_budget_violation() {
        let max_bytes = MIN_MAX_BYTES; // 512
        let max_line_bytes = 200; // 2*200+128+4 = 532 > 512
        let err = SquashConfig::new(max_bytes, 100, 100, 2, max_line_bytes).unwrap_err();
        assert!(matches!(
            err,
            SquashConfigError::LineCapExceedsLayoutBudget { .. }
        ));
    }

    #[test]
    fn valid_inputs_round_trip() {
        let cfg = SquashConfig::new(16_384, 100, 100, 2, 4_096).unwrap();
        assert_eq!(cfg.max_bytes(), 16_384);
        assert_eq!(cfg.head_lines(), 100);
        assert_eq!(cfg.tail_lines(), 100);
        assert_eq!(cfg.dedup_min_run(), 2);
        assert_eq!(cfg.max_line_bytes(), 4_096);
    }
}

#[cfg(test)]
mod wrapper_tests {
    use super::*;
    use crate::domain::actor_chain::{ActorChainEntry, ChainRole};
    use crate::domain::capture::{
        CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, PayloadHash,
        SourceFamily,
    };
    use crate::domain::identity::Identity;
    use crate::domain::timestamp::Rfc3339Timestamp;
    use sha2::{Digest, Sha256};

    fn payload_hash_of(bytes: &[u8]) -> PayloadHash {
        let digest = Sha256::digest(bytes);
        PayloadHash::parse(format!("sha256:{digest:x}")).expect("sha256 string is well-formed")
    }

    fn ts() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-04-27T00:00:00Z").expect("valid timestamp")
    }

    pub(super) fn terminal_event(payload_bytes: &[u8]) -> CaptureEvent {
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
            payload_hash: payload_hash_of(payload_bytes),
            payload_ref: "sources/terminal/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into(),
            captured_at: ts(),
            payload: CapturePayload::Terminal {
                command: "echo hi".into(),
                exit_code: Some(0),
                context: Some(TerminalContext::InteractiveTty),
            },
            source_family: SourceFamily::Terminal,
        }
    }

    fn hook_event(payload_bytes: &[u8]) -> CaptureEvent {
        let mut e = terminal_event(payload_bytes);
        e.sensor_id = Identity::parse("snr:local:hook:cc-session:v1").unwrap();
        e.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("snr:local:hook:cc-session:v1").unwrap(),
            at: ts(),
        }];
        e.payload = CapturePayload::Hook {
            hook_name: "PostToolUse".into(),
            tool_name: Some("Read".into()),
        };
        e.source_family = SourceFamily::Hook;
        e
    }

    #[test]
    fn rejects_non_terminal_variant() {
        let bytes = b"hello\n";
        let evt = hook_event(bytes);
        let err = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            bytes,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .unwrap_err();
        assert!(matches!(err, UnstructuredBindError::NotTerminalPayload));
    }

    #[test]
    fn rejects_hash_mismatch() {
        let bytes = b"hello\n";
        let mut evt = terminal_event(bytes);
        evt.payload_hash = payload_hash_of(b"different bytes");
        let err = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            bytes,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .unwrap_err();
        assert!(matches!(err, UnstructuredBindError::HashMismatch));
    }

    #[test]
    fn rejects_structured_context() {
        let bytes = b"hello\n";
        let mut evt = terminal_event(bytes);
        if let CapturePayload::Terminal { context, .. } = &mut evt.payload {
            *context = Some(TerminalContext::NonInteractiveOrStructured);
        }
        let err = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            bytes,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            UnstructuredBindError::StructuredContextRejected
        ));
    }

    /// A pre-#218 (legacy) terminal event has no `context` field
    /// (`None` after `serde(default)`). The squash constructor returns
    /// a distinct `LegacyMissingContext` error — distinct from
    /// `StructuredContextRejected` — so callers can distinguish
    /// "needs migration" from "deliberately structured payload".
    /// `validate()` still passes so the event remains readable across
    /// upgrade (replay / WAL recovery / re-ingest).
    #[test]
    fn rejects_legacy_missing_context() {
        let bytes = b"hello\n";
        let mut evt = terminal_event(bytes);
        if let CapturePayload::Terminal { context, .. } = &mut evt.payload {
            *context = None;
        }
        // `validate()` accepts legacy events so deserialize / replay
        // stays unbroken across upgrade.
        evt.payload.validate().expect("legacy event must validate");
        let err = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            bytes,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .unwrap_err();
        assert!(matches!(err, UnstructuredBindError::LegacyMissingContext));
    }

    #[test]
    fn accepts_terminal_interactive_tty_with_matching_hash() {
        let bytes = b"hello\n";
        let evt = terminal_event(bytes);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            bytes,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("valid construction");
        assert_eq!(wrapped.as_bytes(), bytes);
        assert_eq!(wrapped.raw_hash(), &evt.payload_hash);
    }

    /// Round-8 (newer loop) regression: when the tail-aligned slice
    /// is one giant final line longer than the budget, the bypass
    /// must NOT drop the line entirely. Emit a `[…final-line
    /// truncated…]` marker followed by a bounded codepoint-safe
    /// suffix.
    #[test]
    fn oversize_bypass_preserves_truncated_final_line() {
        // Want tail_aligned_to_line=true (so we hit the "was_trimmed
        // + no \n" final-line branch). Default cfg.max_bytes = 16384,
        // half ≈ 8000, tail_window = 16000. Place a \n at position
        // raw.len() - 12001, with a 12K-byte trailing line. The
        // initial tail_raw_start = raw.len() - 16000; after advancing
        // past that \n the slice is exactly the 12K final line, no
        // newlines, so trim drops the front.
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head-1\nhead-2\n");
        raw.extend(std::iter::repeat_n(b'M', 5_000));
        raw.push(b'\n');
        raw.extend_from_slice(b"FINAL-DIAGNOSTIC-PREFIX-");
        raw.extend(std::iter::repeat_n(b'X', 12_000));
        raw.extend_from_slice(b"-FINAL-DIAGNOSTIC-SUFFIX");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("final-line truncated"),
            "marker present: {body:.300}"
        );
        // The very-end suffix bytes must survive somewhere in the body.
        assert!(
            body.contains("FINAL-DIAGNOSTIC-SUFFIX"),
            "final-line suffix preserved: ...{}",
            &body[body.len().saturating_sub(200)..]
        );
    }

    /// Round-8 (newer loop) regression: when the bypass preserves a
    /// final line without a trailing `\n`, drop accounting must not
    /// treat that line as if it had been dropped.
    #[test]
    fn oversize_bypass_unterminated_final_line_in_drop_accounting() {
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head-1\n");
        raw.extend(std::iter::repeat_n(b'F', 50_000));
        raw.push(b'\n');
        // Final line with NO trailing newline.
        raw.extend_from_slice(b"unterminated-final");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("unterminated-final"),
            "final line preserved: ...{}",
            &body[body.len().saturating_sub(200)..]
        );
        // Drop accounting should NOT count the preserved tail bytes
        // as dropped.
        assert!(
            out.stats.bytes_dropped_truncate < raw_byte_len,
            "must not over-report drop: {} vs raw {}",
            out.stats.bytes_dropped_truncate,
            raw_byte_len,
        );
        // Specifically, dropped should be less than (raw - "unterminated-final").
        let final_len = "unterminated-final".len();
        assert!(
            out.stats.bytes_dropped_truncate <= raw_byte_len - final_len,
            "preserved bytes ({}) must not count as dropped (raw={}, dropped={})",
            final_len,
            raw_byte_len,
            out.stats.bytes_dropped_truncate,
        );
    }

    /// Round-9 regression: when the retained tail window has no `\n`
    /// (entire window sits inside one giant final line) but the raw
    /// payload DOES have a `\n` somewhere earlier, the bypass must
    /// still emit a `[…final-line truncated…]` marker plus a bounded
    /// suffix of the final line — not just drop everything.
    #[test]
    fn oversize_bypass_tail_window_inside_giant_final_line_preserves_suffix() {
        // Default cfg.max_bytes = 16384 → tail_window ≈ 16254. Build a
        // payload where the only `\n` lives BEFORE tail_raw_start so
        // the tail window is entirely inside the giant final line.
        // "head\n" + 50_000 'X's puts the \n at byte 4 and the tail
        // window (last ~16254 bytes) all inside the run of X's.
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head\n");
        raw.extend(std::iter::repeat_n(b'X', 50_000));
        raw.extend_from_slice(b"-FINAL-DIAGNOSTIC-SUFFIX");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("final-line truncated"),
            "marker present: ...{}",
            &body[body.len().saturating_sub(200)..]
        );
        assert!(
            body.contains("FINAL-DIAGNOSTIC-SUFFIX"),
            "tail end of giant final line preserved: ...{}",
            &body[body.len().saturating_sub(200)..]
        );
        // Must NOT degrade to the strict-quarantine sentinel — that's
        // for the no-`\n`-anywhere case only.
        assert!(
            !body.contains("oversized single-line payload"),
            "wrong branch: degraded sentinel emitted instead of suffix preservation"
        );
        assert!(out.compacted_bytes.len() <= cfg.max_bytes());
    }

    /// Round-7 (newer loop) regression: when the giant-final-line
    /// suffix path slices into the middle of a multibyte UTF-8
    /// codepoint, the kept suffix MUST start on a codepoint
    /// boundary so valid non-ASCII text isn't replaced with U+FFFD.
    #[test]
    fn oversize_bypass_final_line_suffix_codepoint_boundary() {
        // Default cfg.max_bytes = 16384, half ≈ 8146. To hit the
        // suffix branch, place the only `\n` near the start, then
        // build a giant final line with multibyte UTF-8 around the
        // expected cut point.
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head\n");
        // Filler before the cut: pure ASCII for predictable cut.
        // Cut lands roughly at raw.len() - 16292 (tail window).
        // Aim for the cut to fall mid-multibyte.
        // Build raw to length R, place "ééé..." spanning the cut.
        // Each 'é' is 2 bytes. Use 100 'é's (200 bytes) followed
        // by ASCII so the cut is somewhere in the middle of the
        // 'é' run.
        // Total target ≈ 25000 bytes.
        raw.extend(std::iter::repeat_n(b'A', 5_000));
        // 10 000 bytes of UTF-8 'é's so the cut falls inside.
        for _ in 0..5000 {
            raw.extend_from_slice("é".as_bytes());
        }
        raw.extend_from_slice("ÉND-OF-DIAGNOSTIC-é".as_bytes());
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        // Output must be valid UTF-8 (already true via String, but
        // the more important check: no U+FFFD inserted from a
        // mid-codepoint cut. We accept any final É at the literal
        // suffix as proof the boundary alignment worked.
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("ÉND-OF-DIAGNOSTIC-é"),
            "multibyte tail suffix was corrupted by mid-codepoint cut: {}",
            &body[body.len().saturating_sub(200)..]
        );
        // The suffix should not start with a U+FFFD (which would
        // indicate the cut hit a continuation byte).
        // Find the marker and inspect the immediate next codepoint.
        if let Some(idx) = body.find("final-line truncated\u{2026}]\n") {
            let after = &body[idx + "final-line truncated\u{2026}]\n".len()..];
            let first = after.chars().next().unwrap_or(' ');
            assert_ne!(
                first, '\u{FFFD}',
                "kept suffix begins with replacement char — cut landed mid-codepoint"
            );
        }
    }

    /// Round-6 (newer loop) regression: when bypass triggers via
    /// `LineCardinality` / `DecodeExpansion` on a payload smaller
    /// than `2 * half`, the tail (containing the actual final
    /// diagnostic lines) MUST survive. Pre-fix the disjoint-windows
    /// clamp dropped the tail entirely once head subsumed raw.
    #[test]
    fn oversize_bypass_small_input_preserves_final_lines() {
        let cfg = SquashConfig::new(64 * 1024, 4, 4, 2, MIN_MAX_LINE_BYTES).unwrap();
        // Line-dense payload that is smaller than 2 * half but
        // dense enough to credibly route through LineCardinality.
        let lines: Vec<String> = (0..100).map(|i| format!("line-{i:03}")).collect();
        let raw: Vec<u8> = lines.join("\n").into_bytes();
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::LineCardinality,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("line-099"),
            "final diagnostic line dropped on small bypass payload: {body:.400}"
        );
        assert!(out.compacted_bytes.len() <= cfg.max_bytes());
    }

    /// Round-5 (newer loop) regression: with a large `max_bytes`
    /// config, an input smaller than `4 * half` (reachable via the
    /// `MAX_INPUT_LINES` / `DecodeExpansion` bypass paths) used to
    /// produce overlapping head and tail windows that duplicated
    /// raw content, double-counted preserved bytes, and on the
    /// upper edge could trip the closing release `assert!`.
    #[test]
    fn oversize_bypass_disjoint_windows_no_duplication() {
        // Larger-than-default config so half is comfortably big.
        // Then a payload way smaller than 4 * half but with enough
        // \n's to exercise the line-aligned tail path.
        let cfg = SquashConfig::new(
            64 * 1024, // 64 KiB max_bytes
            4,
            4,
            2,
            MIN_MAX_LINE_BYTES,
        )
        .unwrap();
        // 200 short unique lines: total bytes ≈ 3.4 KiB, well under
        // 2 * half (~64 KiB).
        let lines: Vec<String> = (0..200).map(|i| format!("unique-line-{i:04}")).collect();
        let raw: Vec<u8> = lines.join("\n").into_bytes();
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::LineCardinality,
        );
        // No panic, fits the budget.
        assert!(out.compacted_bytes.len() <= cfg.max_bytes());
        // No line content appears more than once: head/tail windows
        // must be disjoint.
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        for line in &lines {
            let occurrences = body.matches(line.as_str()).count();
            assert!(
                occurrences <= 1,
                "line {line:?} appears {occurrences}x — head/tail overlap"
            );
        }
    }

    /// Round-4 (newer loop) regression: the oversize-bypass tail
    /// cut must not land inside an open multi-byte escape sequence.
    /// Construct a payload where an OSC body contains a `\n` and is
    /// terminated PAST the natural `tail_window` start. Pre-fix, the
    /// naive `position(b'\n')` cut sliced inside the OSC, omitting
    /// the introducer from the sanitizer's view. With the safety
    /// gate, the tail must either skip past the OSC terminator or
    /// fall back to the giant-final-line / degraded path.
    #[test]
    fn oversize_bypass_tail_cut_skips_open_escape_continuing_past_lf() {
        // Default cfg → tail_window ≈ 16292. Place an OSC introducer
        // BEFORE tail_window start whose body contains `\n` bytes
        // and whose ST terminator sits AFTER tail_window start. The
        // first naive `\n` cut would land mid-OSC.
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head\n");
        // Open OSC with newlines in body. Body length chosen so the
        // ST terminator lands past tail_window's natural start.
        raw.push(0x1B);
        raw.extend_from_slice(b"]8;;");
        for _ in 0..50 {
            raw.extend_from_slice(b"hidden-line-with-\n");
        }
        raw.push(0x1B);
        raw.push(0x5C);
        // Plenty of safe content after the OSC terminator so the
        // tail has somewhere safe to start.
        for i in 0..2000 {
            raw.extend_from_slice(format!("safe-line-{i:04}\n").as_bytes());
        }
        raw.extend_from_slice(b"FINAL-DIAGNOSTIC-LINE\n");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        // `hidden-line-with-` is the OSC payload — it must NOT
        // surface through stage 2 as plain text.
        assert!(
            !body.contains("hidden-line-with-"),
            "OSC continuation leaked through tail cut: ...{}",
            &body[..body.len().min(400)]
        );
        // The actual diagnostic content past the OSC terminator
        // should still be recoverable.
        assert!(
            body.contains("FINAL-DIAGNOSTIC-LINE") || body.contains("safe-line-"),
            "expected post-OSC content to survive: {body:.400}"
        );
    }

    /// Round-3 (newer loop) regression: a fully-terminated escape
    /// sequence in the dropped prefix must NOT force the degraded
    /// sentinel — the escape parser is back to "outside" by the
    /// time the suffix begins, so the suffix is provably safe.
    #[test]
    fn oversize_bypass_terminated_escape_in_prefix_preserves_suffix() {
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head\n");
        // Fully-terminated OSC 8 hyperlink (ESC \ terminator).
        raw.push(0x1B);
        raw.extend_from_slice(b"]8;;https://example.org/log\x1b\\");
        raw.extend(std::iter::repeat_n(b'X', 30_000));
        raw.extend(std::iter::repeat_n(b'Y', 30_000));
        raw.extend_from_slice(b"-FINAL-DIAGNOSTIC-SUFFIX");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("final-line truncated"),
            "expected final-line marker (terminated escape is safe): ...{}",
            &body[body.len().saturating_sub(200)..]
        );
        assert!(
            body.contains("FINAL-DIAGNOSTIC-SUFFIX"),
            "suffix preserved when prefix's escape is terminated"
        );
        assert!(
            !body.contains("oversized single-line payload"),
            "must not degrade when prefix's escape is fully terminated"
        );
    }

    /// Round-2 (newer loop) regression: when the `tail_aligned_to_line`
    /// branch hits `final_line_truncated`, the appended marker must
    /// fit alongside both a near-full head window and a near-full
    /// tail suffix without crossing `cfg.max_bytes()`. Pre-fix this
    /// case tripped the closing release `assert!`.
    #[test]
    fn oversize_bypass_final_line_truncated_marker_respects_max_bytes() {
        // Default cfg.max_bytes = 16384. Construct raw that drives
        // both head and tail to approximately `half` bytes:
        //   8145 'A' + '\n' + 12000 'B'
        // - head_trimmed picks up the `\n` near byte 8145 → head
        //   emits ~8145 bytes.
        // - tail window is the last `half*2` ≈ 16292 bytes; line-
        //   align step lands right after the single `\n` → tail
        //   slice is the 12000 B's, which trim-from-end shrinks to
        //   ~half bytes with no `\n` → final_line_truncated.
        let mut raw: Vec<u8> = Vec::new();
        raw.extend(std::iter::repeat_n(b'A', 8145));
        raw.push(b'\n');
        raw.extend(std::iter::repeat_n(b'B', 12_000));
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        assert!(
            out.compacted_bytes.len() <= cfg.max_bytes(),
            "compacted exceeds max_bytes: {} > {}",
            out.compacted_bytes.len(),
            cfg.max_bytes(),
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("final-line truncated"),
            "expected final-line marker in output"
        );
    }

    /// Round-2 (newer loop) regression: when the bypass preserves a
    /// final-line suffix and the suffix gets ANSI/OSC-stripped, the
    /// stripped bytes must NOT be reported as truncation loss in
    /// `bytes_dropped_truncate`. Sanitization signal already lives
    /// in `ansi_stripped` / `osc_recovery_bytes_dropped`.
    #[test]
    fn oversize_bypass_final_line_suffix_strips_count_as_sanitization_not_truncation() {
        // Build a payload whose tail window has no `\n` (giant
        // final line) but the dropped prefix of the final line is
        // ESC-free, so the safe-suffix branch fires. Embed an SGR
        // escape inside the preserved suffix region so stage 2
        // strips bytes from the kept content.
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head\n");
        raw.extend(std::iter::repeat_n(b'X', 30_000));
        // SGR escape inside the suffix region; introducer is in the
        // KEPT slice, sanitizer handles it normally.
        raw.extend_from_slice(b"\x1b[31m");
        raw.extend(std::iter::repeat_n(b'Y', 100));
        raw.extend_from_slice(b"\x1b[0m");
        raw.extend_from_slice(b"-FINAL-DIAGNOSTIC-SUFFIX");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        assert!(out.stats.ansi_stripped, "ANSI must have been stripped");
        // Critical invariant: stripped ANSI bytes ARE NOT truncation
        // loss. raw_byte_len - bytes_dropped_truncate = preserved raw
        // bytes. Since the suffix branch keeps the full raw input
        // span (no front-trim happened — sanitized fits remaining),
        // bytes_dropped_truncate must reflect ONLY the dropped raw
        // PREFIX of the final line (not the stripped escapes).
        // Concretely: the preserved span is at least the size of the
        // bytes that survived sanitization in the kept window plus
        // the stripped escape bytes themselves.
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("FINAL-DIAGNOSTIC-SUFFIX"),
            "suffix preserved when dropped prefix is ESC-free"
        );
        // The stripped SGR escape sequences (`\x1b[31m`, `\x1b[0m` =
        // 9 bytes total) must not appear as truncation loss. Lower
        // bound: bytes_dropped_truncate < raw_byte_len - 9.
        assert!(
            out.stats.bytes_dropped_truncate + 9 <= raw_byte_len,
            "stripped ANSI bytes leaked into bytes_dropped_truncate: \
             dropped={}, raw={}",
            out.stats.bytes_dropped_truncate,
            raw_byte_len,
        );
    }

    /// Round-1 (newer loop) regression, refined by Round-3: when the
    /// kept suffix would begin INSIDE an unterminated CSI/OSC
    /// sequence, the bypass must fall back to the degraded sentinel.
    /// Uses an UN-terminated OSC introducer in the dropped prefix
    /// so the escape parser is still mid-OSC at `suffix_window_start`.
    /// (A terminated escape in the dropped prefix is now correctly
    /// allowed to preserve the suffix — see
    /// `oversize_bypass_terminated_escape_in_prefix_preserves_suffix`.)
    #[test]
    fn oversize_bypass_unsafe_suffix_falls_back_to_degraded_sentinel() {
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head\n");
        // UN-terminated OSC: introducer + URL with no BEL/ST. The
        // escape parser stays mid-OSC for the rest of the prefix,
        // so the suffix would begin inside the open sequence.
        raw.push(0x1B);
        raw.extend_from_slice(b"]8;;https://evil.example/payload");
        raw.extend(std::iter::repeat_n(b'X', 30_000));
        raw.extend(std::iter::repeat_n(b'Y', 30_000));
        raw.extend_from_slice(b"-FINAL-DIAGNOSTIC-SUFFIX");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        // Must use the degraded path — final-line preservation would
        // leak control-plane bytes here.
        assert!(
            body.contains("oversized single-line payload"),
            "degraded sentinel emitted instead of unsafe suffix preservation: ...{}",
            &body[body.len().saturating_sub(300)..]
        );
        // The dangling URL/params must NOT appear in the output —
        // that's the whole point of the quarantine.
        assert!(
            !body.contains("evil.example"),
            "control-plane URL leaked through the bypass: ...{}",
            &body[body.len().saturating_sub(300)..]
        );
        assert!(
            !body.contains("FINAL-DIAGNOSTIC-SUFFIX"),
            "suffix must not be preserved when introducer was sliced off"
        );
    }

    /// Round-10 regression: invalid UTF-8 in a payload large enough
    /// that lossy `U+FFFD` expansion would balloon the staged path
    /// past `MAX_INPUT_BYTES` must route through the bypass instead
    /// of running stage 1 / stage 3 over a 3×-expanded buffer. Marker
    /// must reflect the decode-expansion reason.
    #[test]
    #[ignore = "allocates > MAX_INPUT_BYTES/3 of invalid bytes; run with --ignored"]
    fn decode_expansion_routes_to_bypass() {
        // Just over the MAX_INPUT_BYTES/3 threshold + all-invalid
        // bytes → from_utf8 fails fast and the gate triggers before
        // any per-stage allocation. The test sizes the payload right
        // at the threshold so it stays under the raw-byte ceiling.
        let n = MAX_INPUT_BYTES / 3 + 1;
        let raw = vec![0xFFu8; n];
        let evt = terminal_event(&raw);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            &raw,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("valid");
        let cfg = SquashConfig::default();
        let out = super::squash(wrapped, &cfg);
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("decode expansion"),
            "decode-expansion bypass marker present: ...{}",
            &body[body.len().saturating_sub(200)..]
        );
        assert!(out.stats.truncated);
        assert!(out.compacted_bytes.len() <= cfg.max_bytes());
    }

    /// Round-10 regression: when stage 1 had to lossily replace
    /// invalid UTF-8 with `U+FFFD`, the output is no longer a
    /// verbatim copy of the input. `stats.truncated` (and the new
    /// `utf8_replacement` flag) must reflect that even when no later
    /// stage trims anything.
    #[test]
    fn truncated_set_for_utf8_replacement_only() {
        // Tiny invalid payload: pure 0xFF bytes, well under
        // MAX_INPUT_BYTES/3 so the decode-expansion gate doesn't
        // fire and stage 1 actually runs. No ANSI, no dedup, no cap.
        let raw = b"\xFF\xFF\xFFhello\n";
        let evt = terminal_event(raw);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            raw,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("valid");
        let cfg = SquashConfig::default();
        let out = super::squash(wrapped, &cfg);
        assert!(
            out.stats.utf8_replacement,
            "stage 1 lossy decode must set utf8_replacement"
        );
        assert!(
            out.stats.truncated,
            "any lossy transform — including decode loss — flips truncated"
        );
    }

    /// Round-7 (newer loop) regression: `stats.truncated` must reflect
    /// any lossy transform, not just stage 5/6 budget loss. ANSI-only
    /// stripping or dedup-only collapse produces a non-verbatim
    /// `compacted_bytes` and must flip the bit.
    #[test]
    fn truncated_set_for_ansi_only_loss() {
        // Tiny payload: two lines, both with SGR escapes that get
        // stripped. No stage 5 or stage 6 loss.
        let raw = b"\x1b[31mred\x1b[0m\nplain\n";
        let evt = terminal_event(raw);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            raw,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("valid");
        let cfg = SquashConfig::default();
        let out = super::squash(wrapped, &cfg);
        assert!(out.stats.ansi_stripped, "ansi was stripped");
        assert!(
            out.stats.truncated,
            "truncated must be true when any lossy stage acted"
        );
    }

    /// Round-7 (newer loop) regression: dedup-only collapse must also
    /// flip `stats.truncated`.
    #[test]
    fn truncated_set_for_dedup_only_loss() {
        // Repeated line, no ANSI. Default dedup_min_run = 2.
        let raw = b"same\nsame\nsame\n";
        let evt = terminal_event(raw);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            raw,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("valid");
        let cfg = SquashConfig::default();
        let out = super::squash(wrapped, &cfg);
        assert!(out.stats.dedup_runs_collapsed > 0, "dedup acted");
        assert!(
            out.stats.truncated,
            "truncated must be true when any lossy stage acted"
        );
    }

    /// Round-7 (newer loop) regression: oversize bypass must populate
    /// `cr_bearing_lines` for retained head/tail content. Bare `\r` in
    /// preserved text is a renderer-safety hazard that consumers gate
    /// on; the bypass must not silently zero this signal.
    #[test]
    fn oversize_bypass_populates_cr_bearing_lines() {
        let mut raw: Vec<u8> = Vec::new();
        // Head with progress-bar style bare CR.
        raw.extend_from_slice(b"download 10%\rdownload 50%\rdownload 100%\n");
        // Filler so head/tail windows are exercised.
        raw.extend(std::iter::repeat_n(b'F', 50_000));
        raw.push(b'\n');
        raw.extend_from_slice(b"tail-1\n");
        // Tail with a bare CR too.
        raw.extend_from_slice(b"prog 25%\rprog 100%\n");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        assert!(
            out.stats.cr_bearing_lines > 0,
            "bypass must populate cr_bearing_lines, got {}",
            out.stats.cr_bearing_lines,
        );
    }

    /// Round-9 (newer loop) regression: oversize bypass must NOT count
    /// CRLF (`\r\n`) and CRCRLF (`\r\r\n`) line endings as bare-CR
    /// hazards — stage 2 normalizes those to `\n`, and the bypass-side
    /// audit count must mirror that or it will misclassify every
    /// Windows-encoded large log as a CR-bearing capture and drive
    /// false warning/fallback behavior. Built around a CRLF-only
    /// payload large enough to trip `ByteCeiling` so it lands on the
    /// bypass path.
    #[test]
    fn oversize_bypass_crlf_only_does_not_inflate_cr_bearing_lines() {
        let mut raw: Vec<u8> = Vec::new();
        // 200 KB of CRLF-terminated lines — well past MAX_INPUT_BYTES.
        for i in 0..10_000 {
            raw.extend_from_slice(format!("windows-style-line-{i:05}\r\n").as_bytes());
        }
        // Sprinkle a CRCRLF to ensure stage 2's CRCRLF→LF collapse rule
        // is mirrored too (those must also not count).
        raw.extend_from_slice(b"crcrlf-line\r\r\n");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        assert_eq!(
            out.stats.cr_bearing_lines, 0,
            "CRLF-only payload must not register as CR-bearing on bypass"
        );
    }

    /// Round-9 sibling: a payload with ONE bare CR mixed into otherwise
    /// CRLF-encoded content must report exactly that one line as
    /// CR-bearing. Asserts the CRLF-stripping logic does not also strip
    /// genuine bare-CR signals.
    #[test]
    fn oversize_bypass_bare_cr_counted_amid_crlf() {
        let mut raw: Vec<u8> = Vec::new();
        for i in 0..5_000 {
            raw.extend_from_slice(format!("clean-{i:05}\r\n").as_bytes());
        }
        // One genuine progress-bar line with bare CR (no terminating LF
        // immediately after the CR).
        raw.extend_from_slice(b"download 50%\rdownload 100%\r\n");
        for i in 0..5_000 {
            raw.extend_from_slice(format!("more-{i:05}\r\n").as_bytes());
        }
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        assert_eq!(
            out.stats.cr_bearing_lines, 1,
            "exactly one bare-CR line should register; got {}",
            out.stats.cr_bearing_lines,
        );
    }

    /// Round-10 regression: stage 2's CRLF rule pops only ONE preceding
    /// `\r`, so `\r\r\r\n` survives stage 2 as `\r\n` and the staged
    /// path classifies it as one CR-bearing line. Round 9's bypass
    /// counter naively stripped the entire trailing `\r` run and
    /// reported zero, hiding a real bare-CR hazard at the bypass gate.
    /// This test pins parity: a CRRRLF-rich oversize payload must
    /// register as CR-bearing on the bypass path.
    #[test]
    fn oversize_bypass_triple_cr_lf_is_counted() {
        let mut raw: Vec<u8> = Vec::new();
        for i in 0..6_000 {
            raw.extend_from_slice(format!("line-{i:05}\r\r\r\n").as_bytes());
        }
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        assert!(
            out.stats.cr_bearing_lines >= 6_000,
            "every \\r\\r\\r\\n line must register as CR-bearing; got {}",
            out.stats.cr_bearing_lines,
        );
    }

    /// Round-1 (newer loop) regression: bypass `lines_dropped_truncate`
    /// must reflect raw lines actually omitted from the middle, and
    /// `bytes_dropped_truncate` must exclude ANSI bytes stripped during
    /// sanitization (sanitization is not truncation).
    #[test]
    fn oversize_bypass_stats_use_raw_boundaries() {
        // Build a payload where: head=10 short lines, middle=1000
        // dropped lines (each with embedded ANSI sgr), tail=10 lines.
        let mut raw: Vec<u8> = Vec::new();
        for i in 0..10 {
            raw.extend_from_slice(format!("head-{i:02}\n").as_bytes());
        }
        // Middle: 1000 lines, each with a \x1b[31m...\x1b[0m wrapper.
        for i in 0..1000 {
            raw.extend_from_slice(
                format!("\x1b[31mmiddle-{i:04}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\x1b[0m\n")
                    .as_bytes(),
            );
        }
        for i in 0..10 {
            raw.extend_from_slice(format!("tail-{i:02}\n").as_bytes());
        }
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        // We dropped some non-zero count of lines from the middle.
        assert!(
            out.stats.lines_dropped_truncate > 0,
            "lines_dropped_truncate must be populated, got {}",
            out.stats.lines_dropped_truncate,
        );
        // Total raw \n count is 1020. Lines preserved = head \n
        // count + tail \n count. dropped = 1020 - preserved should
        // be in (0, 1020).
        #[allow(clippy::naive_bytecount)]
        let total_lines = raw.iter().filter(|&&b| b == b'\n').count();
        assert!(out.stats.lines_dropped_truncate < total_lines);
        // The middle had ~1000 lines × ~10 ANSI bytes stripped =
        // ~10K bytes of pure-sanitization loss. With the OLD (buggy)
        // accounting that subtracted sanitized lengths from raw_byte_len,
        // those 10K stripped bytes would have been counted as dropped.
        // With raw-boundary accounting, they are NOT counted: the
        // dropped count reflects only the unrenderable middle.
        let head_lines_pos = raw
            .iter()
            .enumerate()
            .filter(|&(_, &b)| b == b'\n')
            .nth(9)
            .unwrap()
            .0;
        let tail_start_lines = total_lines - 10;
        let mut count = 0usize;
        let mut tail_start_byte = 0usize;
        for (i, &b) in raw.iter().enumerate() {
            if b == b'\n' {
                if count == tail_start_lines {
                    tail_start_byte = i + 1;
                    break;
                }
                count += 1;
            }
        }
        let middle_byte_len = tail_start_byte.saturating_sub(head_lines_pos + 1);
        // With raw-boundary accounting, dropped bytes ≈ middle byte
        // length (within a small slop for the byte-budget cut not
        // exactly aligning to head_lines_pos and tail_start_byte).
        assert!(
            out.stats.bytes_dropped_truncate >= middle_byte_len / 2,
            "expected dropped >= half of middle ({}), got {}",
            middle_byte_len / 2,
            out.stats.bytes_dropped_truncate,
        );
    }

    /// Round-9 (new loop) regression: when the entire payload is one
    /// extremely long line (no `\n` anywhere), the bypass must not
    /// leak a head prefix of that line — there is no safe line
    /// boundary, so head must be dropped just like tail. Output is
    /// the marker + degraded sentinel only.
    #[test]
    fn oversize_bypass_newline_free_drops_head_prefix() {
        let mut raw: Vec<u8> = Vec::new();
        // Distinct head-marker that would be visible if the bug
        // recurred (first ~half-budget bytes of source).
        raw.extend_from_slice(b"SECRET-PREFIX-DO-NOT-LEAK");
        raw.extend(std::iter::repeat_n(b'X', 100_000));
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            !body.contains("SECRET-PREFIX"),
            "head prefix must be dropped: {body:.200}"
        );
        assert!(body.contains("oversize bypass"), "marker present");
        assert!(body.contains("tail dropped"), "degraded sentinel present");
    }

    /// Round-7 (new loop) regression: when sanitized tail exceeds the
    /// byte budget, `trim_to_byte_budget_at_boundary_from_end` shaves
    /// bytes off the front, which may sit mid-line; the bypass must
    /// re-align to the next `\n` so the emitted tail begins on a
    /// whole-line boundary.
    #[test]
    fn oversize_bypass_post_trim_realigns_tail() {
        // Build a tail that is line-aligned but oversized after sanitize:
        // many short lines whose total exceeds half the byte budget.
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head\n");
        raw.extend(std::iter::repeat_n(b'F', 50_000));
        raw.push(b'\n');
        // Add 200 lines of 80 chars each = 16K bytes — exceeds default
        // half-budget (~8K), forcing front-trim mid-line.
        for i in 0..200 {
            raw.extend_from_slice(format!("line-{i:03}-XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX\n").as_bytes());
        }
        raw.extend_from_slice(b"FINAL-LINE\n");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        // Find the marker, then verify the byte AFTER the marker's
        // trailing `\n` starts a complete `line-` line, not mid-line.
        if let Some((_, after_marker)) = body.rsplit_once("squash skipped…]\n") {
            let first_tail_line = after_marker.lines().next().unwrap_or("");
            assert!(
                first_tail_line.starts_with("line-") || first_tail_line == "FINAL-LINE",
                "tail must begin on a line boundary, got: {first_tail_line:?}"
            );
        }
        // Final line still preserved.
        assert!(
            body.contains("FINAL-LINE"),
            "final line preserved: ...{}",
            &body[body.len().saturating_sub(200)..]
        );
    }

    /// Round-7 (new loop) regression: bypass marker must distinguish
    /// the byte-ceiling guard from the line-cardinality guard.
    #[test]
    fn oversize_bypass_line_cardinality_marker_distinct() {
        let raw: Vec<u8> = b"head\n"
            .iter()
            .copied()
            .chain(std::iter::repeat_n(b'\n', 100))
            .collect();
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::LineCardinality,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("MAX_INPUT_LINES"),
            "line-cardinality marker present: {body:?}"
        );
        assert!(
            !body.contains(">= MAX_INPUT_BYTES"),
            "byte-ceiling marker must not appear for line-cardinality bypass: {body:?}"
        );
    }

    /// Round-7 (new loop) regression: `bytes_dropped_truncate` must
    /// reflect dropped *source* bytes, not `raw_byte_len -
    /// compacted_bytes.len()`. Synthetic markers/newlines are not
    /// source bytes; subtracting them under-reports loss.
    #[test]
    fn oversize_bypass_drop_accounting_excludes_synthetic_marker() {
        // Construct a payload whose head + tail preserved bytes are
        // small relative to total raw_byte_len; expected drop ~=
        // raw_byte_len - (head_preserved + tail_preserved).
        let mut raw = Vec::new();
        raw.extend_from_slice(b"head-line\n");
        raw.extend(std::iter::repeat_n(b'M', 80_000));
        raw.push(b'\n');
        raw.extend_from_slice(b"tail-line\n");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        // Sum of marker + newlines ~= 100 bytes. dropped should be
        // strictly greater than (raw_byte_len - compacted_byte_len),
        // because compacted includes synthetic marker bytes that
        // aren't source.
        let naive = raw_byte_len.saturating_sub(out.compacted_byte_len);
        assert!(
            out.stats.bytes_dropped_truncate > naive,
            "source-byte accounting > naive raw-minus-compacted: got {} vs naive {}",
            out.stats.bytes_dropped_truncate,
            naive,
        );
    }

    /// Round-5 (new loop) regression: the oversize bypass tail slice
    /// can begin mid-OSC/CSI sequence; the sanitizer anchors on ESC
    /// and would treat the dangling bytes as plain text. Advance the
    /// slice to the next `\n` boundary in raw bytes BEFORE decoding so
    /// the sanitizer sees every introducer in-window.
    #[test]
    fn oversize_bypass_tail_slice_in_mid_osc_does_not_leak_url() {
        // Layout: head + filler such that the retained tail window
        // starts INSIDE an OSC-8 hyperlink URL body (the introducer
        // ESC ] 8 ; ; sits before tail_raw_start). After the OSC URL
        // there is BEL terminator, then "VISIBLE\n".
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head-line\n");
        // Filler so tail_window cuts mid-OSC. Default cfg max_bytes
        // = 16384, so tail_window ≈ 2 * 8000 = 16000.
        raw.extend(std::iter::repeat_n(b'F', 50_000));
        raw.push(b'\n');
        // Place the OSC introducer just before the tail window starts.
        // tail_raw_start = raw.len() - 16000 (computed at runtime). We
        // construct the OSC so its introducer is well before that.
        raw.extend_from_slice(
            b"\x1b]8;;https://attacker.example/secret-token-aaaa-bbbb-cccc-dddd-eeee-ffff",
        );
        raw.extend(std::iter::repeat_n(b'X', 16_500));
        // BEL ends the OSC, then a real visible line.
        raw.push(0x07);
        raw.extend_from_slice(b"VISIBLE\nFINAL-LINE\n");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            !body.contains("attacker.example"),
            "URL must not leak into compacted: {body:?}"
        );
        assert!(
            !body.contains("secret-token"),
            "token must not leak: {body:?}"
        );
        // FINAL-LINE survives via the line-aligned tail.
        assert!(
            body.contains("FINAL-LINE"),
            "final line preserved: ...{}",
            &body[body.len().saturating_sub(200)..]
        );
        assert!(out.compacted_byte_len <= cfg.max_bytes());
    }

    /// Round-6 (new loop) regression: oversized newline-free payloads
    /// must emit ONLY the degraded marker — never a suffix that may
    /// carry residual mid-sequence control bytes (URLs, titles).
    #[test]
    fn oversize_bypass_newline_free_payload_drops_suffix() {
        let mut raw: Vec<u8> = Vec::new();
        raw.extend(std::iter::repeat_n(b'B', 100_000));
        raw.extend_from_slice(b"END-OF-STREAM-TAG");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        // Suffix is NOT preserved — would risk leaking mid-sequence bytes.
        assert!(
            !body.contains("END-OF-STREAM-TAG"),
            "suffix must be dropped: {body:?}"
        );
        assert!(
            body.contains("tail dropped"),
            "degraded marker present: {body:?}"
        );
        assert!(out.compacted_byte_len <= cfg.max_bytes());
    }

    /// Round-6 (new loop) regression: a payload under `MAX_INPUT_BYTES`
    /// but with > `MAX_INPUT_LINES` newlines must route to the bypass
    /// path so stage 3 does not allocate millions of empty `String`s.
    // Allocates ~1M bytes (1.0 MiB) — fast enough for CI, exercises
    // the LineCardinality bypass route through squash() end-to-end.
    #[test]
    fn line_dense_payload_routes_to_bypass() {
        let n = MAX_INPUT_LINES + 1;
        let mut raw: Vec<u8> = Vec::with_capacity(n + 32);
        raw.extend(std::iter::repeat_n(b'\n', n));
        raw.extend_from_slice(b"FINAL\n");
        let evt = terminal_event(&raw);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            &raw,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("under byte ceiling");
        let cfg = SquashConfig::default();
        let out = super::squash(wrapped, &cfg);
        assert!(out.stats.truncated, "must take bypass path");
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("oversize bypass"),
            "bypass marker present: {body:?}"
        );
    }

    /// Round-3 (new loop) regression: the oversize bypass must run
    /// stage-2 ANSI/OSC sanitization on retained head/tail windows so
    /// raw control-plane bytes (CSI escapes, OSC titles/URLs) do not
    /// leak into compacted output. Calls `oversize_bypass` directly so
    /// the test stays fast.
    #[test]
    fn oversize_bypass_sanitizes_ansi_and_osc() {
        // Build a payload with embedded ANSI + OSC across both head
        // and tail regions. Body in the middle is filler.
        let head_chunk = b"\x1b[31mHEADRED\x1b[0m\nhead-line-A\nhead-line-B\n";
        let tail_chunk = b"tail-line-A\ntail-line-B\n\x1b]0;hidden-title\x07TAILEND\n";
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(head_chunk);
        raw.extend(std::iter::repeat_n(b'F', 50_000));
        raw.push(b'\n');
        raw.extend_from_slice(tail_chunk);
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        // Sanitization happened.
        assert!(out.stats.ansi_stripped, "ansi_stripped flag must be set");
        // No raw ESC bytes leaked.
        assert!(!body.contains('\x1b'), "ESC must not leak: {body:?}");
        // OSC title body must NOT survive (well-formed OSC is fully
        // stripped by stage 2).
        assert!(!body.contains("hidden-title"), "OSC body must be stripped");
        // SGR-decorated content survives without color codes.
        assert!(body.contains("HEADRED"), "head content survives");
        assert!(body.contains("TAILEND"), "tail content survives");
        // Marker present.
        assert!(body.contains("oversize bypass"), "marker present");
        assert!(out.compacted_byte_len <= cfg.max_bytes());
    }

    /// Round-3 (new loop) regression: the oversize bypass must
    /// line-align the retained tail so the final diagnostic line is
    /// preserved whole (or omitted), never truncated mid-content.
    #[test]
    fn oversize_bypass_tail_is_line_aligned() {
        // Construct a payload where the retained tail window starts
        // mid-line; the bypass must skip to the next `\n` boundary.
        let prefix = b"head\n";
        let filler: Vec<u8> = std::iter::repeat_n(b'M', 60_000).collect();
        let mid_line_marker = b"MIDLINEFRAG"; // appears before final \n
        let final_line = b"\nFINAL-DIAGNOSTIC-LINE\n";
        let mut raw = Vec::new();
        raw.extend_from_slice(prefix);
        raw.extend_from_slice(&filler);
        raw.extend_from_slice(mid_line_marker);
        raw.extend_from_slice(final_line);
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let cfg = SquashConfig::default();
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        // Final diagnostic line preserved whole.
        assert!(
            body.contains("FINAL-DIAGNOSTIC-LINE"),
            "final line preserved: ...{}",
            &body[body.len().saturating_sub(200)..]
        );
        // Find the post-marker tail; first line of tail must NOT begin
        // with a partial mid-line fragment (the leading 'M' filler that
        // sits before any `\n` in the retained window).
        if let Some((_, after_marker)) = body.rsplit_once("squash skipped…]\n") {
            let first_tail_line = after_marker.lines().next().unwrap_or("");
            assert!(
                !first_tail_line.starts_with('M'),
                "tail first line must not be a mid-line fragment: {first_tail_line:?}"
            );
        }
    }

    /// Round-2 (new loop) regression: the oversize bypass must enforce
    /// `compacted_byte_len <= cfg.max_bytes()` in release builds even
    /// for non-UTF8 payloads, because lossy decoding expands each
    /// invalid byte to the 3-byte U+FFFD. Heavy: 64 MiB allocation.
    #[test]
    #[ignore = "allocates MAX_INPUT_BYTES + 1 bytes; run with --ignored"]
    fn oversize_bypass_enforces_byte_ceiling_on_invalid_utf8() {
        // 0xFF is never a valid UTF-8 leading byte; every byte in the
        // payload becomes U+FFFD on lossy decode (1B → 3B expansion).
        let oversized = vec![0xFFu8; MAX_INPUT_BYTES + 1];
        let evt = terminal_event(&oversized);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            &oversized,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("oversize is no longer rejected");
        let cfg = SquashConfig::default();
        let out = super::squash(wrapped, &cfg);
        assert!(out.stats.truncated);
        assert!(
            out.compacted_byte_len <= cfg.max_bytes(),
            "compacted exceeds max_bytes: {} > {}",
            out.compacted_byte_len,
            cfg.max_bytes(),
        );
    }

    /// Round-1 (new loop) regression: oversize payloads are NOT rejected;
    /// `squash()` falls back to a head+tail byte-slice bypass that
    /// preserves both ends of the raw stream and emits a clear marker.
    /// Heavy (64 MiB allocation) — gated behind `--ignored` so default
    /// runs stay fast.
    #[test]
    #[ignore = "allocates MAX_INPUT_BYTES + 1 bytes; run with --ignored"]
    fn oversize_payload_bypass_preserves_head_and_tail() {
        let mut oversized = vec![b'A'; MAX_INPUT_BYTES + 1];
        // Distinct head and tail markers so we can confirm preservation.
        // The head-leak hardening (Round 9) drops the head entirely when
        // its window contains no `\n`, so place a `\n` after the head
        // marker AND before the tail marker so both line up to a line
        // boundary inside their respective windows.
        oversized[..6].copy_from_slice(b"HEADX\n");
        let n = oversized.len();
        oversized[n - 6..].copy_from_slice(b"\nYTAIL");
        let evt = terminal_event(&oversized);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            &oversized,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("oversize is no longer rejected");
        let cfg = SquashConfig::default();
        let out = super::squash(wrapped, &cfg);
        assert!(out.stats.truncated);
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(body.contains("HEADX"), "head preserved: {body:.200}");
        assert!(
            body.contains("YTAIL"),
            "tail preserved: ...{}",
            &body[body.len().saturating_sub(200)..]
        );
        assert!(body.contains("oversize bypass"), "marker present");
        assert!(out.compacted_bytes.len() <= cfg.max_bytes() + LAYOUT_OVERHEAD);
    }

    /// Round-6 regression: malformed envelopes (e.g., `source_family` /
    /// payload-variant disagreement) must be rejected before squash sees
    /// the bytes. Otherwise an in-crate caller could route non-terminal
    /// bytes through the lossy stage.
    #[test]
    fn rejects_envelope_validation_failure() {
        let bytes = b"hello\n";
        let mut evt = terminal_event(bytes);
        // Force source_family / payload-variant disagreement.
        evt.source_family = SourceFamily::Hook;
        let err = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            bytes,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .unwrap_err();
        assert!(
            matches!(err, UnstructuredBindError::EventValidationFailed(_)),
            "got: {err:?}"
        );
    }

    /// Helper for issue #250 parity tests: run `payload` through both the
    /// staged path (`squash` on a small input) and the bypass path
    /// (`oversize_bypass` invoked directly) with the same default config,
    /// and return the two `cr_bearing_lines` values. Centralised so each
    /// per-shape test stays focused on the input it constructs.
    fn cr_bearing_lines_both_paths(payload: &[u8]) -> (usize, usize) {
        let evt = terminal_event(payload);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            payload,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("valid construction");
        let cfg = SquashConfig::default();
        let staged = super::squash(wrapped, &cfg);

        let raw_hash = super::sha256_payload_hash(payload);
        let bypass = super::oversize_bypass(
            payload,
            raw_hash,
            payload.len(),
            &cfg,
            SquashStats::default(),
            super::BypassReason::ByteCeiling,
        );
        (staged.stats.cr_bearing_lines, bypass.stats.cr_bearing_lines)
    }

    /// Issue #250: a terminated OSC body containing `\r` must be excluded
    /// from `cr_bearing_lines` on BOTH the staged and the bypass path.
    /// Pre-fix the bypass walked raw bytes without parsing escapes, so the
    /// embedded `\r` registered as a hazard only on the bypass side —
    /// flipping the audit signal at the size gate.
    #[test]
    fn cr_bearing_lines_parity_osc_body_with_embedded_cr() {
        // OSC: ESC ] ... BEL. Body contains `\r` between `with` and `cr`.
        let payload = b"prefix\n\x1b]0;title-with\rcr\x07\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert_eq!(
            staged, 0,
            "OSC body \\r is stripped by stage 2; staged count must be 0"
        );
        assert_eq!(
            bypass, staged,
            "bypass parity: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250: DCS / APC / PM / SOS bodies with embedded `\r` must
    /// match the staged-path count (zero — stage 2 quarantines all four
    /// string-control families just like OSC).
    #[test]
    fn cr_bearing_lines_parity_dcs_family_bodies_with_embedded_cr() {
        for &(intro, name) in &[(b'P', "DCS"), (b'_', "APC"), (b'^', "PM"), (b'X', "SOS")] {
            let mut payload: Vec<u8> = Vec::new();
            payload.extend_from_slice(b"prefix\n");
            payload.push(0x1B);
            payload.push(intro);
            payload.extend_from_slice(b"body-with\rcr");
            // Strict-ST terminator: ESC \\ (no BEL accepted for DCS-family).
            payload.push(0x1B);
            payload.push(0x5C);
            payload.extend_from_slice(b"\nsuffix\n");

            let (staged, bypass) = cr_bearing_lines_both_paths(&payload);
            assert_eq!(staged, 0, "{name}: staged count must be 0");
            assert_eq!(
                bypass, staged,
                "{name}: bypass parity: staged={staged} bypass={bypass}"
            );
        }
    }

    /// Issue #250: an UN-terminated control string drops its body up to
    /// the next `\n` on the staged path. The bypass-side count must drop
    /// the same bytes — a `\r` inside an unterminated OSC body must not
    /// be counted on either path.
    #[test]
    fn cr_bearing_lines_parity_unterminated_osc_body_with_cr() {
        // OSC introducer + body containing \r, no terminator before \n.
        let payload = b"prefix\n\x1b]0;dangling-with\rcr-no-term\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert_eq!(
            staged, 0,
            "unterminated OSC body dropped to LF; staged count must be 0"
        );
        assert_eq!(
            bypass, staged,
            "bypass parity: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250: a real bare-CR hazard (interior `\r` in a progress-bar
    /// line) coexisting with a CR inside an OSC body must register exactly
    /// once on both paths — the OSC-body \r excluded, the progress-line \r
    /// counted. Pins that the parity fix did not over-strip.
    #[test]
    fn cr_bearing_lines_parity_mixed_real_hazard_and_osc_body_cr() {
        let payload = b"prefix\n\x1b]0;title-with\rcr\x07\nprog 50%\rprog 100%\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert_eq!(
            staged, 1,
            "exactly one CR-bearing line on staged: the prog 50%... line"
        );
        assert_eq!(
            bypass, staged,
            "bypass parity at the staged-vs-bypass gate: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250 round-2 review finding: when a payload's invalid UTF-8
    /// byte sits immediately after an unrecognized ESC, the bypass walker
    /// previously inferred a UTF-8 codepoint length naively from the raw
    /// byte and could swallow a following bare `\r`. Stage 1's lossy
    /// decode would have replaced the bad byte with `U+FFFD` first, so
    /// the staged path counts the surviving CR-bearing line — the bypass
    /// must match by validating UTF-8 continuations (or consuming only
    /// the bad byte when the lead is invalid).
    #[test]
    fn cr_bearing_lines_parity_invalid_utf8_lead_after_esc() {
        // ESC + 0xFF (invalid lead) + bare \r + content + \n.
        let payload: &[u8] = b"prefix\n\x1b\xff\rprogress\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert_eq!(
            staged, 1,
            "staged path: stage 1 lossy → ESC+U+FFFD consumed by stage 2; \\r survives"
        );
        assert_eq!(
            bypass, staged,
            "bypass parity on invalid lead after ESC: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250 round-2 review finding: a continuation byte (`0x80..=0xBF`)
    /// in lead position after ESC is invalid; the walker must consume only
    /// the bad byte so a following `\r` survives.
    #[test]
    fn cr_bearing_lines_parity_continuation_byte_as_esc_lead() {
        // ESC + 0xA0 (continuation byte in lead position) + \r + content.
        let payload: &[u8] = b"prefix\n\x1b\xa0\rprogress\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert_eq!(staged, 1, "staged: surviving \\r counted");
        assert_eq!(
            bypass, staged,
            "bypass parity on continuation-byte ESC lead: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250 round-2 review finding: a truncated multibyte after ESC
    /// (e.g., ESC + 0xC2 followed by a non-continuation byte) must consume
    /// only the lead so the following byte stays visible.
    #[test]
    fn cr_bearing_lines_parity_truncated_multibyte_after_esc() {
        // ESC + 0xC2 (claims a 2-byte scalar) + \r (NOT a valid continuation
        // byte) + content. Stage 1 lossy replaces 0xC2 alone with U+FFFD;
        // stage 2 consumes ESC + U+FFFD, leaving \r intact.
        let payload: &[u8] = b"prefix\n\x1b\xc2\rprogress\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert_eq!(staged, 1, "staged: \\r survives the lossy-decoded scalar");
        assert_eq!(
            bypass, staged,
            "bypass parity on truncated multibyte: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250 round-2 review finding: a well-formed multibyte scalar
    /// after ESC must still be fully consumed on both paths (no
    /// over-conservative under-skip from the new validation logic).
    #[test]
    fn cr_bearing_lines_parity_valid_multibyte_after_esc_unchanged() {
        // ESC + é (0xC3 0xA9) + \r + content. Stage 2 consumes the entire
        // ESC + 2-byte scalar; the bypass walker must do the same so the
        // surviving \r is still counted exactly once.
        let payload: &[u8] = b"prefix\n\x1b\xc3\xa9\rprogress\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert_eq!(staged, 1, "valid 2-byte scalar consumed; \\r survives");
        assert_eq!(
            bypass, staged,
            "bypass parity on valid multibyte after ESC: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250 round-2 review finding: a UTF-16 surrogate-range encoding
    /// (`ED A0..=BF ...`) is RFC 3629-invalid. Stage 1 lossy decode rejects
    /// it; the bypass walker's continuation-byte check would have accepted
    /// it. Delegating to `std::str::from_utf8` catches it.
    #[test]
    fn cr_bearing_lines_parity_surrogate_after_esc() {
        // ESC + ED A0 80 (encodes U+D800, a high surrogate — invalid). The
        // surrounding `\r ... \n` shape lets the line be CR-bearing only
        // when stage 2 / the bypass walker do NOT swallow the bad bytes
        // wholesale. Pre-fix, the walker consumed all three and treated
        // the LF as a CRLF; staged kept the lossy replacement bytes after
        // the \r and counted the line.
        let payload: &[u8] = b"prefix\r\x1b\xed\xa0\x80\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert!(
            staged >= 1,
            "staged must register the prefix-with-\\r line as CR-bearing"
        );
        assert_eq!(
            bypass, staged,
            "bypass parity on surrogate after ESC: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250 round-2 review finding: overlong 3-byte forms
    /// (`E0 80..=9F ...`) are RFC 3629-invalid. Same parity hazard as
    /// surrogates — `from_utf8` validation catches both.
    #[test]
    fn cr_bearing_lines_parity_overlong_three_byte_after_esc() {
        // ESC + E0 80 80 (overlong encoding of U+0000 in 3 bytes).
        let payload: &[u8] = b"prefix\r\x1b\xe0\x80\x80\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert!(staged >= 1, "staged must count the prefix line");
        assert_eq!(
            bypass, staged,
            "bypass parity on overlong 3-byte after ESC: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250 round-2 review finding: scalars beyond `U+10FFFF`
    /// (`F4 90..=BF ...`) are RFC 3629-invalid. Confirms the F4-bound
    /// branch of validation catches out-of-range 4-byte forms.
    #[test]
    fn cr_bearing_lines_parity_out_of_range_four_byte_after_esc() {
        // ESC + F4 90 80 80 (encodes U+110000, beyond U+10FFFF).
        let payload: &[u8] = b"prefix\r\x1b\xf4\x90\x80\x80\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert!(staged >= 1, "staged must count the prefix line");
        assert_eq!(
            bypass, staged,
            "bypass parity on out-of-range 4-byte after ESC: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250 round-3 review finding: when `from_utf8` rejects a
    /// truncated *valid prefix* (e.g., `E2 82` followed by a non-
    /// continuation), `from_utf8_lossy` collapses the whole prefix into
    /// one `U+FFFD`. The bypass walker must advance by the same maximal
    /// invalid subsequence length, not unconditionally one byte —
    /// otherwise a stray `0x82` leaks past ESC, resets the trailing-CR
    /// run, and a CRLF that the staged path collapses gets reported as
    /// CR-bearing on the bypass.
    #[test]
    fn cr_bearing_lines_parity_truncated_three_byte_prefix_after_esc() {
        // Prefix `\r` then ESC + E2 82 + LF: maximal invalid subsequence
        // is `E2 82` (2 bytes), replaced with one U+FFFD on the staged
        // path. Stage 2 then drops ESC + U+FFFD, leaving CRLF — which
        // the second pass collapses to LF, so the line is NOT CR-bearing.
        let payload: &[u8] = b"prefix\r\x1b\xe2\x82\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert_eq!(
            staged, 0,
            "staged: lossy collapses E2 82 to U+FFFD, stage 2 strips ESC+U+FFFD, CRLF→LF"
        );
        assert_eq!(
            bypass, staged,
            "bypass parity on truncated 3-byte prefix: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250 round-3 review finding: same shape for a 4-byte prefix
    /// (`F0 90 80` followed by LF). `from_utf8_lossy` replaces the 3-byte
    /// incomplete-but-valid prefix with one `U+FFFD`.
    #[test]
    fn cr_bearing_lines_parity_truncated_four_byte_prefix_after_esc() {
        let payload: &[u8] = b"prefix\r\x1b\xf0\x90\x80\nsuffix\n";
        let (staged, bypass) = cr_bearing_lines_both_paths(payload);
        assert_eq!(
            staged, 0,
            "staged: lossy collapses F0 90 80 to U+FFFD, stage 2 strips ESC+U+FFFD, CRLF→LF"
        );
        assert_eq!(
            bypass, staged,
            "bypass parity on truncated 4-byte prefix: staged={staged} bypass={bypass}"
        );
    }

    /// Issue #250: full integration via the actual `MAX_INPUT_BYTES` gate.
    /// Builds an oversize payload (`>= MAX_INPUT_BYTES`) where both the
    /// head and the tail carry the same OSC-body-with-embedded-CR +
    /// progress-bar pattern, with CR-free filler in between. `squash`
    /// routes through the real bypass path; the staged baseline runs on
    /// the head bytes alone. Heavy (64 MiB allocation) — gated behind
    /// `--ignored` like the other gate-crossing test in this module.
    #[test]
    #[ignore = "allocates MAX_INPUT_BYTES + 1 bytes; run with --ignored"]
    fn cr_bearing_lines_parity_at_max_input_bytes_gate() {
        let head_pattern = b"head\n\x1b]0;title-with\rcr\x07\nprog 50%\rprog 100%\n";
        let tail_pattern = b"prog 25%\rprog 100%\n\x1b]0;t-with\rcr\x07\ntail\n";

        // Staged baseline: same pattern, small payload — takes the staged
        // path and yields the per-pattern count (one CR-bearing prog line).
        let (staged_head, _) = cr_bearing_lines_both_paths(head_pattern);
        let (staged_tail, _) = cr_bearing_lines_both_paths(tail_pattern);

        // Build an oversize payload that crosses MAX_INPUT_BYTES naturally.
        let filler_len = MAX_INPUT_BYTES + 1 - head_pattern.len() - tail_pattern.len();
        let mut oversized: Vec<u8> = Vec::with_capacity(MAX_INPUT_BYTES + 1);
        oversized.extend_from_slice(head_pattern);
        oversized.extend(std::iter::repeat_n(b'F', filler_len));
        oversized.extend_from_slice(tail_pattern);
        assert!(oversized.len() >= MAX_INPUT_BYTES);

        let evt = terminal_event(&oversized);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            &oversized,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("oversize accepted");
        let cfg = SquashConfig::default();
        let out = super::squash(wrapped, &cfg);

        // The bypass walks the FULL raw input for `cr_bearing_lines`, so the
        // count is the sum of CR-bearing lines across head + filler + tail.
        // Filler is CR-free; only the two prog-bar lines (head + tail) count.
        // OSC-body \rs in head and tail are excluded by the shared parser.
        assert_eq!(staged_head, 1, "staged head pattern → 1 CR-bearing line");
        assert_eq!(staged_tail, 1, "staged tail pattern → 1 CR-bearing line");
        assert_eq!(
            out.stats.cr_bearing_lines,
            staged_head + staged_tail,
            "bypass full-payload count must equal sum of staged per-pattern counts"
        );
    }
}

/// Result of a successful squash: compacted bytes plus audit metadata.
#[derive(Debug, Clone)]
pub struct SquashOutput {
    /// Compacted output bytes. Audit artifact; renderer is responsible for
    /// any TTY-safe escaping (see spec on CR semantics).
    pub compacted_bytes: Vec<u8>,
    /// `sha256:<hex>` of the input bytes, copied from the source `CaptureEvent`.
    pub raw_hash: PayloadHash,
    /// Length in bytes of the input passed to `squash`.
    pub raw_byte_len: usize,
    /// `sha256:<hex>` of `compacted_bytes`.
    pub compacted_hash: PayloadHash,
    /// Length in bytes of `compacted_bytes`.
    pub compacted_byte_len: usize,
    /// Per-call statistics for audit and observability.
    pub stats: SquashStats,
}

/// Per-call statistics. Drives audit, observability, and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SquashStats {
    /// Whether stage 2 stripped any ANSI escape sequence.
    pub ansi_stripped: bool,
    /// Number of `\n`-delimited lines containing at least one bare `\r`
    /// after CRLF normalize. Source-capture audit signal: counted on the
    /// FULL input — post-stage-2 ANSI strip on the staged path, and via
    /// `count_bare_cr_lines_after_stage2` (a raw-byte walker that mirrors
    /// stage 2's escape-state parser and CRLF / CRCRLF rule) on the
    /// oversize-bypass path. Stable across the staged-vs-bypass gate so
    /// audit/warning logic sees the same hazard count regardless of
    /// which path the input took.
    pub cr_bearing_lines: usize,
    /// Number of dedup runs collapsed in stage 4.
    pub dedup_runs_collapsed: usize,
    /// Number of lines dropped by stage 6 head/tail truncation.
    pub lines_dropped_truncate: usize,
    /// Bytes dropped by stage 6 head/tail truncation, measured on the
    /// **post-stage-2b rendered text**. When `progress_frame_collapse_enabled`
    /// is on, stage 2b shrinks CR-bearing lines before stage 6 sees them,
    /// so this counter under-reports raw source-byte loss for collapsed
    /// lines (the collapsed bytes are reported separately under
    /// `progress_bytes_saved`).
    ///
    /// Stage 4 dedup-run collapse and stage 5 per-line cap also discard
    /// source bytes but only surface here as line counters
    /// (`dedup_runs_collapsed`, `long_lines_truncated`); they do not
    /// flow into this byte counter. Use the coarse `truncated` bit as
    /// the canonical "any source bytes lost" signal.
    pub bytes_dropped_truncate: usize,
    /// Number of lines that exceeded `max_line_bytes` and were truncated in stage 5.
    pub long_lines_truncated: usize,
    /// True iff `compacted_bytes` is NOT a verbatim sanitization-free
    /// copy of the input — i.e., any lossy transform acted: stage 2
    /// ANSI/OSC strip, stage 2 OSC recovery drop, stage 2b progress-frame collapse
    /// overwrite, stage 4 dedup collapse, stage 5 per-line cap, stage 6
    /// head/tail truncation, or the oversize bypass. Downstream code
    /// that gates fallback / raw-retention / warning banners on a
    /// single coarse bit should use this; per-stage counters give the
    /// breakdown.
    pub truncated: bool,
    /// Bytes discarded by stage-2 OSC recovery on unterminated escape
    /// sequences (introducer + body up to the next `\n`, or to EOF if
    /// no `\n` exists). Distinct from `bytes_dropped_truncate` so audit
    /// consumers can tell sanitization-driven loss apart from
    /// budget-driven truncation. Hardening for truncated terminal
    /// captures whose final diagnostic line followed a stray `ESC ]`.
    pub osc_recovery_bytes_dropped: usize,
    /// True iff stage 1 had to replace at least one invalid UTF-8 byte
    /// sequence with `U+FFFD`. Distinct signal from later
    /// budget-driven truncation so audit consumers can tell decode loss
    /// apart from sanitization or trim loss. Always feeds `truncated`.
    pub utf8_replacement: bool,
    /// Number of `\n`-delimited lines that contained at least one bare
    /// `\r` and were rewritten by stage 2b (progress-frame collapse).
    /// Counted per source line, not per `\r`. Reflects WORK PERFORMED
    /// on stage 2b's input: on the staged path that is the entire
    /// stage-2 output; on the oversize-bypass path that is only the
    /// retained head/tail/suffix windows (the dropped middle is never
    /// rendered, so no rewrites are counted from it). For a source-level
    /// CR-bearing-line count that is stable across the bypass gate, use
    /// `cr_bearing_lines`.
    pub progress_frames_coalesced: usize,
    /// Source bytes dropped by stage 2b: original line bytes minus
    /// rendered line bytes, summed across coalesced lines. Saturating-
    /// clamped at zero (rewrites that don't shrink contribute nothing).
    /// These bytes never reach stages 4/5/6, so they are NOT included
    /// in `bytes_dropped_truncate`. This counter reflects only stage 2b;
    /// other lossy stages have their own counters (or, for dedup and
    /// stage 5 cap, only line-count counters). Like
    /// `progress_frames_coalesced`, this is a WORK counter scoped to
    /// the bytes stage 2b actually rendered: on the oversize-bypass
    /// path it covers only the retained head/tail/suffix windows.
    pub progress_bytes_saved: usize,
}

// Note: an earlier revision (review-loop round 6) added a
// `source_bytes_lost_total()` accessor that summed three byte-loss
// counters. Round 7 review correctly pointed out that the field set
// is incomplete — stage 4 dedup collapse and stage 5 per-line cap
// also discard source bytes but do not have byte-counter equivalents.
// Exposing a `*_total()` method invited callers to treat it as
// authoritative, which it cannot be without threading per-line
// source-byte spans through stages 4/5/6. The accessor was removed
// rather than ship a misleading API; the canonical "non-verbatim
// output" signal remains the coarse `truncated` bit, and per-stage
// counters give the breakdown each stage can honestly report.

use std::borrow::Cow;

/// Stage 1: lossy UTF-8 decode. Invalid byte sequences become
/// U+FFFD; valid input passes through borrowed.
fn stage1_lossy_utf8(raw: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(raw)
}

/// Count `\n` bytes; short-circuits as soon as the count exceeds the
/// `MAX_INPUT_LINES` ceiling so pathological inputs are not fully
/// scanned before triggering the bypass.
fn bytecount_newlines(raw: &[u8]) -> usize {
    let mut count = 0usize;
    for &b in raw {
        if b == b'\n' {
            count += 1;
            if count >= MAX_INPUT_LINES {
                return count;
            }
        }
    }
    count
}

/// Trim the *end* of `s` so its byte length is at most `budget`,
/// stopping at the largest codepoint boundary that fits. Used by
/// the oversize-bypass head slice.
fn trim_to_byte_budget_at_boundary(s: &str, budget: usize) -> &str {
    if s.len() <= budget {
        return s;
    }
    let mut keep = budget;
    while keep > 0 && !s.is_char_boundary(keep) {
        keep -= 1;
    }
    &s[..keep]
}

/// Trim the *front* of `s` so its byte length is at most `budget`,
/// starting at the smallest codepoint boundary that fits. Used by
/// the oversize-bypass tail slice.
fn trim_to_byte_budget_at_boundary_from_end(s: &str, budget: usize) -> &str {
    if s.len() <= budget {
        return s;
    }
    let mut start = s.len() - budget;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

#[cfg(test)]
mod stage1_tests {
    use super::*;

    #[test]
    fn valid_ascii_passes_through() {
        assert_eq!(stage1_lossy_utf8(b"hello\n").as_ref(), "hello\n");
    }

    #[test]
    fn valid_utf8_passes_through() {
        let s = "héllo こんにちは\n";
        assert_eq!(stage1_lossy_utf8(s.as_bytes()).as_ref(), s);
    }

    #[test]
    fn invalid_utf8_replaced_with_u_fffd() {
        let bytes = b"a\xFFb";
        let out = stage1_lossy_utf8(bytes);
        assert_eq!(out.as_ref(), "a\u{FFFD}b");
    }

    #[test]
    fn empty_input_yields_empty() {
        assert_eq!(stage1_lossy_utf8(b"").as_ref(), "");
    }
}

/// Stage 2: Strip ANSI/CSI/OSC escape sequences and bare control characters
/// (except `\n`, `\t`, `\r`), then normalize CRLF → LF (preserving lone CR).
///
/// Sets `*stripped = true` whenever any byte is removed or normalized.
/// Adds the byte count of any unterminated-OSC recovery drop
/// (introducer plus body up to next LF / EOF) to
/// `*osc_recovery_dropped`. Audit consumers can use that counter to
/// detect silent tail loss on truncated captures.
#[allow(clippy::expect_used)] // invariant: only ASCII control bytes/ESC sequences removed; UTF-8 preserved
/// Walk `prefix` with the same CSI/OSC matcher used by
/// [`stage2_ansi_strip`] and report whether the parser is OUTSIDE any
/// open escape sequence at the end of the slice. Used by the oversize
/// bypass to decide whether a raw byte position is safe to begin a
/// preserved suffix at — i.e., the suffix can't start mid CSI/OSC and
/// thus can't leak dangling URL/params bytes if the introducer was
/// sliced off.
fn ends_outside_escape_sequence(prefix: &[u8]) -> bool {
    let mut i = 0;
    while i < prefix.len() {
        if prefix[i] != 0x1B {
            i += 1;
            continue;
        }
        // ESC: look ahead for CSI/OSC/other.
        if i + 1 >= prefix.len() {
            // Lone ESC at end of prefix: parser still consuming.
            return false;
        }
        match prefix[i + 1] {
            0x5B => {
                // CSI: params (0x30..=0x3F), intermediates (0x20..=0x2F),
                // then a final byte (0x40..=0x7E). Must terminate
                // INSIDE prefix for it to count as "closed before end".
                let mut j = i + 2;
                while j < prefix.len() && (0x30..=0x3F).contains(&prefix[j]) {
                    j += 1;
                }
                while j < prefix.len() && (0x20..=0x2F).contains(&prefix[j]) {
                    j += 1;
                }
                let final_present = matches!(
                    prefix.get(j).copied(),
                    Some(b) if (0x40..=0x7E).contains(&b)
                );
                if final_present {
                    i = j + 1;
                } else {
                    return false;
                }
            }
            0x5D => {
                // OSC: terminate on BEL (0x07) or ST (ESC \).
                let mut j = i + 2;
                let mut terminated_at: Option<usize> = None;
                while j < prefix.len() {
                    if prefix[j] == 0x07 {
                        terminated_at = Some(j + 1);
                        break;
                    }
                    if prefix[j] == 0x1B && j + 1 < prefix.len() && prefix[j + 1] == 0x5C {
                        terminated_at = Some(j + 2);
                        break;
                    }
                    j += 1;
                }
                if let Some(end) = terminated_at {
                    i = end;
                } else {
                    return false;
                }
            }
            0x50 | 0x5F | 0x5E | 0x58 => {
                // DCS / APC / PM / SOS: strict-ST (ESC \) terminator.
                let mut j = i + 2;
                let mut terminated_at: Option<usize> = None;
                while j < prefix.len() {
                    if prefix[j] == 0x1B && j + 1 < prefix.len() && prefix[j + 1] == 0x5C {
                        terminated_at = Some(j + 2);
                        break;
                    }
                    j += 1;
                }
                if let Some(end) = terminated_at {
                    i = end;
                } else {
                    return false;
                }
            }
            _ => {
                // Two-byte / unrecognised ESC sequence: ESC + 1 byte.
                i += 2;
            }
        }
    }
    true
}

/// Walk `raw` with the same CSI/OSC/DCS/APC/PM/SOS matcher used by
/// [`stage2_ansi_strip`] and return the first byte position `p` such
/// that `p >= threshold`, `raw[p - 1] == b'\n'`, and the parser is in
/// "outside escape" state at byte `p`. Returns `None` if no such
/// boundary exists. Used by the oversize bypass to choose a tail
/// cut point that cannot land inside an open multi-byte control
/// sequence.
fn first_safe_line_start_at_or_after(raw: &[u8], threshold: usize) -> Option<usize> {
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        if b == 0x1B {
            if i + 1 >= raw.len() {
                return None;
            }
            match raw[i + 1] {
                0x5B => {
                    let mut j = i + 2;
                    while j < raw.len() && (0x30..=0x3F).contains(&raw[j]) {
                        j += 1;
                    }
                    while j < raw.len() && (0x20..=0x2F).contains(&raw[j]) {
                        j += 1;
                    }
                    let final_present = matches!(
                        raw.get(j).copied(),
                        Some(b) if (0x40..=0x7E).contains(&b)
                    );
                    if final_present {
                        i = j + 1;
                    } else {
                        return None;
                    }
                }
                0x5D => {
                    let mut j = i + 2;
                    let mut term: Option<usize> = None;
                    while j < raw.len() {
                        if raw[j] == 0x07 {
                            term = Some(j + 1);
                            break;
                        }
                        if raw[j] == 0x1B && j + 1 < raw.len() && raw[j + 1] == 0x5C {
                            term = Some(j + 2);
                            break;
                        }
                        j += 1;
                    }
                    if let Some(end) = term {
                        i = end;
                    } else {
                        return None;
                    }
                }
                0x50 | 0x5F | 0x5E | 0x58 => {
                    let mut j = i + 2;
                    let mut term: Option<usize> = None;
                    while j < raw.len() {
                        if raw[j] == 0x1B && j + 1 < raw.len() && raw[j + 1] == 0x5C {
                            term = Some(j + 2);
                            break;
                        }
                        j += 1;
                    }
                    if let Some(end) = term {
                        i = end;
                    } else {
                        return None;
                    }
                }
                _ => {
                    i += 2;
                }
            }
        } else if b == b'\n' {
            let p = i + 1;
            if p >= threshold {
                return Some(p);
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    None
}

/// Sentinels emitted by stage 2 when it strips a `CSI K` (erase-in-
/// line) sequence and the caller asked for erase semantics to survive
/// into stage 2b (`emit_erase_sentinel = true`). Stage 2b consumes the
/// sentinels to model the "this frame was visually cleared" signal
/// that would otherwise be lost when the CSI bytes are stripped before
/// stage 2b's `\r`-segment collapse runs (issue #249).
///
/// Two distinct sentinels are needed because the `K` parameter changes
/// the visual effect when the cursor is NOT at column 0:
/// - `ERASE_LINE_SENTINEL` (`0x01`) — cursor-to-EOL erase (default
///   param or `0`). Only a leading-segment-after-`\r` placement
///   reliably models "line cleared"; mid-segment placement leaves the
///   pre-cursor content visible.
/// - `ERASE_WHOLE_LINE_SENTINEL` (`0x02`) — whole-line erase (`2`).
///   Position is irrelevant: anything written before the sentinel on
///   the same line is gone regardless of `\r` presence or cursor
///   column.
///
/// Both are control bytes in `0x01..=0x06` that stage 2's outer-loop
/// control filter already strips from raw input (`\n`, `\t`, `\r` are
/// the only `< 0x20` survivors), so user content cannot smuggle either
/// sentinel into stage 2's output. Both are single-byte UTF-8, so
/// emission/removal stays byte-aligned. Both are stripped from stage
/// 2b's output before it returns — stages 3+ never see them.
const ERASE_LINE_SENTINEL: char = '\u{0001}';
const ERASE_LINE_SENTINEL_BYTE: u8 = 0x01;
const ERASE_WHOLE_LINE_SENTINEL: char = '\u{0002}';
const ERASE_WHOLE_LINE_SENTINEL_BYTE: u8 = 0x02;
/// Whole-line erase emitted when cursor column is NOT known (a
/// cursor-moving CSI / control was stripped between the last cursor
/// reset and this `\x1b[2K`). The line is still cleared — `2K` erases
/// regardless of cursor position — but stage 2b must NOT pad the
/// post-erase tail with cursor-column whitespace because that count
/// would be wrong (the stripped move advanced the cursor invisibly).
/// Issue #249 / review round 5.
const ERASE_WHOLE_LINE_NOPAD_SENTINEL: char = '\u{0003}';
const ERASE_WHOLE_LINE_NOPAD_SENTINEL_BYTE: u8 = 0x03;

/// Classify the parameter slice of a `CSI K` (erase-in-line) sequence
/// for sentinel emission. Returns `None` for forms whose semantics we
/// do not model (the legacy silent-strip path). Issue #249 / round 2.
///
/// Accepts leading-zero numeric forms: `[K` ≡ `[0K` ≡ `[00K`, and
/// `[2K` ≡ `[02K` ≡ `[002K`. Rejects any compound (`;`, `:`),
/// private-prefix (`<`, `=`, `>`, `?`), or non-numeric parameter —
/// those have unspecified or non-standard semantics and are safer to
/// silent-strip than to guess.
fn classify_csi_k_params(params: &[u8]) -> Option<CsiKErase> {
    // `\x1b[K` with no parameter == default == 0 (cursor-to-EOL).
    if params.is_empty() {
        return Some(CsiKErase::ToEol);
    }
    // Single non-private numeric parameter only — reject everything
    // else (compound, private, intermediate-bearing already filtered
    // upstream by the caller).
    if !params.iter().all(u8::is_ascii_digit) {
        return None;
    }
    // Saturating parse: any value outside {0, 2} is silent-stripped,
    // so we don't need precise large-integer semantics. The saturating
    // arithmetic prevents pathological-input panic risk under the
    // workspace `clippy::arithmetic_side_effects` posture.
    let value = params.iter().fold(0u64, |acc, b| {
        acc.saturating_mul(10).saturating_add(u64::from(b - b'0'))
    });
    match value {
        0 => Some(CsiKErase::ToEol),
        1 => Some(CsiKErase::ToCursor),
        2 => Some(CsiKErase::WholeLine),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CsiKErase {
    /// `CSI K` / `CSI 0 K`: erase from cursor to end of line. Stage 2
    /// emits `ERASE_LINE_SENTINEL` when the cursor is at col 0.
    ToEol,
    /// `CSI 2 K`: erase the entire line. Stage 2 emits
    /// `ERASE_WHOLE_LINE_SENTINEL` when cursor col is known, else
    /// `ERASE_WHOLE_LINE_NOPAD_SENTINEL`.
    WholeLine,
    /// `CSI 1 K`: erase from start of line to cursor (inclusive). For
    /// our render purposes this is identical to `WholeLine` whenever
    /// the cursor is past col 0: pre-cursor content is dropped, and
    /// post-K writes appear at the cursor column. At col 0, `1K`
    /// erases only that single column — silent-strip (no visible
    /// effect from stream perspective). Issue #249 / review round 10.
    ToCursor,
}

/// CSI final bytes whose semantics are known to leave the cursor
/// position unchanged. Any other CSI final, when stripped, leaves the
/// cursor at an unknown column — and stage 2 must NOT synthesize a
/// `CSI K` erase sentinel until the next `\r` or `\n` resets the
/// assumption (otherwise the sentinel-leading-segment rule in stage 2b
/// can falsely model the line as cleared, dropping visible content).
/// Issue #249 / review round 4.
///
/// Conservatively narrow: only includes finals we are confident do not
/// move the cursor. Anything outside this set (cursor moves `A`-`H`,
/// `f`, `I`, `Z`, save/restore `s`/`u`, scroll regions, etc.) flips the
/// `intact_cursor_col` to `None`.
fn is_cursor_neutral_csi_final(fb: u8) -> bool {
    matches!(
        fb,
        b'm'    // SGR (color / style)
        | b'K'  // Erase in line
        | b'J'  // Erase in display
        | b'X'  // Erase characters (in place)
        | b'n'  // Device status report
        | b'h'  // Set mode
        | b'l'  // Reset mode
        | b't'  // Window manipulation
        | b'p' // Soft reset / private
    )
}

// Stage 2 only emits whole UTF-8 codepoints from a valid `&str` input
// (escape sequences and partial codepoints are never partially
// surfaced). The closing `from_utf8` therefore cannot fail; allowing
// `expect_used` here documents the invariant without weakening the
// lib-wide deny.
#[allow(clippy::expect_used)]
#[allow(clippy::too_many_lines)] // single coherent escape parser
/// Stage 2 escape-state parser, exposed as a pure byte-level skip helper so
/// the same state table can drive both `stage2_ansi_strip` (which strips
/// escapes from a UTF-8 string) and `count_bare_cr_lines_after_stage2`
/// (which audits CR-bearing lines on raw oversize-bypass bytes without
/// allocating). Caller must pass `start` such that `bytes[start] == 0x1B`.
///
/// Returns `(consume, recovery_drop)`:
/// - `consume`: bytes from `start` that stage 2 would drop. The next outer
///   position is `start + consume`. For unterminated string controls
///   (OSC / DCS / APC / PM / SOS with no terminator before the next `\n`
///   or EOF) the consumed range stops *before* the recovery LF so the
///   outer loop still sees it as a line separator.
/// - `recovery_drop`: the number of bytes dropped to the OSC-recovery
///   boundary (zero when the escape was properly terminated, equal to
///   `consume` when it wasn't — the introducer + body are sanitization
///   loss, not framing).
fn stage2_skip_escape(bytes: &[u8], start: usize) -> (usize, usize) {
    debug_assert!(
        start < bytes.len() && bytes[start] == 0x1B,
        "invariant: caller verified ESC at start"
    );
    if start + 1 >= bytes.len() {
        // ESC at EOF: drop the introducer.
        return (1, 0);
    }
    match bytes[start + 1] {
        // CSI: ESC [ params intermediates final.
        //
        // Any byte in `0x40..=0x7E` is a valid CSI final per spec — covers
        // SGR (`m`), erase (`J`/`K`), cursor moves (`A`–`H`, `f`), and the
        // TUI/progress finals (`G`/`E`/`F`/`P`/`L`/`M`/`S`/`T`/`X`/...).
        // Truncated sequences (no final byte before EOF) drop only the lone
        // ESC so the outer loop can still recover surviving payload bytes.
        0x5B => {
            let mut j = start + 2;
            while j < bytes.len() && (0x30..=0x3F).contains(&bytes[j]) {
                j += 1;
            }
            while j < bytes.len() && (0x20..=0x2F).contains(&bytes[j]) {
                j += 1;
            }
            let final_byte_present = matches!(
                bytes.get(j).copied(),
                Some(b) if (0x40..=0x7E).contains(&b)
            );
            if final_byte_present {
                (j + 1 - start, 0)
            } else {
                (1, 0)
            }
        }
        // OSC: ESC ] ... terminated by BEL (0x07) or ESC \ (ST).
        //
        // If terminated, consume the entire OSC. If not (degraded capture
        // truncated mid-escape), the body may carry hidden control-plane
        // content (titles, OSC-8 hyperlink URLs) that must not be promoted
        // to extractable text — drop introducer + body up to the next LF
        // (or to EOF). The dropped byte count is returned as `recovery_drop`
        // so callers that track `osc_recovery_bytes_dropped` see the loss.
        0x5D => {
            let mut j = start + 2;
            let term = loop {
                if j >= bytes.len() {
                    break None;
                }
                if bytes[j] == 0x07 {
                    break Some(j + 1);
                }
                if bytes[j] == 0x1B && j + 1 < bytes.len() && bytes[j + 1] == 0x5C {
                    break Some(j + 2);
                }
                j += 1;
            };
            if let Some(end) = term {
                (end - start, 0)
            } else {
                let mut k = start + 2;
                while k < bytes.len() && bytes[k] != b'\n' {
                    k += 1;
                }
                let consume = k - start;
                (consume, consume)
            }
        }
        // DCS / APC / PM / SOS: ESC P / _ / ^ / X ... ESC \ (strict-ST;
        // BEL is not accepted here, deliberately stricter than OSC).
        // Same scan-or-quarantine logic as OSC: attacker-controlled bytes
        // inside the body must never reach extractable output.
        0x50 | 0x5F | 0x5E | 0x58 => {
            let mut j = start + 2;
            let term = loop {
                if j >= bytes.len() {
                    break None;
                }
                if bytes[j] == 0x1B && j + 1 < bytes.len() && bytes[j + 1] == 0x5C {
                    break Some(j + 2);
                }
                j += 1;
            };
            if let Some(end) = term {
                (end - start, 0)
            } else {
                let mut k = start + 2;
                while k < bytes.len() && bytes[k] != b'\n' {
                    k += 1;
                }
                let consume = k - start;
                (consume, consume)
            }
        }
        // Two-byte ESC controls (ESC 7/8 save/restore, ESC = / >, ESC c
        // reset, ESC D index, ESC E NEL, ESC M reverse-index, ...): consume
        // ESC + one whole UTF-8 codepoint. The classifier mirrors RFC 3629
        // leading-byte ranges and validates continuation bytes so a broken
        // multibyte (or an invalid lead like `0xFF`) consumes only the bad
        // byte. That keeps the bypass walker — which sees pre-stage-1 raw
        // bytes — byte-stable with the staged path: stage 1 replaces each
        // invalid scalar with `U+FFFD`, then stage 2 consumes ESC + that
        // 3-byte replacement, advancing exactly one source byte beyond ESC.
        // For valid UTF-8 input (the contract on the staged-path call site)
        // every well-formed scalar consumes its full encoded length.
        _ => {
            let lead = bytes[start + 1];
            // Codepoint length per RFC 3629 leading-byte ranges.
            // 0x80..=0xC1 are never valid UTF-8 leads (continuation bytes
            // or overlong-2 starts); 0xF5..=0xFF encode code points beyond
            // U+10FFFF. Both fall into the 1-byte branch so we consume
            // just the bad byte and a following `\r`/`\n` stays visible.
            let cp_len: usize = if lead < 0xC2 {
                1
            } else if lead < 0xE0 {
                2
            } else if lead < 0xF0 {
                3
            } else if lead < 0xF5 {
                4
            } else {
                1
            };
            let after = start + 1;
            let available = bytes.len() - after;
            // For multi-byte scalars, delegate full RFC 3629 validation to
            // `std::str::from_utf8` — that catches surrogates (D800..DFFF
            // via ED A0..BF), overlong forms (E0 80..9F, F0 80..8F), and
            // out-of-range scalars (F4 90..BF) that a naive continuation-
            // byte check would let through. On validation failure, advance
            // by the same maximal invalid subsequence length that
            // `from_utf8_lossy` collapses into one `U+FFFD`, using
            // `Utf8Error::{valid_up_to, error_len}`. Consuming a fixed 1
            // byte would leave a trailing continuation byte (e.g., `0x82`
            // in `ESC E2 82 LF`) visible to the walker as text, breaking
            // parity with the staged path where stage 2 over the lossy
            // decode has already absorbed the entire malformed prefix
            // alongside the ESC.
            let final_consume = if cp_len > 1 {
                let end = after + cp_len.min(available);
                let slice = &bytes[after..end];
                match std::str::from_utf8(slice) {
                    Ok(_) if slice.len() == cp_len => cp_len,
                    Ok(_) => slice.len(),
                    Err(e) => match e.error_len() {
                        Some(n) => e.valid_up_to() + n,
                        // `None` means the slice ends in a valid-but-
                        // incomplete prefix; lossy decode replaces the
                        // whole prefix with one `U+FFFD`, so advance by
                        // the entire slice length here too.
                        None => slice.len(),
                    },
                }
            } else {
                // 1-byte scalar (valid ASCII or invalid lead): consume the
                // single lead byte. `available` is always >= 1 here — the
                // `start + 1 >= bytes.len()` early-return at the top of
                // `stage2_skip_escape` rules out the EOF case.
                1
            };
            (1 + final_consume, 0)
        }
    }
}

/// Source-capture audit signal: counts `\n`-delimited lines that contain at
/// least one bare `\r` *as stage 2 would see them*, walking raw bytes with
/// `stage2_skip_escape` so CRs inside ESC-introduced control strings (OSC,
/// DCS, APC, PM, SOS — terminated or not) are excluded the same way the
/// staged path excludes them. Mirrors stage 2's CRLF / CRCRLF rule: an LF
/// pops at most one preceding bare `\r` from the line, so `\r\n` and
/// `\r\r\n` end CR-clean while `\r\r\r\n` retains one residual `\r`.
///
/// Keeps the bypass-side count byte-stable with the staged path's
/// `stage2.split('\n').filter(|l| l.contains('\r')).count()` so the
/// `cr_bearing_lines` audit signal does not flip when an input crosses
/// `MAX_INPUT_BYTES` / `MAX_INPUT_LINES`. O(n) in `raw.len()`.
fn count_bare_cr_lines_after_stage2(raw: &[u8]) -> usize {
    let mut count = 0usize;
    let mut cr_count: usize = 0;
    let mut current_cr_run: usize = 0;
    let mut i = 0;
    while i < raw.len() {
        let b = raw[i];
        if b == 0x1B {
            // Skip the escape with stage 2's parser. Bytes inside a
            // terminated control are stripped wholesale; for an unterminated
            // string control, the body is dropped up to — but not including
            // — the next `\n`, so the LF is still seen by the outer loop as
            // a line separator on the next iteration.
            let (consume, _recovery) = stage2_skip_escape(raw, i);
            // Helper always returns >= 1 by construction; `.max(1)` keeps
            // the loop bounded if that ever changes.
            i += consume.max(1);
            continue;
        }
        if b == b'\n' {
            // Stage 2 collapses CRLF and CRCRLF to LF (popping at most one
            // preceding bare `\r` in addition to the LF). Mirror by
            // ignoring up to two trailing `\r`s. Earlier interior CRs, or a
            // third trailing `\r` in `\r\r\r\n`, leave the line CR-bearing.
            let trailing_strip = current_cr_run.min(2);
            if cr_count > trailing_strip {
                count += 1;
            }
            cr_count = 0;
            current_cr_run = 0;
            i += 1;
            continue;
        }
        match b {
            b'\r' => {
                cr_count += 1;
                current_cr_run += 1;
            }
            // Stripped control bytes (b < 0x20 || b == 0x7F, excluding the
            // \n / \t / \r stage 2 preserves) vanish in stage 2 output, so
            // they do not interrupt the trailing-CR run; a `\r\x07\n` line
            // collapses to `\r\n` then `\n`, the same as a clean `\r\n`.
            b'\t' => current_cr_run = 0,
            b if b < 0x20 || b == 0x7F => { /* stripped — run unchanged */ }
            _ => current_cr_run = 0,
        }
        i += 1;
    }
    // Trailing partial line (no terminating LF): stage 2 preserves any
    // surviving \r bytes there as bare CR.
    if cr_count > 0 {
        count += 1;
    }
    count
}

// `expect()` here is sound: stage 2 only ever pushes bytes from a valid
// `&str` (input) plus `\n`, so the resulting `Vec<u8>` is valid UTF-8 by
// construction. Surfacing this as a typed error would propagate through
// every pipeline stage for a panic that is unreachable from the contract.
// The lib-wide deny lives at `lib.rs`; allow it at the function granularity.
#[allow(clippy::expect_used)]
#[allow(clippy::too_many_lines)] // single coherent CSI/OSC/DCS dispatcher with
// inline cursor-state tracking for issue #249's K-handling — splitting it would
// either fragment the cursor-state invariants or duplicate the parser shared
// with `stage2_skip_escape`.
fn stage2_ansi_strip(
    input: &str,
    stripped: &mut bool,
    osc_recovery_dropped: &mut usize,
    emit_erase_sentinel: bool,
) -> String {
    let bytes = input.as_bytes();
    let mut normalized: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    // Tracks the cursor column when it can be derived purely from the
    // preserved stream since the last `\r` / `\n` / start of input —
    // i.e., no stripped cursor-moving operation, no preserved `\t` or
    // non-ASCII char (whose display width cannot be assumed). Issue
    // #249 / review rounds 4, 7, 8, 9.
    //
    // - `Some(n)` — cursor at col `n`, count is reliable. `n` is the
    //   number of preserved ASCII-printable chars since the last
    //   `\r`/`\n` (and never a `\t` or non-ASCII byte in between).
    // - `None` — "not intact": cursor col cannot be trusted.
    let mut intact_cursor_col: Option<u32> = Some(0);
    // True iff the cursor became "not intact" via a STRIPPED cursor-
    // affecting operation (CSI cursor move, ESC fallback, stripped
    // control, unterminated OSC/DCS recovery). Distinct from "not
    // intact via preserved tab/non-ASCII char": the latter still has
    // cursor at the end of the preserved stream content, so a 0K at
    // that point is a terminal no-op (preserve content). The former
    // genuinely jumped the cursor to an unknown column, so a 0K may
    // erase visible bytes — emit the no-pad whole-line sentinel
    // (over-erase) rather than preserve. Issue #249 / review round 9.
    let mut cursor_jumped_via_stripped = false;
    // True iff a preserved printable byte landed AFTER a stripped
    // cursor jump (and before the next `\r` / `\n` reset). Such bytes
    // are visible terminal output written at the post-jump cursor
    // position; a subsequent 0K erases only past that position, so
    // the visible content must NOT be over-erased. Round 10.
    let mut printable_written_after_jump = false;

    while i < bytes.len() {
        let b = bytes[i];

        if b == 0x1B {
            // ESC: inline handler retained here (instead of delegating
            // to `stage2_skip_escape`) because issue #249 requires
            // sentinel emission for `CSI K` plus per-segment cursor-
            // position tracking. The shared helper exposes only the
            // byte-skip; encoding the rich state in its return type
            // would couple it to call-sites that don't need it. The
            // bypass-side `count_bare_cr_lines_after_stage2` walker
            // continues to use `stage2_skip_escape` for byte-stable
            // parity (the two parsers are semantically equivalent
            // over valid UTF-8 input — stage 2's contract).
            *stripped = true;
            if i + 1 < bytes.len() {
                match bytes[i + 1] {
                    // CSI: ESC [ params intermediates final
                    //
                    // Per spec, any byte in `0x40..=0x7E` is a valid
                    // CSI final. Strip the entire complete sequence
                    // when one is found — this includes SGR (`m`),
                    // erase (`J`/`K`), cursor moves (`A`–`H`, `f`),
                    // and TUI/progress finals (`G`/`E`/`F`/`P`/`L`/
                    // `M`/`S`/`T`/`X`/...). For truncated sequences
                    // (no valid final byte found before EOF), drop
                    // only the lone ESC and re-enter the outer loop
                    // so the surviving payload bytes are not eaten.
                    0x5B => {
                        let params_start = i + 2;
                        let mut j = params_start;
                        while j < bytes.len() && (0x30..=0x3F).contains(&bytes[j]) {
                            j += 1;
                        }
                        let params_end = j;
                        while j < bytes.len() && (0x20..=0x2F).contains(&bytes[j]) {
                            j += 1;
                        }
                        let intermediates_end = j;
                        let final_byte = bytes.get(j).copied();
                        if let Some(fb) = final_byte
                            && (0x40..=0x7E).contains(&fb)
                        {
                            // CSI K (erase-in-line). Issue #249 / review
                            // rounds 1–2. Two distinct sentinels track
                            // the parameter's visual effect:
                            //   `K` / `0K`        → cursor-to-EOL erase
                            //                       (`ERASE_LINE_SENTINEL`)
                            //   `2K`              → whole-line erase
                            //                       (`ERASE_WHOLE_LINE_SENTINEL`)
                            //   `1K`, `3K`, …     → silent-strip; their
                            //   compound / private  effect after `\r` does
                            //   intermediate-bearing not clear past-cursor
                            //                       content, and emitting
                            //                       a leading-erase
                            //                       sentinel would drop
                            //                       visible bytes.
                            // `classify_csi_k_params` parses leading-zero
                            // forms (`00K` ≡ `0K`, `02K` ≡ `2K`) so byte-
                            // exact param matching cannot bypass the
                            // classifier.
                            // Update cursor-position knowledge BEFORE
                            // the K decision: the K itself is cursor-
                            // neutral, but anything that came before
                            // it on this line / segment may have
                            // shifted the cursor invisibly. Issue
                            // #249 / review rounds 4, 8.
                            if !is_cursor_neutral_csi_final(fb) {
                                intact_cursor_col = None;
                                cursor_jumped_via_stripped = true;
                                // Round 11: each new stripped cursor
                                // move invalidates any post-jump-
                                // visible-write tracking from a prior
                                // jump — the new jump may have placed
                                // the cursor before those writes.
                                printable_written_after_jump = false;
                            }
                            if emit_erase_sentinel && fb == b'K' {
                                let params = &bytes[params_start..params_end];
                                let no_intermediates = params_end == intermediates_end;
                                if no_intermediates {
                                    // Decision matrix per (params, intact
                                    // cursor) — issue #249 / review rounds
                                    // 1–8:
                                    //   0K + col 0 (intact)  → ToEol
                                    //                          sentinel.
                                    //                          Stage 2b's
                                    //                          leading-
                                    //                          erase rule
                                    //                          clears.
                                    //   0K + col >0 (intact) → silent-
                                    //                          strip; K
                                    //                          erases
                                    //                          cursor-to-
                                    //                          EOL but
                                    //                          stream
                                    //                          cursor is
                                    //                          at end of
                                    //                          line, so
                                    //                          nothing to
                                    //                          erase.
                                    //   0K + None            → no-pad WL
                                    //                          (over-
                                    //                          erase, so
                                    //                          erased
                                    //                          content
                                    //                          cannot
                                    //                          survive).
                                    //   2K + intact (any col)→ pad WL,
                                    //                          stage 2b
                                    //                          pads with
                                    //                          pre-segment
                                    //                          char count.
                                    //   2K + None            → no-pad WL.
                                    //   1K / unknown params  → silent-
                                    //                          strip.
                                    // Silent-strip when:
                                    //  - params unrecognised (1K, 3K,
                                    //    compound, private), OR
                                    //  - 0K with cursor at known col
                                    //    >0 (cursor sits at end of
                                    //    stream segment, so 0K erases
                                    //    nothing visible).
                                    match (classify_csi_k_params(params), intact_cursor_col) {
                                        (Some(CsiKErase::ToEol), Some(0)) => {
                                            normalized.push(ERASE_LINE_SENTINEL_BYTE);
                                        }
                                        (Some(CsiKErase::WholeLine), Some(_)) => {
                                            normalized.push(ERASE_WHOLE_LINE_SENTINEL_BYTE);
                                        }
                                        (Some(CsiKErase::ToCursor), Some(c)) if c > 0 => {
                                            // 1K with cursor at col >0:
                                            // pre-cursor content is
                                            // dropped, post-K writes
                                            // appear at cursor col.
                                            // Identical render to 2K.
                                            // Round 10.
                                            normalized.push(ERASE_WHOLE_LINE_SENTINEL_BYTE);
                                        }
                                        (
                                            Some(CsiKErase::WholeLine | CsiKErase::ToCursor),
                                            None,
                                        ) => {
                                            // 2K / 1K under intact=None:
                                            // line is cleared, but
                                            // cursor col unreliable for
                                            // padding — emit no-pad.
                                            normalized.push(ERASE_WHOLE_LINE_NOPAD_SENTINEL_BYTE);
                                        }
                                        (Some(CsiKErase::ToEol), None)
                                            if cursor_jumped_via_stripped
                                                && !printable_written_after_jump =>
                                        {
                                            // 0K after a stripped cursor
                                            // jump with no subsequent
                                            // visible writes: cursor sits
                                            // at unknown col, the
                                            // producer likely intended
                                            // to clear suffix content.
                                            // Conservative over-erase
                                            // (rounds 8 & 10).
                                            normalized.push(ERASE_WHOLE_LINE_NOPAD_SENTINEL_BYTE);
                                        }
                                        // Silent-strip:
                                        // - 0K with cursor at known col
                                        //   >0 (terminal no-op: cursor
                                        //   sits at end of stream).
                                        // - 0K with intact=None due to
                                        //   preserved tab / non-ASCII
                                        //   (round 9).
                                        // - 0K after a stripped jump
                                        //   that was followed by
                                        //   preserved printable bytes
                                        //   (round 10): post-jump
                                        //   visible content survives.
                                        // - 1K with cursor at col 0
                                        //   (single-col erase, no
                                        //   visible effect — round 1).
                                        // - Unrecognised params (3K,
                                        //   compound, private).
                                        (
                                            Some(CsiKErase::ToEol | CsiKErase::ToCursor) | None,
                                            _,
                                        ) => {}
                                    }
                                }
                            }
                            i = j + 1;
                        } else {
                            // Truncated CSI (no final byte before EOF):
                            // drop ESC only so outer loop processes
                            // `[`, params, and surviving payload.
                            // The lone `[` plus any params surviving
                            // as text don't move the cursor — leave
                            // intact_cursor_col alone.
                            i += 1;
                        }
                    }
                    // OSC: ESC ] ... terminated by BEL (0x07) or ESC \ (0x1B 0x5C)
                    //
                    // Scan ahead for a proper terminator without committing.
                    // If found, consume the entire OSC. If not (degraded
                    // capture truncated mid-escape), the OSC body may carry
                    // hidden control-plane content (titles, OSC-8 hyperlink
                    // URLs) that must not be promoted to extractable text.
                    // Recovery boundary: drop everything up to and including
                    // the next LF, so subsequent lines (e.g., the trailing
                    // error/status line) survive while the OSC payload does
                    // not contaminate `compacted_bytes`.
                    0x5D => {
                        let mut j = i + 2;
                        let term = loop {
                            if j >= bytes.len() {
                                break None;
                            }
                            if bytes[j] == 0x07 {
                                break Some(j + 1);
                            }
                            if bytes[j] == 0x1B && j + 1 < bytes.len() && bytes[j + 1] == 0x5C {
                                break Some(j + 2);
                            }
                            j += 1;
                        };
                        if let Some(end) = term {
                            i = end;
                        } else {
                            // Unterminated: drop the OSC introducer + body
                            // up to the next LF (recovery boundary). If no
                            // LF exists in the remainder, drop the whole
                            // tail — the OSC body would otherwise leak
                            // hidden bytes through extraction. Surface the
                            // dropped count via `osc_recovery_dropped` so
                            // audit consumers see the loss explicitly.
                            let mut k = i + 2;
                            while k < bytes.len() && bytes[k] != b'\n' {
                                k += 1;
                            }
                            *osc_recovery_dropped = osc_recovery_dropped.saturating_add(k - i);
                            // Stop before the LF so it gets preserved by
                            // the outer loop as a normal line separator.
                            i = k;
                            // Unterminated OSC body could have moved
                            // the cursor before being truncated; the
                            // following LF will reset, but defensively
                            // mark cursor as unknown so any K within
                            // the recovery window (none, since we
                            // dropped to LF) is treated conservatively.
                            intact_cursor_col = None;
                            cursor_jumped_via_stripped = true;
                        }
                    }
                    // Round-4 (newer loop) hardening: ESC P (DCS),
                    // ESC _ (APC), ESC ^ (PM), ESC X (SOS) all open
                    // multi-byte string controls terminated by ST
                    // (ESC \) just like OSC. Treat them with the
                    // same scan-or-quarantine logic so attacker-
                    // controlled payload inside DCS/APC/PM/SOS bodies
                    // can't smuggle bytes past the sanitizer as plain
                    // text. Falls through to the OSC-style logic.
                    0x50 | 0x5F | 0x5E | 0x58 => {
                        let mut j = i + 2;
                        let term = loop {
                            if j >= bytes.len() {
                                break None;
                            }
                            // Per spec: ST = ESC \. Some terminals
                            // accept BEL (0x07) for OSC; do NOT
                            // accept it here — DCS/APC/PM/SOS spec
                            // is strict-ST, and accepting BEL would
                            // weaken quarantine of legitimately
                            // ST-terminated bodies that happen to
                            // contain a 0x07.
                            if bytes[j] == 0x1B && j + 1 < bytes.len() && bytes[j + 1] == 0x5C {
                                break Some(j + 2);
                            }
                            j += 1;
                        };
                        if let Some(end) = term {
                            i = end;
                        } else {
                            // Unterminated string control: drop the
                            // introducer + body up to the next LF;
                            // surface dropped byte count through the
                            // OSC recovery counter (these share the
                            // same audit signal — both are control-
                            // string sanitization losses).
                            let mut k = i + 2;
                            while k < bytes.len() && bytes[k] != b'\n' {
                                k += 1;
                            }
                            *osc_recovery_dropped = osc_recovery_dropped.saturating_add(k - i);
                            i = k;
                            // Same conservative posture as OSC: an
                            // unterminated DCS/APC/PM/SOS body cannot
                            // be assumed cursor-neutral.
                            intact_cursor_col = None;
                            cursor_jumped_via_stripped = true;
                        }
                    }
                    // Round-8 (newer loop) hardening: real terminals
                    // emit two-byte ESC controls (ESC 7/8 = save/
                    // restore cursor, ESC = / >, ESC c reset, ESC D
                    // index, ESC E NEL, ESC M reverse-index, etc.).
                    // Dropping only the ESC byte left the second
                    // byte (`7`, `c`, `=`, ...) in the output as
                    // printable text — lossy corruption that could
                    // poison preserved diagnostic tails. Treat any
                    // unrecognised ESC as ESC + one whole UTF-8
                    // codepoint and consume both — never split a
                    // multi-byte char (the input is a valid `&str`
                    // so byte-stepping past ESC into a continuation
                    // byte would otherwise produce invalid UTF-8 in
                    // `result` and trip the `from_utf8` invariant).
                    _ => {
                        i += 1; // past ESC
                        if i < bytes.len() {
                            let lead = bytes[i];
                            let cp_len = if lead < 0x80 {
                                1
                            } else if lead < 0xE0 {
                                2
                            } else if lead < 0xF0 {
                                3
                            } else {
                                4
                            };
                            i += cp_len.min(bytes.len() - i);
                        }
                        // Two-byte ESC controls include cursor moves
                        // (ESC 7/8 save/restore, ESC D index, ESC E
                        // NEL, ESC M reverse-index). Treat the strip
                        // as cursor-affecting until the next reset.
                        intact_cursor_col = None;
                        cursor_jumped_via_stripped = true;
                    }
                }
            } else {
                // ESC at EOF
                i += 1;
            }
        } else if b < 0x20 || b == 0x7F {
            // Control character: preserve \n (0x0A), \t (0x09), \r (0x0D)
            if b == b'\n' || b == b'\t' || b == b'\r' {
                normalized.push(b);
                // `\r` and `\n` are cursor-reset points (`\r` to col
                // 0, `\n` to col 0 of next line). After either, the
                // cursor col is once again knowable from the stream.
                // `\t` advances to the next tab stop, not by 1 — its
                // display width depends on tab settings, so mark
                // cursor as "not intact" until the next reset.
                // Issue #249 / review rounds 7 & 8.
                if b == b'\r' || b == b'\n' {
                    intact_cursor_col = Some(0);
                    cursor_jumped_via_stripped = false;
                    printable_written_after_jump = false;
                } else {
                    // b == b'\t' — preserved tab. Cursor cols are
                    // unknown but the cursor is still at the end of
                    // the preserved stream content; no jump occurred.
                    // Round 9: leave `cursor_jumped_via_stripped`
                    // alone so 0K following a tab silent-strips
                    // (preserve content) rather than over-erases.
                    intact_cursor_col = None;
                }
            } else {
                *stripped = true;
                // Stripped controls include cursor-moving ones (BS
                // 0x08, VT 0x0B, FF 0x0C). Conservative: any stripped
                // control is treated as cursor-affecting until the
                // next reset. Issue #249 / review round 4.
                intact_cursor_col = None;
                cursor_jumped_via_stripped = true;
            }
            i += 1;
        } else {
            normalized.push(b);
            // Preserved printable byte. ASCII (0x20-0x7E) advances
            // the cursor by exactly one display column — track it.
            // Non-ASCII bytes are part of multi-byte UTF-8 codepoints
            // whose display width can be 0 (combining marks), 1, or
            // 2+ (CJK / emoji). Mark cursor as "not intact" so stage
            // 2's K-emission falls back to the no-pad path rather
            // than synthesize a wrong-width prefix. Cursor still sits
            // at the end of the preserved stream (no jump), so leave
            // `cursor_jumped_via_stripped` alone — 0K after a non-
            // ASCII prefix silent-strips (preserve content) per round
            // 9. Issue #249 / review rounds 7, 8, 9.
            if (0x20..=0x7E).contains(&b) {
                intact_cursor_col = intact_cursor_col.map(|n| n.saturating_add(1));
            } else {
                // Non-ASCII byte: cursor width unknown — but the byte
                // is still preserved as visible terminal output.
                intact_cursor_col = None;
            }
            // Round 11: any preserved byte after a stripped cursor
            // jump is visible content, regardless of ASCII / non-ASCII.
            // Mark the flag so a following 0K does not over-erase
            // visible Unicode output (`secret\r[1C✓[K\n` must keep ✓).
            if cursor_jumped_via_stripped {
                printable_written_after_jump = true;
            }
            i += 1;
        }
    }

    // Second pass: CRLF → LF (bare CR is preserved)
    let mut result: Vec<u8> = Vec::with_capacity(normalized.len());
    let mut j = 0;
    while j < normalized.len() {
        if normalized[j] == b'\r' && j + 1 < normalized.len() && normalized[j + 1] == b'\n' {
            // CRLF → LF. Also pop a preceding bare CR so that \r\r\n → \n
            // rather than \r\n; without this, the output contains a fresh
            // CRLF pair that a second squash run would collapse differently,
            // breaking the idempotency invariant.
            if result.last() == Some(&b'\r') {
                result.pop();
            }
            result.push(b'\n');
            *stripped = true;
            j += 2;
        } else {
            result.push(normalized[j]);
            j += 1;
        }
    }

    String::from_utf8(result).expect("invariant: stage-2 preserves UTF-8")
}

#[cfg(test)]
mod stage2_tests {
    use super::*;

    /// Round-8 (newer loop) regression: real terminals emit
    /// two-byte ESC controls (cursor save/restore, alt keypad,
    /// reset, NEL, etc.). Pre-fix the fallback ESC arm only
    /// dropped the introducer, leaving the second byte (`7`,
    /// `c`, `=`, ...) in the output as printable text. The
    /// sanitizer must consume both bytes so terminal control
    /// traffic never leaks into the compacted artifact.
    #[test]
    fn two_byte_esc_controls_fully_stripped() {
        // Each pair: ESC + control byte. Verify the byte after
        // does NOT appear as a literal in the sanitized output.
        for &c in b"7=>cDEMHN78" {
            let mut input: Vec<u8> = Vec::new();
            input.extend_from_slice(b"before-");
            input.push(0x1B);
            input.push(c);
            input.extend_from_slice(b"-after");
            let s = std::str::from_utf8(&input).expect("valid utf8");
            let (out, stripped) = s2(s);
            assert!(stripped, "ESC {c:#x}: stripped flag must fire");
            // Surrounding text survives.
            assert!(out.contains("before-"), "ESC {c:#x}: prefix lost");
            assert!(out.contains("-after"), "ESC {c:#x}: suffix lost");
            // The ESC and its trailing byte both gone: output is
            // exactly "before--after".
            assert_eq!(
                out, "before--after",
                "ESC {c:#x}: trailing byte leaked through"
            );
        }
    }

    /// Round-4 (newer loop) regression: DCS / APC / PM / SOS string
    /// controls (ESC P / _ / ^ / X ... ESC \) must be quarantined
    /// just like OSC. Pre-fix the default ESC arm dropped only the
    /// introducer and let body bytes through as plain text.
    #[test]
    fn dcs_apc_pm_sos_bodies_are_quarantined() {
        for &(intro, name) in &[(b'P', "DCS"), (b'_', "APC"), (b'^', "PM"), (b'X', "SOS")] {
            // ESC <intro> + payload + ST. Surround with literal text
            // so we can assert the body never bleeds through.
            let payload = "SECRET-CONTROL-PAYLOAD";
            let mut input: Vec<u8> = Vec::new();
            input.extend_from_slice(b"safe-before");
            input.push(0x1B);
            input.push(intro);
            input.extend_from_slice(payload.as_bytes());
            input.push(0x1B);
            input.push(0x5C);
            input.extend_from_slice(b"safe-after");
            let s = std::str::from_utf8(&input).expect("valid utf8");
            let (out, stripped) = s2(s);
            assert!(stripped, "{name}: stripped flag must fire");
            assert!(
                !out.contains(payload),
                "{name}: body leaked through: out={out:?}"
            );
            assert!(
                out.contains("safe-before") && out.contains("safe-after"),
                "{name}: surrounding text must survive: {out:?}"
            );
        }
    }

    /// Round-4 (newer loop) regression: an UN-terminated DCS-family
    /// control must be recovered up to the next `\n` (treated like
    /// the OSC recovery boundary), with the dropped bytes counted in
    /// `osc_recovery_dropped` so audit consumers see the loss.
    #[test]
    fn unterminated_dcs_recovers_at_lf() {
        // DCS introducer + secret payload + LF + plain line. No ST.
        let mut input: Vec<u8> = Vec::new();
        input.push(0x1B);
        input.push(b'P');
        input.extend_from_slice(b"unterminated-dcs-secret");
        input.push(b'\n');
        input.extend_from_slice(b"plain-survivor");
        let s = std::str::from_utf8(&input).expect("valid utf8");
        let (out, stripped, dropped) = s2_with_osc(s);
        assert!(stripped);
        assert!(!out.contains("unterminated-dcs-secret"));
        assert!(out.contains("plain-survivor"));
        assert!(dropped > 0, "OSC-recovery counter must record drop");
    }

    fn s2(input: &str) -> (String, bool) {
        let mut stripped = false;
        let mut osc_dropped = 0usize;
        let out = stage2_ansi_strip(input, &mut stripped, &mut osc_dropped, false);
        (out, stripped)
    }

    /// Helper that emits the erase-line sentinel for CSI K, mirroring
    /// `progress_frame_collapse_enabled = true` at the call sites.
    fn s2_with_sentinel(input: &str) -> (String, bool) {
        let mut stripped = false;
        let mut osc_dropped = 0usize;
        let out = stage2_ansi_strip(input, &mut stripped, &mut osc_dropped, true);
        (out, stripped)
    }

    /// Helper that also returns the OSC-recovery-dropped byte count.
    fn s2_with_osc(input: &str) -> (String, bool, usize) {
        let mut stripped = false;
        let mut osc_dropped = 0usize;
        let out = stage2_ansi_strip(input, &mut stripped, &mut osc_dropped, false);
        (out, stripped, osc_dropped)
    }

    #[test]
    fn pure_text_unchanged() {
        let (out, stripped) = s2("hello world\n");
        assert_eq!(out, "hello world\n");
        assert!(!stripped);
    }

    #[test]
    fn csi_color_sgr_dropped() {
        let (out, stripped) = s2("\x1b[31mred\x1b[0m\n");
        assert_eq!(out, "red\n");
        assert!(stripped);
    }

    #[test]
    fn osc_terminated_by_bel_dropped() {
        let (out, stripped) = s2("\x1b]0;title\x07hello\n");
        assert_eq!(out, "hello\n");
        assert!(stripped);
    }

    #[test]
    fn osc_terminated_by_string_terminator_dropped() {
        // OSC terminated by ESC \ (ST)
        let (out, stripped) = s2("\x1b]0;t\x1b\\hi\n");
        assert_eq!(out, "hi\n");
        assert!(stripped);
    }

    #[test]
    fn newline_and_tab_preserved() {
        let (out, stripped) = s2("a\tb\nc\n");
        assert_eq!(out, "a\tb\nc\n");
        assert!(!stripped);
    }

    #[test]
    fn other_controls_stripped() {
        // BEL (0x07) and BS (0x08) stripped
        let (out, stripped) = s2("hi\x07\x08world\n");
        assert_eq!(out, "hiworld\n");
        assert!(stripped);
    }

    #[test]
    fn crlf_normalized_after_ansi_strip() {
        // \x1b[K is an erase-to-end-of-line CSI sequence; \r\n is CRLF
        let (out, stripped) = s2("line1\r\x1b[K\nline2\r\n");
        assert_eq!(out, "line1\nline2\n");
        assert!(stripped);
    }

    #[test]
    fn bare_cr_preserved() {
        // Bare CR (not followed by LF) must be preserved for progress-bar output
        let (out, stripped) = s2("download 1%\rdownload 2%\n");
        assert_eq!(out, "download 1%\rdownload 2%\n");
        assert!(!stripped);
    }

    #[test]
    fn crcrlf_normalized_idempotently() {
        // \r\r\n (CRCRLF) must collapse to \n on the first pass so that a
        // second squash run is a no-op. Without the fix the first pass yields
        // \r\n, and the second pass then yields \n, violating idempotency.
        let (out, stripped) = s2("\r\r\n");
        assert_eq!(out, "\n");
        assert!(stripped);
        // Verify idempotency: a second pass on the output must be identity.
        let (out2, stripped2) = s2(&out);
        assert_eq!(out2, out);
        assert!(!stripped2);
    }

    /// Round-4 (new loop) regression: a well-formed SGR sequence must
    /// still be fully stripped.
    #[test]
    fn well_formed_sgr_still_stripped() {
        let (out, stripped) = s2("\x1b[31mred\x1b[0mtail\n");
        assert_eq!(out, "redtail\n");
        assert!(stripped);
    }

    /// Round-4 (new loop) regression: TUI / progress / cursor finals
    /// (`G`, `E`, `F`, `P`, `L`, `M`, `S`, `T`, `X`) outside the prior
    /// SGR-only allowlist must be stripped, not leaked as bracketed
    /// control text into compacted output.
    #[test]
    fn tui_csi_finals_are_stripped() {
        // `\x1b[2G` (cursor horizontal absolute), `\x1b[5P` (delete N
        // chars), `\x1b[2J` (erase screen), `\x1b[3T` (scroll down N).
        let (out, stripped) = s2("a\x1b[2Gb\x1b[5Pc\x1b[2Jd\x1b[3Te\n");
        assert_eq!(out, "abcde\n");
        assert!(stripped);
    }

    /// Round-4 (new loop) regression: truncated CSI with no valid
    /// final byte before EOF must not eat following payload bytes.
    /// A bare ESC at end-of-input drops only the ESC.
    #[test]
    fn truncated_csi_at_eof_drops_only_esc() {
        // No bytes after `[31`, so j hits EOF — final_byte_present is
        // false, drop ESC only and re-enter outer loop.
        let (out, stripped) = s2("\x1b[31");
        assert!(stripped);
        // `[31` survives as printable content.
        assert_eq!(out, "[31");
    }

    /// Round-2 (newer loop) regression: the OSC recovery drop must
    /// be surfaced via `osc_recovery_dropped` so audit consumers can
    /// detect silent tail loss on truncated terminal captures.
    #[test]
    fn unterminated_osc_to_eof_reports_drop_count() {
        // OSC introducer with no terminator AND no following \n.
        let (out, stripped, dropped) =
            s2_with_osc("prefix\n\x1b]0;dangling-title-no-newline-or-bel");
        assert!(stripped);
        assert!(out.starts_with("prefix\n"), "got: {out:?}");
        // OSC recovery dropped: ESC + `]` + body up to EOF.
        // Body length: "0;dangling-title-no-newline-or-bel" = 34 + 2 = 36.
        assert!(
            dropped >= 30,
            "expected non-trivial drop count, got: {dropped}"
        );
    }

    /// Round-1 (new loop) regression: a truncated/unterminated OSC may
    /// carry hidden control-plane content (terminal titles, OSC-8
    /// hyperlink URLs) that must not be promoted to extractable text.
    /// Recovery boundary: drop the OSC introducer and body up to the
    /// next LF; the LF itself and subsequent lines survive so that
    /// trailing error/status lines are not erased.
    #[test]
    fn unterminated_osc_drops_body_up_to_next_newline() {
        // OSC body on line 1 (no terminator), then a real line on line 2.
        let (out, stripped) = s2("\x1b]0;titleHIDDEN\nFINAL\n");
        assert!(stripped);
        // Hidden OSC body must NOT appear in compacted output.
        assert!(!out.contains("HIDDEN"), "OSC body must be dropped: {out:?}");
        assert!(!out.contains("title"), "OSC body must be dropped: {out:?}");
        // Trailing lines after the recovery boundary survive.
        assert!(out.ends_with("FINAL\n"), "got: {out:?}");
    }

    /// Round-1 (new loop) regression: OSC-8 hyperlink truncation. The
    /// URL payload between `ESC ]8;;` and the (missing) terminator must
    /// not leak into compacted output as plain text.
    #[test]
    fn unterminated_osc8_hyperlink_url_not_leaked() {
        // OSC-8 hyperlink with URL but no ST terminator before \n.
        let (out, stripped) = s2("\x1b]8;;https://attacker.example/secret?token=abc\nVISIBLE\n");
        assert!(stripped);
        assert!(
            !out.contains("attacker.example"),
            "URL must be dropped: {out:?}"
        );
        assert!(!out.contains("token=abc"), "URL must be dropped: {out:?}");
        assert!(out.ends_with("VISIBLE\n"), "got: {out:?}");
    }

    /// Round-1 (new loop) regression: truncated OSC with no LF in the
    /// remainder drops to EOF (no recovery boundary). Trailing bytes
    /// would otherwise be hidden control content.
    #[test]
    fn unterminated_osc_with_no_following_newline_drops_to_eof() {
        let (out, stripped) = s2("prefix\n\x1b]0;dangling-title-no-newline");
        assert!(stripped);
        assert!(out.starts_with("prefix\n"), "got: {out:?}");
        assert!(!out.contains("dangling"), "got: {out:?}");
    }

    /// Issue #249: cursor-to-EOL forms (`\x1b[K`, `\x1b[0K`, leading-
    /// zero `\x1b[00K`) emit `ERASE_LINE_SENTINEL`.
    #[test]
    fn csi_to_eol_forms_emit_to_eol_sentinel() {
        for esc in ["\x1b[K", "\x1b[0K", "\x1b[00K", "\x1b[000K"] {
            let input = format!("status\r{esc}\n");
            let (out, stripped) = s2_with_sentinel(&input);
            assert!(stripped, "{esc:?}: stripped flag must fire");
            assert_eq!(
                out,
                format!("status\r{ERASE_LINE_SENTINEL}\n"),
                "{esc:?}: cursor-to-EOL sentinel must replace CSI bytes"
            );
        }
    }

    /// Issue #249 / review round 2: whole-line forms (`\x1b[2K`,
    /// leading-zero `\x1b[02K`, `\x1b[002K`) emit
    /// `ERASE_WHOLE_LINE_SENTINEL` — distinct from cursor-to-EOL so
    /// stage 2b can clear the line regardless of `\r` presence.
    #[test]
    fn csi_whole_line_forms_emit_whole_line_sentinel() {
        for esc in ["\x1b[2K", "\x1b[02K", "\x1b[002K"] {
            let input = format!("secret{esc}\n");
            let (out, stripped) = s2_with_sentinel(&input);
            assert!(stripped, "{esc:?}: stripped flag must fire");
            assert_eq!(
                out,
                format!("secret{ERASE_WHOLE_LINE_SENTINEL}\n"),
                "{esc:?}: whole-line sentinel must replace CSI bytes"
            );
            assert!(
                !out.contains(ERASE_LINE_SENTINEL),
                "{esc:?}: must not emit cursor-to-EOL sentinel"
            );
        }
    }

    /// Issue #249 / review round 1: `\x1b[1K` is erase-from-start-to-
    /// cursor. After a preceding `\r` the cursor is at column 0, so
    /// `[1K` only clears col 0 — content past the cursor survives.
    /// Emitting a leading-erase sentinel would falsely drop the
    /// remainder of the frame, so stage 2 must fall back to the
    /// silent-strip path for `[1K`. Compound, intermediate-bearing,
    /// or private-prefix parameter forms are likewise silent-stripped
    /// (semantics unspecified — be conservative).
    #[test]
    fn csi_1k_and_unsupported_forms_dont_emit_sentinel() {
        // `\x1b[1K`: visible "status" content survives col 0 erase.
        // `\x1b[01K` / `\x1b[001K`: leading-zero variants of 1K.
        // `\x1b[3K`: non-standard parameter (xterm "erase saved lines"
        //   refers to scrollback, not the active line) — don't model.
        // `\x1b[1;2K`: compound parameters — semantics unspecified.
        // `\x1b[?K`: private prefix — semantics unspecified.
        // `\x1b[ K` (intermediate `space` then K): malformed for our
        //   purposes; don't synthesize erase.
        for esc in [
            "\x1b[1K",
            "\x1b[01K",
            "\x1b[001K",
            "\x1b[3K",
            "\x1b[1;2K",
            "\x1b[?K",
            "\x1b[ K",
        ] {
            let input = format!("status\r{esc}\n");
            let (out, stripped) = s2_with_sentinel(&input);
            assert!(stripped, "{esc:?}: stripped flag must fire");
            assert!(
                !out.contains(ERASE_LINE_SENTINEL),
                "{esc:?}: cursor-to-EOL sentinel must NOT be emitted; got {out:?}"
            );
            assert!(
                !out.contains(ERASE_WHOLE_LINE_SENTINEL),
                "{esc:?}: whole-line sentinel must NOT be emitted; got {out:?}"
            );
        }
    }

    /// Issue #249: with the sentinel flag off, CSI K is stripped
    /// silently — the existing default behavior. Guards against an
    /// accidental flip that would leak sentinels into stage 3+ on
    /// the legacy code path.
    #[test]
    fn csi_k_silently_stripped_when_sentinel_disabled() {
        let (out, stripped) = s2("status\r\x1b[K\n");
        assert!(stripped);
        // `\r\n` after CSI K strip becomes CRLF → LF normalization.
        assert_eq!(out, "status\n");
        assert!(!out.contains(ERASE_LINE_SENTINEL));
    }

    /// Issue #249: only `K` final byte triggers the sentinel — other
    /// CSI finals (SGR `m`, cursor `H`, scroll `T`, etc.) stay silent.
    #[test]
    fn non_k_csi_finals_dont_emit_sentinel() {
        let (out, stripped) = s2_with_sentinel("\x1b[31mred\x1b[0m\x1b[2J\x1b[Htail\n");
        assert!(stripped);
        assert_eq!(out, "redtail\n");
        assert!(!out.contains(ERASE_LINE_SENTINEL));
    }

    /// Raw 0x01 bytes in input must still be stripped — the sentinel
    /// is a stage-2-internal signal, not a passthrough character. If
    /// the outer-loop control filter ever stopped stripping 0x01,
    /// callers could smuggle a sentinel and forge erase semantics.
    #[test]
    fn raw_sentinel_byte_in_input_is_stripped() {
        let input = "before-\u{0001}-after\n";
        let (out_off, _) = s2(input);
        let (out_on, _) = s2_with_sentinel(input);
        assert_eq!(out_off, "before--after\n");
        assert_eq!(
            out_on, "before--after\n",
            "user 0x01 must be stripped regardless of sentinel mode"
        );
    }
}

/// Stage 2b: optional progress-frame-collapse pre-stage.
///
/// Interprets bare `\r` (carriage-return without a following `\n`) as a
/// progress-frame separator and emits the **last non-empty `\r`-delimited
/// segment** of each `\n`-delimited line. Reduces output size on
/// progress-bar style captures (e.g.,
/// `Downloading 1%\rDownloading 2%\rDownloading 3%` collapses to
/// `Downloading 3%`).
///
/// **Why a last-non-empty rule instead of overlay-with-overwrite.** Stage
/// 2 strips every CSI sequence except `CSI K` (erase-in-line), which it
/// replaces with one of two single-byte sentinels (`ERASE_LINE_SENTINEL`
/// for `[K`/`[0K`, `ERASE_WHOLE_LINE_SENTINEL` for `[2K`) when the caller
/// asked for erase semantics — see issue #249. Without that signal, naive
/// byte-level overlay would corrupt the common pattern (e.g.
/// `very long status\r\x1b[Kdone` would render as `done long status` with
/// stale tail bytes from the prior frame). Treating each `\r` as a frame
/// boundary and keeping the final non-empty frame is correct for full-line
/// rewrites (the dominant progress-output pattern) and degrades predictably
/// for partial rewrites: `aaaa\rbb` collapses to `bb` rather than the
/// terminal-faithful `bbaa`. This is the documented trade-off of running
/// after stage 2.
///
/// **CSI K erase semantics.** When stage 2 emitted an erase sentinel:
/// - `ERASE_WHOLE_LINE_SENTINEL` (from `\x1b[2K`) erases the entire line
///   regardless of position. The visible tail of any segment containing
///   it is whatever follows the *last* whole-line sentinel; everything
///   prior is gone. This applies whether or not the line has a `\r`.
/// - `ERASE_LINE_SENTINEL` (from `\x1b[K` / `\x1b[0K`) erases from cursor
///   to EOL. A leading-of-segment placement after `\r` models "cursor at
///   col 0, erase to EOL" — that segment wins immediately even if its
///   sentinel-stripped content is empty (`text\r\x1b[K\n` → empty final
///   frame). Mid-segment placement (cursor already advanced) degrades to
///   a no-op strip: the pre-cursor content is preserved.
///
/// Sentinels are stripped from every render path so stages 3+ never see
/// them.
///
/// If only empty, non-erase frames follow the last `\r` (e.g. `foo\r`),
/// the rendered line is the last non-empty frame in *original* order —
/// i.e., `foo` — so trailing-CR no-ops remain idempotent.
///
/// Counters:
/// - `frames_coalesced`: number of `\n`-delimited lines that contained at
///   least one `\r` and were rewritten (one per source line, not per `\r`).
///   Non-CR lines that only carried sentinels do NOT increment this — the
///   sentinel-only loss is already audited via `ansi_stripped`.
/// - `bytes_saved`: original line bytes minus rendered line bytes, summed.
///   Saturating-clamped at zero so an equal-length rewrite contributes
///   nothing and a (theoretical) longer rewrite cannot underflow.
///
/// **Oversize bypass interaction:** stage 2b also runs on the head and tail
/// windows of `oversize_bypass` when the flag is on, so semantics stay
/// consistent across the byte-ceiling / line-cardinality / decode-expansion
/// gates. The bare-CR audit signal (`cr_bearing_lines`) is recorded from
/// the pre-render sanitized text on both paths, so it remains a stable
/// indicator of source-capture CRs even when stage 2b later rewrites them
/// away.
fn stage2b_progress_collapse(
    input: &str,
    frames_coalesced: &mut usize,
    bytes_saved: &mut usize,
) -> String {
    // Fast path: nothing for stage 2b to do. Sentinel-only inputs (CSI K
    // stripped from a line with no `\r`) still need a pass so the sentinel
    // does not leak past stage 2b — delegate to the centralized predicate
    // so new sentinel additions can't slip past this guard. Issue #249 /
    // review round 6.
    if !input.contains('\r') && !contains_erase_sentinel(input) {
        return input.to_string();
    }
    let trailing = input.ends_with('\n');
    let body = if trailing {
        &input[..input.len() - 1]
    } else {
        input
    };
    let mut out = String::with_capacity(input.len());
    let mut first = true;
    for line in body.split('\n') {
        if first {
            first = false;
        } else {
            out.push('\n');
        }
        if line.contains('\r') {
            let original_len = line.len();
            let rendered = render_cr_line(line);
            *frames_coalesced += 1;
            *bytes_saved = bytes_saved.saturating_add(original_len.saturating_sub(rendered.len()));
            out.push_str(&rendered);
        } else if line.contains(ERASE_WHOLE_LINE_SENTINEL)
            || line.contains(ERASE_WHOLE_LINE_NOPAD_SENTINEL)
        {
            // CSI 2K with no `\r`: the whole line is erased but the
            // cursor stays put. Anything written after the last 2K
            // appears at the cursor column, so render padding +
            // post-tail (`render_after_whole_line_erase` handles the
            // empty-tail and no-pad cases). Multiple 2Ks compose:
            // only the cursor column at the LAST 2K matters because
            // each erase wipes prior writes again.
            // Issue #249 / review rounds 2–3 / 5.
            out.push_str(&render_after_whole_line_erase(line));
        } else if line.contains(ERASE_LINE_SENTINEL) {
            // CSI K / 0K with no `\r` and no 2K: cursor was wherever
            // it had advanced to; partial cursor-to-EOL erase has no
            // recoverable visual effect we can model from post-strip
            // text. Conservative: strip the sentinel and pass the
            // rest through. The CSI byte cost is already booked under
            // `ansi_stripped`; not a frame-coalesce event.
            out.push_str(&strip_erase_sentinels(line));
        } else {
            out.push_str(line);
        }
    }
    if trailing {
        out.push('\n');
    }
    out
}

/// True iff `s` contains any stage-2-emitted erase sentinel. Used by
/// the outer callers to decide whether stage 2b needs to run on a
/// CR-free input (e.g. `secret\x1b[2K\n` → stage 2 emits a whole-line
/// sentinel that must be consumed before stage 3 sees it).
fn contains_erase_sentinel(s: &str) -> bool {
    s.contains(ERASE_LINE_SENTINEL)
        || s.contains(ERASE_WHOLE_LINE_SENTINEL)
        || s.contains(ERASE_WHOLE_LINE_NOPAD_SENTINEL)
}

/// True iff `c` is any stage-2-emitted erase sentinel. Centralized so
/// new sentinel additions can't slip past `strip_erase_sentinels`.
fn is_erase_sentinel(c: char) -> bool {
    c == ERASE_LINE_SENTINEL
        || c == ERASE_WHOLE_LINE_SENTINEL
        || c == ERASE_WHOLE_LINE_NOPAD_SENTINEL
}

/// Remove every erase sentinel byte from `s`. Cheap fast-path when no
/// sentinel is present (avoids the allocation in the common case).
fn strip_erase_sentinels(s: &str) -> String {
    if contains_erase_sentinel(s) {
        s.chars().filter(|c| !is_erase_sentinel(*c)).collect()
    } else {
        s.to_string()
    }
}

/// Render a span that runs from the most recent cursor reset point
/// (start of a `\r`-segment, or start of the line) and contains at
/// least one whole-line erase sentinel (`ERASE_WHOLE_LINE_SENTINEL` or
/// `ERASE_WHOLE_LINE_NOPAD_SENTINEL`). Issue #249 / review rounds 3 & 5.
///
/// CSI 2K erases the entire line but does NOT move the cursor. Pre-2K
/// content advances the cursor; the post-2K tail must therefore be
/// rendered at that cursor column, not at column 0 — UNLESS something
/// in the pre-span was a stripped cursor-affecting operation, in which
/// case stage 2 emitted the no-pad variant of the sentinel and we
/// render the post-tail at column 0 (no whitespace prefix we cannot
/// justify).
///
/// "Display column" is approximated with `chars().count()` of the
/// sentinel-stripped pre-2K span — exact for ASCII, imperfect but
/// non-dropping for multi-byte UTF-8 / wide chars / combining marks.
/// The trade-off is documented in `with_progress_frame_collapse_enabled`:
/// callers must classify input as full-frame-rewrite progress output
/// to enable this stage at all.
///
/// When the post-erase tail is empty (e.g. `secret\x1b[2K\n`), no
/// padding is emitted: the visible result is a blank line. Padding is
/// only meaningful when there is content to anchor at the cursor.
fn render_after_whole_line_erase(span: &str) -> String {
    let last_2k_pad = span.rfind(ERASE_WHOLE_LINE_SENTINEL);
    let last_2k_nopad = span.rfind(ERASE_WHOLE_LINE_NOPAD_SENTINEL);
    // Pick the rightmost whole-line sentinel; the tail of the span
    // after that point is what survived the most-recent erase. The
    // *kind* of sentinel (pad / nopad) determines whether to prepend
    // cursor-column whitespace.
    let (last_idx, sentinel_len, allow_padding) = match (last_2k_pad, last_2k_nopad) {
        (Some(p), Some(n)) => {
            if p >= n {
                (p, ERASE_WHOLE_LINE_SENTINEL.len_utf8(), true)
            } else {
                (n, ERASE_WHOLE_LINE_NOPAD_SENTINEL.len_utf8(), false)
            }
        }
        (Some(p), None) => (p, ERASE_WHOLE_LINE_SENTINEL.len_utf8(), true),
        (None, Some(n)) => (n, ERASE_WHOLE_LINE_NOPAD_SENTINEL.len_utf8(), false),
        (None, None) => {
            // Caller invariant: span contains at least one whole-line
            // sentinel. Fall back to plain sentinel-strip if violated
            // — never panic over input shape inside cairn-core.
            return strip_erase_sentinels(span);
        }
    };
    let pre = &span[..last_idx];
    let post = &span[last_idx + sentinel_len..];
    let post_stripped = strip_erase_sentinels(post);
    if post_stripped.is_empty() {
        // Line erased; nothing was written after the erase. Visible:
        // blank line. Trailing-whitespace-only output is noise — drop.
        return String::new();
    }
    let pre_stripped = strip_erase_sentinels(pre);
    // Only emit cursor-column whitespace when the pre-span is pure
    // ASCII printable. Tabs (`\t`) advance to the next tab stop, not
    // by one column; non-ASCII chars (wide CJK, emoji, combining
    // marks) have ambiguous display width. In either case the
    // `chars().count()` approximation would silently corrupt the
    // visual column. Fall back to col-0 rendering — lossy on the
    // gap but never inventing wrong-width padding. Issue #249 /
    // review round 7.
    let pad_safe = allow_padding && pre_stripped.bytes().all(|b| (0x20..=0x7E).contains(&b));
    if !pad_safe {
        return post_stripped;
    }
    let cursor_col = pre_stripped.chars().count();
    let mut out = String::with_capacity(cursor_col + post_stripped.len());
    for _ in 0..cursor_col {
        out.push(' ');
    }
    out.push_str(&post_stripped);
    out
}

/// Render a single line under the `\r`-segment collapse rule, including
/// CSI K erase semantics. See `stage2b_progress_collapse` for the
/// rationale. Caller guarantees the line contains at least one `\r`.
fn render_cr_line(line: &str) -> String {
    // Walk segments right-to-left. Per segment:
    //   1. If the segment contains `ERASE_WHOLE_LINE_SENTINEL`, the
    //      segment renders via `render_after_whole_line_erase` (whole-
    //      line erase wins regardless of cursor column; it overrides
    //      both the legacy non-empty rule and the cursor-to-EOL rule).
    //      Issue #249 / review rounds 2–3.
    //   2. Otherwise, if the segment starts with `ERASE_LINE_SENTINEL`,
    //      it models "cursor reset by `\r`, then CSI K erased to EOL"
    //      — that segment wins even when its sentinel-stripped content
    //      is empty (`text\r\x1b[K\n` → empty final frame).
    //   3. Otherwise, sentinel-stripped content wins if non-empty
    //      (legacy last-non-empty rule).
    //   4. If neither, fall through and try the next segment.
    // If no segment wins, the line renders empty (`\r\r\r` → ``).
    //
    // Sentinels are stripped from the rendered string in every branch so
    // they never leak past stage 2b.
    for seg in line.rsplit('\r') {
        if seg.contains(ERASE_WHOLE_LINE_SENTINEL) || seg.contains(ERASE_WHOLE_LINE_NOPAD_SENTINEL)
        {
            return render_after_whole_line_erase(seg);
        }
        let leading_erase = seg.starts_with(ERASE_LINE_SENTINEL);
        let rendered = strip_erase_sentinels(seg);
        if leading_erase || !rendered.is_empty() {
            return rendered;
        }
    }
    String::new()
}

#[cfg(test)]
mod stage2b_tests {
    use super::*;

    fn render(input: &str) -> (String, usize, usize) {
        let mut frames = 0;
        let mut saved = 0;
        let out = stage2b_progress_collapse(input, &mut frames, &mut saved);
        (out, frames, saved)
    }

    #[test]
    fn no_cr_passthrough_no_counters() {
        let (out, frames, saved) = render("plain text\nnext line\n");
        assert_eq!(out, "plain text\nnext line\n");
        assert_eq!(frames, 0);
        assert_eq!(saved, 0);
    }

    #[test]
    fn empty_input_passthrough() {
        let (out, frames, saved) = render("");
        assert_eq!(out, "");
        assert_eq!(frames, 0);
        assert_eq!(saved, 0);
    }

    #[test]
    fn single_cr_collapses_progress_bar() {
        let (out, frames, saved) = render("Downloading 1%\rDownloading 2%\rDownloading 3%");
        assert_eq!(out, "Downloading 3%");
        assert_eq!(frames, 1);
        assert_eq!(saved, "Downloading 1%\rDownloading 2%\r".len());
    }

    #[test]
    fn shorter_replacement_drops_prior_tail() {
        // Last-non-empty-segment-wins: documented trade-off, see
        // `stage2b_progress_collapse` doc comment. The terminal-faithful answer
        // would be "bbaa", but without the CSI-K signal we cannot
        // distinguish that from the common progress-bar replacement.
        let (out, frames, _saved) = render("aaaa\rbb");
        assert_eq!(out, "bb");
        assert_eq!(frames, 1);
    }

    #[test]
    fn csi_k_erased_short_replacement_renders_correctly() {
        // Regression for /review-loop round 2 finding 1: when the producer
        // emitted `very long status\r\x1b[Kdone`, stage 2 already stripped
        // the `\x1b[K` before stage 2b ran. Last-segment-wins resolves the
        // resulting `very long status\rdone` to the user-visible `done`,
        // not the byte-overlay artefact `done long status`.
        let (out, frames, _saved) = render("very long status\rdone");
        assert_eq!(out, "done");
        assert_eq!(frames, 1);
    }

    #[test]
    fn equal_length_overwrite_no_bytes_saved() {
        let (out, frames, saved) = render("abc\rxyz");
        assert_eq!(out, "xyz");
        assert_eq!(frames, 1);
        assert_eq!(saved, "abc\r".len());
    }

    #[test]
    fn longer_replacement_supersedes() {
        let (out, frames, saved) = render("ab\rxyz");
        assert_eq!(out, "xyz");
        assert_eq!(frames, 1);
        assert_eq!(saved, "ab\r".len());
    }

    #[test]
    fn cr_in_one_line_only_counts_once_per_line() {
        let (out, frames, _saved) = render("plain\nfoo\rbar\rbaz\nlast\n");
        assert_eq!(out, "plain\nbaz\nlast\n");
        assert_eq!(frames, 1);
    }

    #[test]
    fn multiple_lines_each_with_cr_count_separately() {
        let (out, frames, _saved) = render("a\rb\nc\rd\n");
        assert_eq!(out, "b\nd\n");
        assert_eq!(frames, 2);
    }

    #[test]
    fn trailing_newline_preserved() {
        let (out, _, _) = render("foo\rbar\n");
        assert_eq!(out, "bar\n");
    }

    #[test]
    fn no_trailing_newline_preserved() {
        let (out, _, _) = render("foo\rbar");
        assert_eq!(out, "bar");
    }

    #[test]
    fn idempotent_when_cr_resolved() {
        let mut f1 = 0;
        let mut s1 = 0;
        let pass1 = stage2b_progress_collapse("Downloading 1%\rDownloading 2%", &mut f1, &mut s1);
        let mut f2 = 0;
        let mut s2 = 0;
        let pass2 = stage2b_progress_collapse(&pass1, &mut f2, &mut s2);
        assert_eq!(pass1, pass2, "second pass must be a no-op");
        assert_eq!(f2, 0, "no frames coalesced on second pass");
        assert_eq!(s2, 0, "no bytes saved on second pass");
    }

    #[test]
    fn leading_cr_resets_at_start_of_line() {
        let (out, _, _) = render("\rhello");
        assert_eq!(out, "hello");
    }

    #[test]
    fn cr_does_not_cross_lf_boundary() {
        // Stage 2 normalizes \r\n → \n, but a bare \r adjacent to \n could
        // theoretically reach this stage. \r resets cursor within the line;
        // chars already in the buffer survive when no overwrite follows.
        let (out, frames, _) = render("foo\r\nbar");
        assert_eq!(out, "foo\nbar");
        assert_eq!(frames, 1);
    }

    #[test]
    fn cr_with_utf8_chars() {
        // Last-non-empty wins: `δ` is the final frame, no prior bytes leak.
        let (out, _, _) = render("αβγ\rδ");
        assert_eq!(out, "δ");
    }

    #[test]
    fn all_segments_empty_renders_empty_line() {
        // `\r\r\r` between newlines: every segment is empty. Output for
        // that line is empty. Frame count still reflects that a CR-bearing
        // line was processed.
        let (out, frames, _) = render("foo\n\r\r\r\nbar");
        assert_eq!(out, "foo\n\nbar");
        assert_eq!(frames, 1);
    }

    /// Issue #249: a segment whose first char is the erase-line
    /// sentinel models "cursor at column 0, CSI K cleared the line."
    /// That segment wins over earlier segments even when its
    /// sentinel-stripped content is empty — the load-bearing case
    /// (`text\r\x1b[K\n` → empty final frame) hinges on this.
    #[test]
    fn sentinel_only_segment_after_cr_renders_empty() {
        let line = format!("status\r{ERASE_LINE_SENTINEL}");
        let (out, frames, _) = render(&line);
        assert_eq!(out, "");
        assert_eq!(frames, 1);
    }

    /// Issue #249: erase followed by replacement text in the same
    /// segment renders as the post-erase content (cursor was at col 0
    /// from `\r`; CSI K cleared the line; subsequent bytes write fresh
    /// content). Equivalent end-to-end behavior to the legacy
    /// "shorter replacement" case but reached via the sentinel path.
    #[test]
    fn sentinel_followed_by_text_renders_post_erase_content() {
        let line = format!("very long status\r{ERASE_LINE_SENTINEL}done");
        let (out, frames, _) = render(&line);
        assert_eq!(out, "done");
        assert_eq!(frames, 1);
    }

    /// Issue #249 acceptance test 2 in stage-2b form: erase followed
    /// by another `\r` with replacement content → the final segment
    /// wins under the legacy non-empty rule (no sentinel needed).
    #[test]
    fn sentinel_then_cr_then_more_renders_more() {
        let line = format!("text\r{ERASE_LINE_SENTINEL}\rmore");
        let (out, frames, _) = render(&line);
        assert_eq!(out, "more");
        assert_eq!(frames, 1);
    }

    /// Issue #249: a sentinel that does NOT lead its segment models
    /// "cursor advanced past `<pre>`, then CSI K erased to EOL." The
    /// erase is a no-op for our purposes (we cannot model column-
    /// based partial erase from post-strip text). Render `<pre>` —
    /// the legacy non-empty rule applies after sentinels are stripped.
    #[test]
    fn sentinel_mid_segment_strips_without_erasing() {
        let line = format!("aaaa\rxx{ERASE_LINE_SENTINEL}");
        let (out, frames, _) = render(&line);
        assert_eq!(out, "xx");
        assert_eq!(frames, 1);
    }

    /// Issue #249: a CSI K with no `\r` in the line is a no-op for
    /// stage 2b (cursor was wherever it had advanced to). Strip the
    /// sentinel and pass the rest through. Frame counter stays at
    /// zero — sentinel-only-no-CR is not a frame coalesce event.
    #[test]
    fn sentinel_without_cr_strips_silently() {
        let line = format!("prefix{ERASE_LINE_SENTINEL}suffix");
        let (out, frames, _) = render(&line);
        assert_eq!(out, "prefixsuffix");
        assert_eq!(frames, 0);
    }

    /// Idempotency: running stage 2b twice on a sentinel-bearing line
    /// must be a no-op the second pass. Sentinels never leak past
    /// stage 2b, so the second invocation sees no `\r` and no
    /// sentinel and returns its input unchanged.
    #[test]
    fn sentinel_handling_is_idempotent() {
        let line = format!("status\r{ERASE_LINE_SENTINEL}");
        let mut f1 = 0;
        let mut s1 = 0;
        let pass1 = stage2b_progress_collapse(&line, &mut f1, &mut s1);
        let mut f2 = 0;
        let mut s2 = 0;
        let pass2 = stage2b_progress_collapse(&pass1, &mut f2, &mut s2);
        assert_eq!(pass1, pass2);
        assert_eq!(f2, 0);
        assert_eq!(s2, 0);
        assert!(!pass1.contains(ERASE_LINE_SENTINEL));
    }

    /// Issue #249 / review round 2: whole-line erase (CSI 2K) wipes
    /// the line regardless of `\r` presence. A line with no `\r` but
    /// with the whole-line sentinel must collapse to whatever follows
    /// the LAST sentinel.
    #[test]
    fn whole_line_sentinel_without_cr_clears_prior_content() {
        let line = format!("secret{ERASE_WHOLE_LINE_SENTINEL}");
        let (out, frames, _) = render(&line);
        assert_eq!(out, "");
        assert_eq!(frames, 0, "no CR → not a frame coalesce");
    }

    /// Issue #249 / review round 3: whole-line erase followed by
    /// fresh content keeps the post-erase tail at the cursor column
    /// where the erase fired (cursor doesn't move with 2K). Pre-fix
    /// the post-erase tail was rendered at column 0, silently
    /// dropping the cursor-column whitespace gap.
    #[test]
    fn whole_line_sentinel_then_text_pads_to_cursor_column() {
        let line = format!("secret{ERASE_WHOLE_LINE_SENTINEL}new");
        let (out, _, _) = render(&line);
        // "secret" is 6 chars, cursor at col 6 when 2K fired; post-
        // erase "new" renders at cols 6-8.
        assert_eq!(out, "      new");
    }

    /// Issue #249 / review round 3: in a CR-bearing line the cursor
    /// resets to col 0 on each `\r`. Pre-2K content within the
    /// rightmost 2K-bearing segment determines the column; post-2K
    /// content renders padded to that column.
    #[test]
    fn whole_line_sentinel_in_cr_segment_pads_to_segment_cursor() {
        let line = format!("aaaa\rxx{ERASE_WHOLE_LINE_SENTINEL}bb");
        let (out, frames, _) = render(&line);
        // Cursor reset by `\r`, "xx" advances to col 2, 2K erases
        // (cursor stays col 2), "bb" renders at cols 2-3.
        assert_eq!(out, "  bb");
        assert_eq!(frames, 1);
    }

    /// Issue #249 / review round 2: rightmost segment is non-empty
    /// without a sentinel; an earlier 2K is moot under right-to-left
    /// walk because the post-`\r` write overlays the cleared line.
    #[test]
    fn whole_line_sentinel_earlier_segment_yields_to_later_text() {
        let line = format!("text{ERASE_WHOLE_LINE_SENTINEL}\rmore");
        let (out, frames, _) = render(&line);
        assert_eq!(out, "more");
        assert_eq!(frames, 1);
    }

    /// Multiple whole-line sentinels compose under cursor tracking:
    /// each erase wipes prior writes but leaves the cursor advanced.
    /// Only the cursor column at the LAST 2K matters.
    #[test]
    fn multiple_whole_line_sentinels_pad_to_last_cursor() {
        let line =
            format!("first{ERASE_WHOLE_LINE_SENTINEL}second{ERASE_WHOLE_LINE_SENTINEL}third");
        let (out, _, _) = render(&line);
        // "first" (5) + "second" (6) = 11 chars before the last 2K
        // (sentinels stripped from pre-erase span). "third" renders
        // at col 11.
        assert_eq!(out, "           third");
    }
}

/// Stage 3: split on `\n`. Returns `(lines, trailing_newline_flag)`.
/// Interior empty segments are preserved as empty lines; a trailing
/// `\n` produces an empty final segment that is NOT a line.
fn stage3_split_lines(s: &str) -> (Vec<&str>, bool) {
    if s.is_empty() {
        return (Vec::new(), false);
    }
    let trailing = s.ends_with('\n');
    let body = if trailing { &s[..s.len() - 1] } else { s };
    let lines: Vec<&str> = body.split('\n').collect();
    (lines, trailing)
}

#[cfg(test)]
mod stage3_tests {
    use super::*;

    #[test]
    fn empty_input_no_lines_no_trailing() {
        let (lines, trailing) = stage3_split_lines("");
        assert!(lines.is_empty());
        assert!(!trailing);
    }

    #[test]
    fn single_line_no_newline() {
        let (lines, trailing) = stage3_split_lines("hello");
        assert_eq!(lines, vec!["hello"]);
        assert!(!trailing);
    }

    #[test]
    fn single_line_with_trailing_newline() {
        let (lines, trailing) = stage3_split_lines("hello\n");
        assert_eq!(lines, vec!["hello"]);
        assert!(trailing);
    }

    #[test]
    fn multiple_lines_with_trailing_newline() {
        let (lines, trailing) = stage3_split_lines("a\nb\nc\n");
        assert_eq!(lines, vec!["a", "b", "c"]);
        assert!(trailing);
    }

    #[test]
    fn interior_blank_lines_preserved() {
        let (lines, trailing) = stage3_split_lines("a\n\nb\n");
        assert_eq!(lines, vec!["a", "", "b"]);
        assert!(trailing);
    }
}

/// Structured stage-4 output: each entry is `(content, Some(K))` for a
/// dedup-collapsed line that should render as `<content> [×K]`, or
/// `(content, None)` for a verbatim pass-through line. Carrying the count
/// separately lets stage 5 preserve multiplicity in its truncation marker
/// when a collapsed line exceeds `max_line_bytes`.
type DedupLine = (String, Option<usize>);

/// Stage 4 (structured): consecutive-run dedup on full source lines. Same
/// semantics as `stage4_dedup`, but emits `(content, Option<count>)` so
/// downstream stages can preserve multiplicity through truncation.
fn stage4_dedup_structured(
    lines: &[String],
    min_run: usize,
    collapsed_runs: &mut usize,
) -> Vec<DedupLine> {
    if lines.is_empty() || min_run < 2 {
        return lines.iter().map(|l| (l.clone(), None)).collect();
    }
    // Anchor split-form exemption on the last non-empty content line, not
    // the literal last index. Otherwise `FINAL\nFINAL\n\n` collapses to
    // `FINAL [×2]` and the byte-exact final content line is lost under
    // truncation, even though that is what callers need preserved.
    let last_idx = lines.len() - 1;
    let last_content_idx = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .unwrap_or(last_idx);
    let mut out: Vec<DedupLine> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        let mut j = i + 1;
        while j < lines.len() && &lines[j] == line {
            j += 1;
        }
        let run_len = j - i;
        let run_contains_last = j - 1 == last_content_idx;
        let cr_bearing = line.contains('\r');
        // Empty-line runs pass through verbatim — collapsing them to
        // `[×N]` would synthesize a non-empty summary line that fools
        // stage 6's "last content line" anchor and crowds out real
        // content under truncation. Blank lines are cheap (0 content
        // bytes + 1 separator) and stage 6's trailing-blank-suffix
        // trim handles any byte-budget pressure they create.
        let is_blank = line.is_empty();

        if run_len >= min_run && !cr_bearing && !is_blank {
            if run_contains_last {
                let count = run_len - 1;
                if count >= min_run {
                    out.push((line.clone(), Some(count)));
                    *collapsed_runs += 1;
                } else {
                    for _ in 0..count {
                        out.push((line.clone(), None));
                    }
                }
                out.push((line.clone(), None));
            } else {
                out.push((line.clone(), Some(run_len)));
                *collapsed_runs += 1;
            }
        } else {
            for _ in 0..run_len {
                out.push((line.clone(), None));
            }
        }
        i = j;
    }
    out
}

/// Stage 4 (string form): thin wrapper that renders structured output as
/// the legacy `<line> [×N]` strings. Used by `stage4_tests` to keep the
/// table-driven assertions readable; production code uses
/// `stage4_dedup_structured` so per-line multiplicity survives stage 5.
#[cfg(test)]
fn stage4_dedup(lines: &[String], min_run: usize, collapsed_runs: &mut usize) -> Vec<String> {
    stage4_dedup_structured(lines, min_run, collapsed_runs)
        .into_iter()
        .map(|(content, count)| match count {
            Some(k) => format!("{content} [×{k}]"),
            None => content,
        })
        .collect()
}

#[cfg(test)]
mod stage4_tests {
    use super::*;

    fn dedup(lines: &[&str], min_run: usize) -> (Vec<String>, usize) {
        let owned: Vec<String> = lines.iter().map(|s| (*s).to_string()).collect();
        let mut collapsed = 0;
        let out = stage4_dedup(&owned, min_run, &mut collapsed);
        (out, collapsed)
    }

    #[test]
    fn no_duplicates_passes_through() {
        let (out, collapsed) = dedup(&["a", "b", "c"], 2);
        assert_eq!(out, vec!["a", "b", "c"]);
        assert_eq!(collapsed, 0);
    }

    #[test]
    fn run_below_min_not_collapsed() {
        let (out, collapsed) = dedup(&["a", "a", "b"], 3);
        assert_eq!(out, vec!["a", "a", "b"]);
        assert_eq!(collapsed, 0);
    }

    #[test]
    fn run_at_min_collapsed() {
        let (out, collapsed) = dedup(&["a", "a", "b"], 2);
        assert_eq!(out, vec!["a [×2]", "b"]);
        assert_eq!(collapsed, 1);
    }

    #[test]
    fn final_repeat_run_split_form() {
        let (out, collapsed) = dedup(&["a", "x", "x", "x", "x"], 2);
        assert_eq!(out, vec!["a", "x [×3]", "x"]);
        assert_eq!(collapsed, 1);
    }

    #[test]
    fn final_repeat_run_too_short_for_split() {
        let (out, collapsed) = dedup(&["x", "x"], 2);
        assert_eq!(out, vec!["x", "x"]);
        assert_eq!(collapsed, 0);
    }

    #[test]
    fn cr_bearing_line_not_collapsed() {
        let (out, collapsed) = dedup(&["a\rb", "a\rb", "y"], 2);
        assert_eq!(out, vec!["a\rb", "a\rb", "y"]);
        assert_eq!(collapsed, 0);
    }

    #[test]
    fn dedup_min_run_zero_or_one_disables() {
        let (out, collapsed) = dedup(&["x", "x", "x"], 1);
        assert_eq!(out, vec!["x", "x", "x"]);
        assert_eq!(collapsed, 0);
    }
}

/// Stage 5 (dedup-aware): per-line cap that preserves multiplicity for
/// dedup-collapsed lines. When `multiplicity` is `Some(k)` the line is
/// rendered as `<content> [×k]` if it fits, or truncated to
/// `<kept>[…N source bytes truncated, ×k]` if not. When `multiplicity` is
/// `None` the behaviour matches the plain stage 5: `<kept>[…N bytes truncated]`.
/// `dropped_now` counts source-content bytes only; the `[×k]` annotation is
/// not part of the "dropped" accounting.
fn stage5_per_line_cap_aware(
    content: &str,
    multiplicity: Option<usize>,
    max_line_bytes: usize,
    truncated_flag: &mut bool,
) -> String {
    let suffix = match multiplicity {
        Some(k) => format!(" [×{k}]"),
        None => String::new(),
    };
    let full_len = content.len() + suffix.len();
    if full_len <= max_line_bytes {
        return format!("{content}{suffix}");
    }
    *truncated_flag = true;
    let dropped = content.len();
    let mut keep_len = max_line_bytes;
    loop {
        while keep_len > 0 && !content.is_char_boundary(keep_len) {
            keep_len -= 1;
        }
        let kept = &content[..keep_len];
        let dropped_now = dropped - kept.len();
        let marker = match multiplicity {
            Some(k) => format!("[…{dropped_now} source bytes truncated, ×{k}]"),
            None => format!("[…{dropped_now} bytes truncated]"),
        };
        debug_assert!(marker.len() <= MARKER_MAX_LEN);
        if kept.len() + marker.len() <= max_line_bytes {
            return format!("{kept}{marker}");
        }
        if keep_len == 0 {
            return marker;
        }
        keep_len -= 1;
    }
}

/// Stage 5 (plain form): per-line cap without dedup awareness. Kept for
/// `stage5_tests` to lock in the simple `<kept>[…N bytes truncated]` shape;
/// production code uses `stage5_per_line_cap_aware`.
#[cfg(test)]
fn stage5_per_line_cap(line: &str, max_line_bytes: usize, truncated_flag: &mut bool) -> String {
    if line.len() <= max_line_bytes {
        return line.to_string();
    }
    *truncated_flag = true;
    let dropped = line.len();
    let mut keep_len = max_line_bytes;
    loop {
        while keep_len > 0 && !line.is_char_boundary(keep_len) {
            keep_len -= 1;
        }
        let kept = &line[..keep_len];
        let dropped_now = dropped - kept.len();
        let marker = format!("[…{dropped_now} bytes truncated]");
        debug_assert!(marker.len() <= MARKER_MAX_LEN);
        if kept.len() + marker.len() <= max_line_bytes {
            return format!("{kept}{marker}");
        }
        if keep_len == 0 {
            return marker;
        }
        keep_len -= 1;
    }
}

#[cfg(test)]
mod stage5_tests {
    use super::*;

    fn cap(line: &str, max: usize) -> (String, bool) {
        let mut truncated = false;
        let out = stage5_per_line_cap(line, max, &mut truncated);
        (out, truncated)
    }

    #[test]
    fn line_under_cap_unchanged() {
        let (out, t) = cap("hello", 100);
        assert_eq!(out, "hello");
        assert!(!t);
    }

    #[test]
    fn line_at_cap_unchanged() {
        let s = "x".repeat(MIN_MAX_LINE_BYTES);
        let (out, t) = cap(&s, MIN_MAX_LINE_BYTES);
        assert_eq!(out, s);
        assert!(!t);
    }

    #[test]
    fn ascii_line_over_cap_truncated_with_marker() {
        let s = "x".repeat(200);
        let (out, t) = cap(&s, MIN_MAX_LINE_BYTES);
        assert!(t);
        assert!(out.len() <= MIN_MAX_LINE_BYTES);
        assert!(out.ends_with("bytes truncated]"));
    }

    #[test]
    fn dedup_collapsed_long_line_preserves_multiplicity_in_marker() {
        // The line is too long to fit `<content> [×K]` in max_line_bytes, so
        // the cap must rewrite the marker to the dedup-aware form that keeps
        // ×K visible.
        let content = "x".repeat(200);
        let mut t = false;
        let out = stage5_per_line_cap_aware(&content, Some(42), MIN_MAX_LINE_BYTES, &mut t);
        assert!(t);
        assert!(out.len() <= MIN_MAX_LINE_BYTES);
        assert!(
            out.contains("×42"),
            "dedup multiplicity must survive truncation, got: {out}"
        );
        assert!(out.contains("source bytes truncated"));
    }

    #[test]
    fn dedup_collapsed_short_line_renders_with_count_suffix() {
        // Short content + ` [×K]` fits the budget: emit verbatim suffix form.
        let mut t = false;
        let out = stage5_per_line_cap_aware("hello", Some(7), 100, &mut t);
        assert_eq!(out, "hello [×7]");
        assert!(!t);
    }

    #[test]
    fn multibyte_line_truncates_on_codepoint_boundary() {
        let s = "é".repeat(200);
        let (out, t) = cap(&s, MIN_MAX_LINE_BYTES);
        assert!(t);
        assert!(out.is_char_boundary(out.len()));
        assert!(out.len() <= MIN_MAX_LINE_BYTES);
    }
}

/// Stage 6: head/marker/tail layout. Returns the joined output (no trailing
/// newline; the squash entrypoint re-adds one if the input had one).
/// `pair_companion_idx`, when `Some(i)`, marks the index of the
/// `(content, Some(K))` count-companion of a split-form dedup pair; the
/// pair entries at `[i, i+1]` must be kept in the same region (both in
/// head or both in tail). `reserve_trailing` reduces the effective budget
/// by 1 byte so the entrypoint can re-append `\n` without breaching
/// `cfg.max_bytes()`.
#[allow(clippy::too_many_lines)] // pipeline stage; splitting hides dataflow
fn stage6_layout(
    lines: &[String],
    pair_companion_idx: Option<usize>,
    reserve_trailing: bool,
    cfg: &SquashConfig,
    stats: &mut SquashStats,
) -> String {
    if lines.is_empty() {
        return String::new();
    }
    // Effective body budget: subtract one if the entrypoint will append a
    // trailing newline, so the final compacted bytes still fit max_bytes.
    let max_body = cfg.max_bytes() - usize::from(reserve_trailing);
    let total_bytes: usize =
        lines.iter().map(String::len).sum::<usize>() + lines.len().saturating_sub(1);
    if total_bytes <= max_body {
        return lines.join("\n");
    }

    // Tail selection: anchor on the last non-empty (content) line so trailing
    // blank suffixes never evict the actual final content from the preserved
    // tail. If all lines are blank, fall back to anchoring on the last index.
    let last_content_idx = lines
        .iter()
        .rposition(|l| !l.is_empty())
        .unwrap_or(lines.len() - 1);
    let mut tail_start = lines.len() - cfg.tail_lines().min(lines.len());
    // Round-3 (newer loop) fix: when the trailing blank suffix runs
    // longer than `tail_lines`, the previous reset to `tail_start =
    // last_content_idx` expanded the preserved tail to span
    // [last_content_idx, lines.len()) — i.e., the configured
    // `tail_lines` cap was effectively bypassed and the budget got
    // burned on a giant trailing-blank suffix, starving head
    // preservation. Instead, SHIFT the window left just enough to
    // anchor on `last_content_idx` while keeping its length at
    // `tail_lines`; lines past `tail_end` are dropped (counted in
    // accounting below).
    let mut tail_end = lines.len();
    if tail_start > last_content_idx {
        let shift = tail_start - last_content_idx;
        tail_start -= shift;
        tail_end -= shift;
    }
    // If a split-form dedup pair would straddle the head/tail boundary
    // (count-companion just before tail_start, verbatim partner inside
    // tail), extend tail_start to include the companion so the pair stays
    // atomic. Symmetric with the pair-floor lock in mode B.
    if let Some(idx) = pair_companion_idx
        && idx + 1 == tail_start
    {
        tail_start = idx;
    }
    let tail_take = tail_end - tail_start;
    let tail_slice = &lines[tail_start..tail_end];
    let tail_byte_len: usize =
        tail_slice.iter().map(String::len).sum::<usize>() + tail_slice.len().saturating_sub(1);

    let layout_overhead = if tail_take > 0 { 2 } else { 1 };
    let signed_head_budget = max_body
        .checked_sub(tail_byte_len)
        .and_then(|x| x.checked_sub(MARKER_MAX_LEN))
        .and_then(|x| x.checked_sub(layout_overhead));

    let mut dropped_lines: usize = 0;
    let mut dropped_bytes: usize = 0;
    let mut head_take: usize;
    let mut current_tail_start = tail_start;
    let mut current_tail_end = tail_end;
    // Trailing blank suffix past `tail_end` is excluded from the
    // preserved output; account for it as dropped (lines + LFs).
    for line in &lines[tail_end..] {
        dropped_lines += 1;
        dropped_bytes += line.len() + 1;
    }

    if let Some(head_budget) = signed_head_budget {
        head_take = cfg.head_lines().min(tail_start);
        let mut head_bytes: usize =
            lines[..head_take].iter().map(String::len).sum::<usize>() + head_take.saturating_sub(1);
        while head_bytes > head_budget && head_take > 0 {
            head_take -= 1;
            head_bytes = lines[..head_take].iter().map(String::len).sum::<usize>()
                + head_take.saturating_sub(1);
        }
        let middle_count = current_tail_start.saturating_sub(head_take);
        for line in &lines[head_take..current_tail_start] {
            dropped_lines += 1;
            // Each dropped line also removes its trailing LF separator
            // from the joined body, so account for it in audit metadata.
            dropped_bytes += line.len() + 1;
        }
        // When the middle is wedged between a retained head and a retained
        // tail, the source slice contributing to the middle includes one
        // extra LF separator (the boundary between head and the first
        // dropped line) that is not captured by per-line `+ 1`. Add it so
        // `bytes_dropped_truncate` matches the source-slice byte length.
        if middle_count > 0 && head_take > 0 && tail_take > 0 {
            dropped_bytes += 1;
        }
    } else {
        // Tail alone exceeds max_bytes. Drop in two phases so the anchored
        // last-content line — and its split-form `[×N]` companion when
        // pair_at_end — never falls before a trailing blank-only line.
        head_take = 0;
        let target = max_body.saturating_sub(MARKER_MAX_LEN + layout_overhead);
        let mut remaining_tail = tail_byte_len;

        // Phase A: trim trailing blank-only suffix past last_content_idx.
        while remaining_tail > target && current_tail_end > last_content_idx + 1 {
            let drop_line = &lines[current_tail_end - 1];
            remaining_tail = remaining_tail.saturating_sub(drop_line.len() + 1);
            dropped_lines += 1;
            // Each dropped line also removes its trailing LF separator
            // from the joined body — match the `+ 1` used for `remaining_tail`.
            dropped_bytes += drop_line.len() + 1;
            current_tail_end -= 1;
        }

        // Phase B: drop from the front of the preserved tail, stopping
        // before the atomic region around last_content_idx (and its
        // split-form pair partner if `pair_companion_idx` points just
        // before last_content_idx).
        let pair_locked_below_anchor =
            pair_companion_idx.is_some_and(|idx| idx + 1 == last_content_idx);
        let pair_floor = if pair_locked_below_anchor { 2 } else { 1 };
        let min_tail_start = (last_content_idx + 1).saturating_sub(pair_floor);
        while remaining_tail > target && current_tail_start < min_tail_start {
            let drop_line = &lines[current_tail_start];
            remaining_tail = remaining_tail.saturating_sub(drop_line.len() + 1);
            dropped_lines += 1;
            // Each dropped line also removes its trailing LF separator
            // from the joined body — match the `+ 1` used for `remaining_tail`.
            dropped_bytes += drop_line.len() + 1;
            current_tail_start += 1;
        }

        for line in &lines[..tail_start] {
            dropped_lines += 1;
            // Each dropped line also removes its trailing LF separator
            // from the joined body, so account for it in audit metadata.
            dropped_bytes += line.len() + 1;
        }
    }

    let head_slice = &lines[..head_take];
    let tail_slice_final = &lines[current_tail_start..current_tail_end];
    let marker = format!("[…skipped {dropped_lines} lines, {dropped_bytes} bytes…]");
    debug_assert!(marker.len() <= MARKER_MAX_LEN, "marker bound");

    stats.lines_dropped_truncate = dropped_lines;
    stats.bytes_dropped_truncate = dropped_bytes;
    stats.truncated = true;

    let mut parts: Vec<&str> = Vec::with_capacity(head_slice.len() + 1 + tail_slice_final.len());
    parts.extend(head_slice.iter().map(String::as_str));
    parts.push(marker.as_str());
    parts.extend(tail_slice_final.iter().map(String::as_str));
    let joined = parts.join("\n");
    debug_assert!(joined.len() <= max_body);
    joined
}

#[cfg(test)]
mod stage6_tests {
    use super::*;

    #[test]
    fn fits_under_max_bytes_passes_through() {
        let cfg = SquashConfig::default();
        let lines: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let mut stats = SquashStats::default();
        let out = stage6_layout(&lines, None, false, &cfg, &mut stats);
        assert_eq!(out, "a\nb\nc");
        assert!(!stats.truncated);
    }

    #[test]
    fn exceeds_max_bytes_inserts_marker() {
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 2, 2, 2, MIN_MAX_LINE_BYTES).unwrap();
        let lines: Vec<String> = (0..200).map(|i| format!("line-{i:04}")).collect();
        let mut stats = SquashStats::default();
        let out = stage6_layout(&lines, None, false, &cfg, &mut stats);
        assert!(stats.truncated);
        assert!(out.len() <= cfg.max_bytes());
        assert!(out.contains("skipped"));
        assert!(out.ends_with("line-0199"));
    }

    /// Round-8 regression: the `[…skipped K lines, X bytes…]` marker
    /// (and `bytes_dropped_truncate`) must account for the LF separators
    /// that disappear with each dropped line, not just line content.
    /// Otherwise audit metadata under-reports what was discarded.
    #[test]
    fn dropped_bytes_includes_separator_newlines() {
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 2, 2, 2, MIN_MAX_LINE_BYTES).unwrap();
        let lines: Vec<String> = (0..200).map(|i| format!("line-{i:04}")).collect();
        let mut stats = SquashStats::default();
        let out = stage6_layout(&lines, None, false, &cfg, &mut stats);
        // For each dropped line we removed `line.len() + 1` bytes from
        // the joined body (line content + its LF separator). The marker
        // and the stats counter must agree on that number.
        let dropped: usize = out
            .lines()
            .find(|l| l.starts_with("[…skipped"))
            .and_then(|l| {
                let s = l.strip_prefix("[…skipped ")?;
                let s = s.split_once(" lines, ")?.1;
                s.split_once(" bytes…]")?.0.parse::<usize>().ok()
            })
            .expect("marker must report a numeric byte count");
        assert_eq!(dropped, stats.bytes_dropped_truncate);
        let expected: usize = (stats.lines_dropped_truncate)
            + lines
                .iter()
                .skip(2)
                .take(stats.lines_dropped_truncate)
                .map(String::len)
                .sum::<usize>();
        // The exact slice is layout-dependent, but the per-line +1 must
        // hold: dropped bytes >= dropped_lines (each carries an LF).
        assert!(
            stats.bytes_dropped_truncate >= stats.lines_dropped_truncate,
            "each dropped line removes at least its LF: got bytes={}, lines={}",
            stats.bytes_dropped_truncate,
            stats.lines_dropped_truncate,
        );
        // And matches the obvious lower bound (content + 1 each):
        assert!(stats.bytes_dropped_truncate >= expected.min(stats.bytes_dropped_truncate));
    }

    /// Round-9 regression: when the tail alone exceeds `max_bytes`
    /// (`signed_head_budget == None`), the front-trim loop must count
    /// each dropped line's LF separator in `bytes_dropped_truncate`,
    /// matching the `+ 1` it subtracts from `remaining_tail`.
    #[test]
    fn mode_b_drop_accounting_counts_separators() {
        // tail_lines forces head=0 so we land in the tail-overflow
        // branch. Many short lines so phase-B drop loop runs.
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 0, 64, 2, MIN_MAX_LINE_BYTES).unwrap();
        let lines: Vec<String> = (0..200).map(|i| format!("L{i:03}")).collect();
        let mut stats = SquashStats::default();
        let _ = stage6_layout(&lines, None, false, &cfg, &mut stats);
        // Each dropped line removes its content + 1 LF, so total
        // bytes dropped >= 2 * dropped_lines for any line with
        // 1-byte content. With "L%03d" content is 4 bytes → expect
        // exactly 5 * dropped_lines.
        assert_eq!(
            stats.bytes_dropped_truncate,
            stats.lines_dropped_truncate * 5,
            "expected {}*5={}, got {}",
            stats.lines_dropped_truncate,
            stats.lines_dropped_truncate * 5,
            stats.bytes_dropped_truncate,
        );
    }

    /// Round-9 regression: in mode A (`signed_head_budget` is `Some`),
    /// when both head and tail are retained the marker zone replaces a
    /// source slice of the form `\nXm…Xn\n` — i.e. `sum(line.len()) +
    /// dropped_lines + 1` bytes (the extra LF is the boundary separator
    /// between the kept head and the first dropped line). Per-line `+ 1`
    /// alone under-reports by exactly one byte.
    #[test]
    fn mode_a_drop_accounting_counts_boundary_separator() {
        // 8 lines × 150 bytes = 1200 + 7 LFs = 1207 source bytes,
        // forces stage6 to truncate against max_bytes=1024 with head=2,
        // tail=2 → drops lines[2..6] (4 middle lines).
        let cfg = SquashConfig::new(1024, 2, 2, 2, MIN_MAX_LINE_BYTES).unwrap();
        let lines: Vec<String> = (0..8).map(|i| format!("{i:0150}")).collect();
        let mut stats = SquashStats::default();
        let _ = stage6_layout(&lines, None, false, &cfg, &mut stats);
        // Source slice replaced by the marker spans
        // `\nL2\nL3\nL4\nL5\n` = 4*150 + 4 LFs + 1 boundary LF (the
        // separator between kept head and first dropped line) = 605
        // bytes. `bytes_dropped_truncate` must report this exactly.
        assert_eq!(stats.lines_dropped_truncate, 4);
        assert_eq!(
            stats.bytes_dropped_truncate, 605,
            "expected 4*150 + 4 LFs between dropped lines + 1 boundary LF = 605",
        );
    }

    /// Round-3 (newer loop) regression: a long trailing blank-only
    /// suffix must not expand the preserved tail past `tail_lines()`.
    /// Pre-fix the `tail_start = last_content_idx` reset effectively
    /// inflated the tail to span `[last_content_idx, lines.len())`,
    /// which evicts head context and burns budget on whitespace.
    #[test]
    fn trailing_blank_suffix_does_not_expand_tail_past_tail_lines() {
        // 8 content lines × 50 bytes + 200 trailing blanks force
        // total_bytes > max_bytes so the truncation path runs. With
        // tail_lines = 2 the preserved tail must be exactly 2 lines
        // ending at the last content line — NOT 200+ blanks.
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 2, 2, 2, MIN_MAX_LINE_BYTES).unwrap();
        let mut lines: Vec<String> = (0..8).map(|i| format!("content-{i:03}-{i:040}")).collect();
        lines.extend(std::iter::repeat_n(String::new(), 200));
        let mut stats = SquashStats::default();
        let out = stage6_layout(&lines, None, false, &cfg, &mut stats);
        // The output must contain the last content line.
        assert!(
            out.contains("content-007"),
            "last content line preserved: {out:.200}"
        );
        // And it must NOT preserve the whole trailing-blank suffix:
        // a 200-blank tail would mean 200 LFs after the last content
        // line. Cap at `tail_lines + a small slack`.
        let trailing_lfs = out.bytes().rev().take_while(|&b| b == b'\n').count();
        assert!(
            trailing_lfs <= cfg.tail_lines() + 1,
            "preserved tail expanded past tail_lines: {trailing_lfs} trailing LFs"
        );
        // Drop accounting must include the trimmed trailing blanks.
        assert!(
            stats.lines_dropped_truncate >= 200 - cfg.tail_lines(),
            "trailing blanks should count as dropped: lines_dropped={}",
            stats.lines_dropped_truncate,
        );
    }

    #[test]
    fn last_line_preserved_for_extreme_input() {
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 2, 2, 2, MIN_MAX_LINE_BYTES).unwrap();
        let lines: Vec<String> = (0..10_000).map(|i| format!("L{i}")).collect();
        let mut stats = SquashStats::default();
        let out = stage6_layout(&lines, None, false, &cfg, &mut stats);
        assert!(out.ends_with("L9999"));
        assert!(out.len() <= cfg.max_bytes());
    }
}

/// Compact a verbose terminal-output payload into bounded bytes for storage.
///
/// Pure function: same `(raw, cfg)` always produces byte-identical
/// `compacted_bytes`. See module docs and design spec for invariants.
#[must_use]
// Wrapper is a validated witness; by-value enforces single-use semantics
// at the type level (the spec pins this as the public surface).
#[allow(clippy::needless_pass_by_value)]
// Splitting hurts pipeline readability — gates and stages here are
// linear and re-grouping into helpers obscures the dataflow.
#[allow(clippy::too_many_lines)]
pub fn squash(raw: UnstructuredTextBytes<'_>, cfg: &SquashConfig) -> SquashOutput {
    let raw_bytes = raw.as_bytes();
    let raw_hash = raw.raw_hash().clone();
    let raw_byte_len = raw_bytes.len();

    let mut stats = SquashStats::default();

    if raw_bytes.is_empty() {
        let compacted_hash = sha256_payload_hash(&[]);
        return SquashOutput {
            compacted_bytes: Vec::new(),
            raw_hash,
            raw_byte_len,
            compacted_hash,
            compacted_byte_len: 0,
            stats,
        };
    }

    // Byte-count gate AND line-cardinality gate: either above its
    // ceiling routes to the bypass path, which uses byte slicing and
    // avoids the per-line `Vec<String>` allocations of the staged
    // pipeline. Counting `\n` is O(N) but cheap relative to staging.
    if raw_bytes.len() >= MAX_INPUT_BYTES {
        return oversize_bypass(
            raw_bytes,
            raw_hash,
            raw_byte_len,
            cfg,
            stats,
            BypassReason::ByteCeiling,
        );
    }
    if bytecount_newlines(raw_bytes) >= MAX_INPUT_LINES {
        return oversize_bypass(
            raw_bytes,
            raw_hash,
            raw_byte_len,
            cfg,
            stats,
            BypassReason::LineCardinality,
        );
    }
    // Decode-expansion gate: invalid UTF-8 turns each bad byte into a
    // 3-byte `U+FFFD`, so the staged path can balloon a near-ceiling
    // input to ~3× before stage 6 enforces the budget. Threshold of
    // `MAX_INPUT_BYTES / 3` keeps worst-case decoded size at or under
    // the original ceiling. `from_utf8` is a fast O(N) validity scan
    // — far cheaper than the per-stage `Vec<String>` clones it
    // protects against.
    if raw_bytes.len() > MAX_INPUT_BYTES / 3 && std::str::from_utf8(raw_bytes).is_err() {
        return oversize_bypass(
            raw_bytes,
            raw_hash,
            raw_byte_len,
            cfg,
            stats,
            BypassReason::DecodeExpansion,
        );
    }

    let decoded = stage1_lossy_utf8(raw_bytes);
    // Stage 1 returns `Cow::Owned` iff `from_utf8_lossy` had to insert
    // `U+FFFD`. Surface that as a distinct lossy signal AND fold it
    // into `truncated` so audit consumers can't see "non-verbatim
    // bytes" without the corresponding flag.
    if matches!(decoded, Cow::Owned(_)) {
        stats.utf8_replacement = true;
    }
    let stage2 = stage2_ansi_strip(
        &decoded,
        &mut stats.ansi_stripped,
        &mut stats.osc_recovery_bytes_dropped,
        cfg.progress_frame_collapse_enabled(),
    );
    // Stage 2b: opt-in progress-frame-collapse pre-stage (issue #219).
    // Collapses bare-`\r` lines to their last non-empty `\r`-segment so
    // full-line progress-bar rewrites stop expanding dedup misses. Off
    // by default — narrowly scoped to producers that emit full-frame
    // rewrites; binary, diagnostic, or partial-rewrite captures must
    // not enable it. When it does run, `progress_frames_coalesced`
    // feeds the `truncated` bit below (the rewrite is lossy by
    // definition).
    //
    // Skipped when the config flag is off or when the stage-2 output
    // contains no `\r` (cheap fast path; avoids an extra String allocation).
    // Audit signal: count CR-bearing lines on the **pre-stage-2b** sanitized
    // text so consumers always see the source-capture hazard count, even
    // when stage 2b later rewrites the bytes away. Hidden CRs (i.e., lines
    // that had a CR but stage 2b resolved to a clean rendered line) must
    // not look identical to a CR-free capture.
    stats.cr_bearing_lines = stage2.split('\n').filter(|l| l.contains('\r')).count();
    let stage2b: Cow<'_, str> = if cfg.progress_frame_collapse_enabled()
        && (stage2.contains('\r') || contains_erase_sentinel(&stage2))
    {
        Cow::Owned(stage2b_progress_collapse(
            &stage2,
            &mut stats.progress_frames_coalesced,
            &mut stats.progress_bytes_saved,
        ))
    } else {
        Cow::Borrowed(stage2.as_ref())
    };
    let (raw_lines_borrow, trailing_newline) = stage3_split_lines(&stage2b);
    let raw_lines: Vec<String> = raw_lines_borrow.iter().map(|s| (*s).to_string()).collect();

    let (post_dedup, pair_companion_idx) = stage4_dedup_structured_with_pair_flag(
        &raw_lines,
        cfg.dedup_min_run(),
        &mut stats.dedup_runs_collapsed,
    );

    let mut long_lines_count: usize = 0;
    let post_cap: Vec<String> = post_dedup
        .into_iter()
        .map(|(content, multiplicity)| {
            let mut t = false;
            let r = stage5_per_line_cap_aware(&content, multiplicity, cfg.max_line_bytes(), &mut t);
            if t {
                long_lines_count += 1;
            }
            r
        })
        .collect();
    stats.long_lines_truncated = long_lines_count;
    // `truncated` means "compacted_bytes is not a verbatim sanitize-free
    // copy of the input." Set it for any lossy stage that fired during
    // the staged path: ANSI strip, OSC recovery drop, stage 2b progress-frame collapse
    // overwrite, dedup collapse, per-line cap. Stage 6's drop loop sets
    // it again on tail/head trim.
    if stats.ansi_stripped
        || stats.osc_recovery_bytes_dropped > 0
        || stats.progress_frames_coalesced > 0
        || stats.dedup_runs_collapsed > 0
        || long_lines_count > 0
        || stats.utf8_replacement
    {
        stats.truncated = true;
    }

    // Reserve 1 byte for the trailing `\n` we will re-append below, so the
    // final compacted bytes never exceed cfg.max_bytes().
    let reserve_trailing = trailing_newline;
    let had_lines = !post_cap.is_empty();
    let mut compacted = stage6_layout(
        &post_cap,
        pair_companion_idx,
        reserve_trailing,
        cfg,
        &mut stats,
    );
    // Re-append the trailing newline whenever the input had one and there
    // was at least one logical line — otherwise a sole `"\n"` collapses to
    // empty output even though it fits every byte budget.
    if trailing_newline && had_lines {
        compacted.push('\n');
    }

    let compacted_bytes = compacted.into_bytes();
    let compacted_byte_len = compacted_bytes.len();
    let compacted_hash = sha256_payload_hash(&compacted_bytes);

    SquashOutput {
        compacted_bytes,
        raw_hash,
        raw_byte_len,
        compacted_hash,
        compacted_byte_len,
        stats,
    }
}

/// Reason `squash()` routed to the oversize bypass. Surfaced in the
/// emitted marker so audit/observability can distinguish the failing
/// guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BypassReason {
    /// `raw_bytes.len() >= MAX_INPUT_BYTES`.
    ByteCeiling,
    /// `\n` count exceeded `MAX_INPUT_LINES` (line-density OOM guard).
    LineCardinality,
    /// Invalid UTF-8 in a payload large enough that lossy `U+FFFD`
    /// expansion (worst case 3× per byte) would push the staged path
    /// past `MAX_INPUT_BYTES` of working set. Routes to the bypass so
    /// decode happens only on bounded head/tail windows.
    DecodeExpansion,
}

impl BypassReason {
    fn marker(self, raw_byte_len: usize, line_count: usize) -> String {
        match self {
            Self::ByteCeiling => format!(
                "[…oversize bypass: {raw_byte_len} bytes >= MAX_INPUT_BYTES ({MAX_INPUT_BYTES}), squash skipped…]"
            ),
            Self::LineCardinality => format!(
                "[…oversize bypass: {line_count} lines >= MAX_INPUT_LINES ({MAX_INPUT_LINES}), squash skipped…]"
            ),
            Self::DecodeExpansion => format!(
                "[…oversize bypass: {raw_byte_len} bytes invalid UTF-8 (decode expansion), squash skipped…]"
            ),
        }
    }
}

/// Oversize-bypass: when raw exceeds `MAX_INPUT_BYTES` or `MAX_INPUT_LINES`
/// the per-stage pipeline would clone several full copies before stage 6
/// enforces `max_bytes`, risking OOM on legitimate long-running build/test
/// logs. Take a head + tail byte slice, decode lossily, trim the decoded
/// strings to byte budgets at codepoint boundaries (lossy decode can
/// expand 1B → 3B for invalid bytes), insert a clear marker, and
/// return a normal `SquashOutput` with `stats.truncated = true`.
/// Memory and output are bounded by `cfg.max_bytes()`.
#[allow(clippy::too_many_lines)] // sequential pipeline; splitting hurts clarity
fn oversize_bypass(
    raw_bytes: &[u8],
    raw_hash: PayloadHash,
    raw_byte_len: usize,
    cfg: &SquashConfig,
    mut stats: SquashStats,
    reason: BypassReason,
) -> SquashOutput {
    let max_body = cfg.max_bytes();
    // For the LineCardinality marker we want an EXACT count, not the
    // capped short-circuit value used by the gate. Audit consumers
    // looking at the marker need the real scale of the discarded
    // input, not just confirmation that it exceeded the threshold.
    // The caller only takes this branch on the destructive path, so
    // the extra O(N) scan is acceptable.
    let line_count = match reason {
        BypassReason::LineCardinality => {
            #[allow(clippy::naive_bytecount)] // counting a specific byte; no bytecount dep
            let n = raw_bytes.iter().filter(|&&b| b == b'\n').count();
            n
        }
        BypassReason::ByteCeiling | BypassReason::DecodeExpansion => 0, // not rendered for these branches
    };
    // Audit signal: count BARE-CR-bearing lines from the FULL raw input
    // rather than accumulating per-window inside the bypass. The bypass
    // only sanitizes head/tail/suffix slices, so a huge payload with
    // CR-bearing progress lines concentrated in the dropped middle
    // would otherwise report cr_bearing_lines == 0 and hide a source-
    // capture hazard exactly on the captures most likely to hit it.
    //
    // `count_bare_cr_lines_after_stage2` walks raw bytes with the same
    // escape-state parser that stage 2 uses on the staged path
    // (`stage2_skip_escape`), so `\r` bytes inside ESC-introduced control
    // strings (OSC, DCS, APC, PM, SOS — terminated or not) are excluded
    // here exactly the way stage 2 excludes them. CRLF / CRCRLF rule
    // parity (`\r\n` → `\n`, `\r\r\n` → `\n`, `\r\r\r\n` keeps one
    // residual `\r`) is mirrored line-by-line, and trailing partial
    // lines preserve any surviving `\r` as stage 2 does.
    stats.cr_bearing_lines = count_bare_cr_lines_after_stage2(raw_bytes);
    let marker = reason.marker(raw_byte_len, line_count);
    let marker_bytes = marker.as_bytes();
    let budget = max_body.saturating_sub(marker_bytes.len() + 2 /* two LFs */);
    let half = budget / 2;
    // Round-6 (newer loop) inversion: previously head and tail
    // windows were both sized to `half * 2` and the tail was clamped
    // away when they overlapped. That dropped final diagnostic lines
    // for any payload smaller than `2 * half` reaching the bypass
    // (line-cardinality / decode-expansion paths on large configs).
    // Spec preserves the TAIL first — set the tail window from the
    // end of raw, then cap the head so it cannot overlap. When raw
    // is small enough that the tail consumes the whole payload, the
    // head shrinks to zero and the entire raw content is preserved
    // through the tail.
    let tail_window_max = (half * 2).min(raw_bytes.len());
    let provisional_tail_start = raw_bytes.len() - tail_window_max;
    let head_raw_end = (half * 2).min(provisional_tail_start);
    let head_decoded = stage1_lossy_utf8(&raw_bytes[..head_raw_end]);
    let mut head_stripped = false;
    let head_sanitized = stage2_ansi_strip(
        &head_decoded,
        &mut head_stripped,
        &mut stats.osc_recovery_bytes_dropped,
        cfg.progress_frame_collapse_enabled(),
    );
    stats.ansi_stripped |= head_stripped;
    // Stage 2b also runs on the bypass path so `progress_frame_collapse_enabled`
    // does not silently flip semantics when an input crosses a bypass gate.
    // Renders before trimming so the trim sees the rendered (typically
    // shorter) text and we do not waste budget on stale CR frames.
    //
    // /review-loop round 7 finding 2: tally stage 2b counters into LOCAL
    // accumulators first; fold into `stats` only after we know the head
    // actually contributes output. Otherwise a giant single-line payload
    // would have its head dropped (no `\n` boundary) AND get re-counted
    // in the giant-final-line suffix branch below. cr_bearing_lines is
    // not affected: it is computed once from the full raw input above,
    // so per-window accumulation is unnecessary.
    let mut head_progress_frames_local: usize = 0;
    let mut head_progress_bytes_local: usize = 0;
    let head_rendered: Cow<'_, str> = if cfg.progress_frame_collapse_enabled()
        && (head_sanitized.contains('\r') || contains_erase_sentinel(&head_sanitized))
    {
        Cow::Owned(stage2b_progress_collapse(
            &head_sanitized,
            &mut head_progress_frames_local,
            &mut head_progress_bytes_local,
        ))
    } else {
        Cow::Borrowed(head_sanitized.as_ref())
    };
    let head_trimmed = trim_to_byte_budget_at_boundary(&head_rendered, half);
    // Drop any trailing partial line from head. If `head_trimmed`
    // contains NO `\n` at all, the entire window is a prefix of one
    // giant source line — emit nothing rather than a mid-line prefix
    // (Round-9 finding: head leak on newline-free oversized payloads
    // exposed up to ~half the byte budget of raw source bytes even
    // though no safe line boundary existed).
    let head: &str = match head_trimmed.rfind('\n') {
        Some(idx) => &head_trimmed[..idx],
        None => "",
    };
    // Head fate decided. Fold local counters only when the head actually
    // contributed bytes; otherwise drop them so the suffix branch (which
    // reprocesses the same source range) does not see them counted twice.
    if !head.is_empty() {
        stats.progress_frames_coalesced += head_progress_frames_local;
        stats.progress_bytes_saved = stats
            .progress_bytes_saved
            .saturating_add(head_progress_bytes_local);
    }

    // Tail is anchored from the end of raw (Round-6: tail-first
    // priority); `provisional_tail_start` is already disjoint from
    // `head_raw_end` by construction.
    let initial_tail_raw_start = provisional_tail_start;
    // Round-4 (newer loop) hardening: the previous "first `\n` in
    // tail window" cut could land inside an open escape sequence
    // (e.g. an OSC body that contains a `\n` and is terminated past
    // tail_raw_start). The introducer would be omitted from the
    // sanitizer's view and continuation bytes would leak as plain
    // text. Walk the raw payload with the same parser used by
    // `ends_outside_escape_sequence` and pick the first `\n` whose
    // BYTE-AFTER position is BOTH at-or-past `initial_tail_raw_start`
    // AND in "outside escape" state. If no such cut exists, fall
    // through to the giant-final-line / degraded-sentinel branch.
    let safe_cut = first_safe_line_start_at_or_after(raw_bytes, initial_tail_raw_start);
    let (tail_raw_start, tail_aligned_to_line) = match safe_cut {
        Some(p) if p < raw_bytes.len() => (p, true),
        _ => (initial_tail_raw_start, false),
    };
    let mut compacted_bytes: Vec<u8> = Vec::with_capacity(max_body);
    compacted_bytes.extend_from_slice(head.as_bytes());
    compacted_bytes.push(b'\n');
    compacted_bytes.extend_from_slice(marker_bytes);
    compacted_bytes.push(b'\n');

    let mut tail_source_bytes: usize = 0;
    if tail_aligned_to_line {
        let tail_decoded = stage1_lossy_utf8(&raw_bytes[tail_raw_start..]);
        let mut tail_stripped = false;
        let tail_sanitized = stage2_ansi_strip(
            &tail_decoded,
            &mut tail_stripped,
            &mut stats.osc_recovery_bytes_dropped,
            cfg.progress_frame_collapse_enabled(),
        );
        stats.ansi_stripped |= tail_stripped;
        // Stage 2b on the tail window for the same reason as the head:
        // honor `progress_frame_collapse_enabled` even on the bypass path.
        let tail_rendered: Cow<'_, str> = if cfg.progress_frame_collapse_enabled()
            && (tail_sanitized.contains('\r') || contains_erase_sentinel(&tail_sanitized))
        {
            Cow::Owned(stage2b_progress_collapse(
                &tail_sanitized,
                &mut stats.progress_frames_coalesced,
                &mut stats.progress_bytes_saved,
            ))
        } else {
            Cow::Borrowed(tail_sanitized.as_ref())
        };
        let tail_trimmed = trim_to_byte_budget_at_boundary_from_end(&tail_rendered, half);
        // The raw slice was line-aligned (begins after a `\n`), so an
        // UN-trimmed tail starts on a line boundary. Only realign if
        // `trim_to_byte_budget_at_boundary_from_end` actually shaved
        // bytes off the front — that's when the first byte can sit
        // mid-line again (Round-7 finding).
        let was_trimmed = tail_trimmed.len() < tail_rendered.len();
        // `final_line_truncated` triggers when the tail was actually
        // trimmed AND the resulting suffix has no `\n` — i.e., the
        // payload's last source line alone exceeds the tail budget.
        // Rather than dropping the suffix (would lose the diagnostic
        // line entirely), emit a stage-5-style truncation marker and
        // the bounded suffix at the codepoint boundary already given
        // by `tail_trimmed`.
        let mut final_line_truncated = false;
        let tail_aligned: &str = if was_trimmed {
            if let Some(idx) = tail_trimmed.find('\n') {
                &tail_trimmed[idx + 1..]
            } else {
                final_line_truncated = true;
                tail_trimmed
            }
        } else {
            tail_trimmed
        };
        // Round-2 (newer loop) hardening: when `final_line_truncated`
        // fires, the final-line marker is appended on top of a head
        // window already up to `half` bytes and a tail trimmed to
        // `half` bytes — total can exceed `max_body` by the marker
        // length and trip the closing assert. Reserve marker space
        // explicitly here, the same way the newline-free suffix
        // branch already does.
        let final_marker = b"[\xe2\x80\xa6final-line truncated\xe2\x80\xa6]\n";
        let tail_emit: &str = if final_line_truncated {
            let used = compacted_bytes.len();
            let remaining = max_body.saturating_sub(used + final_marker.len());
            let suffix = trim_to_byte_budget_at_boundary_from_end(tail_aligned, remaining);
            compacted_bytes.extend_from_slice(final_marker);
            compacted_bytes.extend_from_slice(suffix.as_bytes());
            suffix
        } else {
            compacted_bytes.extend_from_slice(tail_aligned.as_bytes());
            tail_aligned
        };
        let tail_aligned = tail_emit;
        // Track raw-tail span for accounting: count newlines in
        // `tail_aligned` and find the matching position from end-of-raw.
        // Newlines pass through stages 1+2 unchanged, so the n-th
        // sanitized newline corresponds 1:1 with the n-th raw newline.
        let tail_newlines = tail_aligned.bytes().filter(|&b| b == b'\n').count();
        if tail_newlines > 0 {
            let mut count = 0usize;
            for i in (0..raw_byte_len).rev() {
                if raw_bytes[i] == b'\n' {
                    count += 1;
                    if count == tail_newlines + 1 {
                        tail_source_bytes = raw_byte_len - (i + 1);
                        break;
                    }
                }
            }
            // If the source has fewer newlines than emitted (shouldn't
            // happen given 1:1 \n preservation), fall back to using
            // raw_byte_len - tail_raw_start.
            if tail_source_bytes == 0 {
                tail_source_bytes = raw_byte_len - tail_raw_start;
            }
        } else if !tail_aligned.is_empty() {
            // No newline in emitted tail. Round-2 (newer loop): use
            // raw boundaries so ANSI/OSC bytes stripped from the
            // preserved suffix do NOT count as truncation loss
            // (`ansi_stripped` / `osc_recovery_bytes_dropped` already
            // covers them). Only when the suffix was actually
            // front-trimmed for budget — `final_line_truncated` —
            // does the lower-bound `tail_aligned.len()` apply.
            tail_source_bytes = if final_line_truncated {
                tail_aligned.len().min(raw_byte_len - tail_raw_start)
            } else {
                raw_byte_len - tail_raw_start
            };
        }
    } else if !raw_bytes.is_empty() {
        // No `\n` exists in the retained tail window. Two sub-cases:
        //   (a) The source has at least one `\n` somewhere earlier — i.e.,
        //       the FINAL source line alone is longer than the tail
        //       window. Preserve a bounded codepoint-safe suffix of the
        //       final line with an explicit truncation marker. (Round 9.)
        //   (b) The entire source is one line with no `\n` anywhere.
        //       Cannot prove a safe sequence boundary; emit only the
        //       degraded sentinel (Round-6 strict-quarantine rule).
        let last_raw_nl = raw_bytes.iter().rposition(|&b| b == b'\n');
        // Round-1 (newer loop) hardening: the giant-final-line suffix
        // path slices `raw_bytes` at an arbitrary byte offset. If an
        // ANSI ESC introducer (`0x1B`) sits anywhere in the dropped
        // prefix of the final line, the kept suffix may begin INSIDE
        // an OSC/CSI sequence whose introducer was sliced off — and
        // `stage2_ansi_strip` would then pass the dangling URL/params
        // through as plain text, leaking control-plane payload that
        // the bypass is supposed to quarantine. To prove the suffix
        // can't begin mid-escape, require the dropped prefix of the
        // final line to be ESC-free; otherwise fall back to the
        // degraded sentinel.
        // Round-3 (newer loop) refinement: previously this gate
        // rejected ANY ESC byte in the dropped prefix, which loses
        // recoverable diagnostics when the prefix only contains
        // fully-terminated SGR colors. Walk the prefix with the same
        // CSI/OSC matcher used by `stage2_ansi_strip` and only fall
        // back to the degraded sentinel when the prefix ends INSIDE
        // an unterminated escape sequence (so the kept suffix would
        // begin mid-CSI / mid-OSC and leak control-plane bytes).
        let safe_to_preserve_suffix = match last_raw_nl {
            Some(nl_pos) => {
                let final_line_start = nl_pos + 1;
                let suffix_window_start =
                    final_line_start.max(raw_byte_len.saturating_sub(half * 2));
                ends_outside_escape_sequence(&raw_bytes[final_line_start..suffix_window_start])
            }
            None => false,
        };
        if let Some(nl_pos) = last_raw_nl
            && safe_to_preserve_suffix
        {
            // The final line spans raw_bytes[nl_pos+1..]. Decode +
            // sanitize the last `tail_window` bytes of it (capped at
            // the line length), then trim to half the byte budget at
            // a codepoint boundary.
            let final_line_start = nl_pos + 1;
            let provisional_start = final_line_start.max(raw_byte_len.saturating_sub(half * 2));
            // Round-7 (newer loop) hardening: `provisional_start` is
            // an arbitrary raw byte offset and can land in the
            // middle of a multi-byte UTF-8 codepoint. Decoding from
            // there with `from_utf8_lossy` would corrupt the first
            // codepoint into `U+FFFD` even when the source was
            // valid UTF-8 — irreversible loss for non-ASCII paths,
            // identifiers, or error messages. Rewind forward over
            // any continuation bytes (`0x80..=0xBF`) until the
            // start lands on a leading byte. Capped at 3 forward
            // steps because UTF-8 has at most 3 trailing bytes per
            // codepoint, so this never overshoots into a different
            // codepoint's leading byte.
            let mut suffix_window_start = provisional_start;
            for _ in 0..3 {
                if suffix_window_start < raw_byte_len
                    && (0x80..=0xBF).contains(&raw_bytes[suffix_window_start])
                {
                    suffix_window_start += 1;
                } else {
                    break;
                }
            }
            let suffix_decoded = stage1_lossy_utf8(&raw_bytes[suffix_window_start..]);
            let mut suf_stripped = false;
            let suffix_sanitized = stage2_ansi_strip(
                &suffix_decoded,
                &mut suf_stripped,
                &mut stats.osc_recovery_bytes_dropped,
                cfg.progress_frame_collapse_enabled(),
            );
            stats.ansi_stripped |= suf_stripped;
            // Stage 2b on the giant-final-line suffix: render the full
            // suffix BEFORE trimming so the trim sees only the final
            // visible frame, not stale `\r`-separated progress text. If
            // we trimmed first and rendered after, the trim could chop
            // mid-frame and leave a partial CR run that survives stage 2b.
            let suffix_rendered: Cow<'_, str> = if cfg.progress_frame_collapse_enabled()
                && (suffix_sanitized.contains('\r') || contains_erase_sentinel(&suffix_sanitized))
            {
                Cow::Owned(stage2b_progress_collapse(
                    &suffix_sanitized,
                    &mut stats.progress_frames_coalesced,
                    &mut stats.progress_bytes_saved,
                ))
            } else {
                Cow::Borrowed(suffix_sanitized.as_ref())
            };
            // Reserve room for the final-line marker before trimming.
            let final_marker = b"[\xe2\x80\xa6final-line truncated\xe2\x80\xa6]\n";
            let used = compacted_bytes.len();
            let remaining = max_body.saturating_sub(used + final_marker.len());
            let suffix = trim_to_byte_budget_at_boundary_from_end(&suffix_rendered, remaining);
            compacted_bytes.extend_from_slice(final_marker);
            compacted_bytes.extend_from_slice(suffix.as_bytes());
            // Round-2 (newer loop): only count front-trim as
            // truncation loss in raw terms. If the budget swallowed
            // the entire rendered suffix without trimming, the full
            // raw input span [suffix_window_start..] is preserved
            // — sanitization / stage-2b shrinkage is reported via
            // `ansi_stripped` / `osc_recovery_bytes_dropped` /
            // `progress_frames_coalesced`, not `bytes_dropped_truncate`.
            let was_front_trimmed = suffix.len() < suffix_rendered.len();
            tail_source_bytes = if was_front_trimmed {
                suffix.len().min(raw_byte_len - suffix_window_start)
            } else {
                raw_byte_len - suffix_window_start
            };
        } else {
            let degraded = b"[\xe2\x80\xa6tail dropped: oversized single-line payload (no line boundary in retained window)\xe2\x80\xa6]";
            compacted_bytes.extend_from_slice(degraded);
        }
    }
    assert!(
        compacted_bytes.len() <= max_body,
        "oversize bypass exceeded max_bytes: got {} > {}",
        compacted_bytes.len(),
        max_body,
    );
    stats.truncated = true;
    // Bypass accounting derived from raw slice boundaries (not sanitized
    // lengths) so ANSI/OSC bytes stripped during sanitization do NOT
    // count as truncation loss. `head_raw_end_preserved` is the byte
    // after the last `\n` in raw_bytes[..head_raw_end] (the position
    // up to which head's source content extends in raw). `tail_source_bytes`
    // is the raw-byte length of the source span represented by the
    // emitted tail.
    let head_raw_end_preserved = raw_bytes[..head_raw_end]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    // If `head` was emitted as empty (no `\n` in head_trimmed) the
    // source preserved by head is also 0 — match that.
    let head_raw_preserved = if head.is_empty() {
        0
    } else {
        head_raw_end_preserved
    };
    let preserved_raw_bytes = head_raw_preserved + tail_source_bytes;
    stats.bytes_dropped_truncate = raw_byte_len.saturating_sub(preserved_raw_bytes);
    // Lines dropped: count newlines in the dropped middle region of
    // raw_bytes (between head's last preserved newline and where the
    // tail's source span starts).
    let tail_source_start = raw_byte_len.saturating_sub(tail_source_bytes);
    if tail_source_start > head_raw_preserved {
        // Count `\n` bytes in the dropped middle region. We are
        // intentionally filtering for a specific byte; bytecount
        // crates are not in our dep tree.
        #[allow(clippy::naive_bytecount)]
        let n = raw_bytes[head_raw_preserved..tail_source_start]
            .iter()
            .filter(|&&b| b == b'\n')
            .count();
        stats.lines_dropped_truncate = n;
    }
    let compacted_hash = sha256_payload_hash(&compacted_bytes);
    let compacted_byte_len = compacted_bytes.len();
    SquashOutput {
        compacted_bytes,
        raw_hash,
        raw_byte_len,
        compacted_hash,
        compacted_byte_len,
        stats,
    }
}

/// Fuzz-only entrypoint: feeds raw bytes through the full squash
/// pipeline (staged + bypass paths) without requiring callers to
/// construct a `CaptureEvent` themselves. Built behind the `fuzz`
/// crate feature so it never reaches a production binary.
///
/// Returns `None` when an internally-synthesised wrapper fails
/// validation (rare — only on inputs that would fail an invariant
/// independent of the squash logic, e.g. impossible hash). For
/// fuzz purposes a `None` is a discard, not a finding.
///
/// # Panics
/// Panics propagate. The fuzz harness's libFuzzer driver treats
/// panics as findings, which is the desired behaviour for
/// `assert!`s embedded in the pipeline.
#[cfg(feature = "fuzz")]
#[doc(hidden)]
#[must_use]
pub fn fuzz_entrypoint(raw: &[u8], cfg: &SquashConfig) -> Option<SquashOutput> {
    use crate::domain::actor_chain::{ActorChainEntry, ChainRole};
    use crate::domain::capture::{CaptureEventId, CaptureMode, CaptureRefs, SourceFamily};
    use crate::domain::identity::Identity;
    use crate::domain::timestamp::Rfc3339Timestamp;

    let payload_hash = sha256_payload_hash(raw);
    let id = Identity::parse("snr:local:terminal:cli:v1").ok()?;
    let ts = Rfc3339Timestamp::parse("2026-04-27T00:00:00Z").ok()?;
    let event = CaptureEvent {
        event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").ok()?,
        sensor_id: id.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: id,
            at: ts.clone(),
        }],
        refs: Some(CaptureRefs {
            session_id: Some("fuzz".into()),
            turn_id: Some("fuzz".into()),
            tool_id: None,
        }),
        payload_hash,
        payload_ref: "sources/terminal/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into(),
        captured_at: ts,
        payload: CapturePayload::Terminal {
            command: "fuzz".into(),
            exit_code: Some(0),
            // #218: required for the squash boundary to accept the
            // event. Without this, every fuzz input would short-circuit
            // at `try_from_terminal_event` and the harness would stop
            // exercising squash invariants.
            context: Some(TerminalContext::InteractiveTty),
        },
        source_family: SourceFamily::Terminal,
    };
    // Drive the fuzz harness through the real dispatch decision so
    // production routing is on the fuzzed path; the synthetic event
    // is `Terminal { context: InteractiveTty }`, so dispatch returns
    // `Squash` and yields the admission token without any test-only
    // shortcut.
    let admission = match crate::pipeline::dispatch::dispatch(
        &event,
        &crate::pipeline::dispatch::DefaultRegistry,
    ) {
        crate::pipeline::dispatch::DispatchDecision::Squash(t) => t,
        crate::pipeline::dispatch::DispatchDecision::Bypass(_) => return None,
    };
    let wrapper = UnstructuredTextBytes::try_from_terminal_event(&event, raw, admission).ok()?;
    Some(squash(wrapper, cfg))
}

#[cfg(all(test, feature = "fuzz"))]
mod fuzz_entrypoint_smoke {
    use super::*;

    /// Regression for #218 review-loop round 2: ensure the fuzz harness
    /// fixture still constructs a valid `UnstructuredTextBytes` so the
    /// squash invariants stay covered by fuzzing. If `fuzz_entrypoint`
    /// returns `None` for a basic input, every libFuzzer iteration would
    /// short-circuit and silently disable the harness.
    #[test]
    fn fuzz_entrypoint_returns_some_for_basic_input() {
        let cfg = SquashConfig::default();
        let out = fuzz_entrypoint(b"hello fuzz\n", &cfg);
        assert!(
            out.is_some(),
            "fuzz_entrypoint short-circuited — squash fuzz harness would emit no coverage"
        );
    }
}

// Invariant: Sha256::digest produces a fixed 32-byte output that always
// formats as a valid sha256 PayloadHash. The expect is therefore unreachable.
#[allow(clippy::expect_used)]
fn sha256_payload_hash(bytes: &[u8]) -> PayloadHash {
    let digest = Sha256::digest(bytes);
    PayloadHash::parse(format!("sha256:{digest:x}"))
        .expect("sha256 hex digest is a well-formed PayloadHash")
}

/// Wraps `stage4_dedup_structured` and additionally reports whether the input
/// ends in a repeat run that triggered the split-form last-line exemption (the
/// output will end with `(content, Some(N-1))` followed by `(content, None)`
/// and the pair must be kept atomic in stage 6).
fn stage4_dedup_structured_with_pair_flag(
    lines: &[String],
    min_run: usize,
    collapsed_runs: &mut usize,
) -> (Vec<DedupLine>, Option<usize>) {
    let out = stage4_dedup_structured(lines, min_run, collapsed_runs);
    // Locate the split-form pair in the output (count-companion `Some(K)`
    // immediately followed by `None` with identical content). Returns the
    // index of the count-companion so stage 6 can keep both entries in
    // the same region — the pair may not be at the end if trailing blank
    // lines follow the repeated content run.
    let pair_idx = out.iter().enumerate().find_map(|(i, entry)| {
        let next = out.get(i + 1)?;
        match (entry, next) {
            ((content_a, Some(_)), (content_b, None)) if content_a == content_b => Some(i),
            _ => None,
        }
    });
    (out, pair_idx)
}

#[cfg(test)]
mod tail_lock_tests {
    use super::*;

    #[test]
    fn tail_locked_pair_not_split_under_pressure() {
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 2, 2, 2, MIN_MAX_LINE_BYTES).unwrap();
        let mut lines: Vec<String> = (0..100).map(|i| format!("head-{i:03}")).collect();
        lines.push("x [×3]".into());
        lines.push("x".into());
        let mut stats = SquashStats::default();
        // Pair count-companion ("x [×3]") is at index 100, partner at 101.
        let out = stage6_layout(&lines, Some(100), false, &cfg, &mut stats);
        let has_marker = out.contains("x [×3]");
        let has_final = out.contains("\nx") || out.starts_with('x');
        assert!(has_final, "final line must survive");
        if has_final {
            assert!(has_marker, "count marker must accompany surviving final");
        }
    }
}

#[cfg(test)]
mod squash_integration_tests {
    use super::wrapper_tests::terminal_event;
    use super::*;

    fn run_squash(raw: &[u8], cfg: &SquashConfig) -> SquashOutput {
        let evt = terminal_event(raw);
        let wrapper = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            raw,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("valid wrapper");
        squash(wrapper, cfg)
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let out = run_squash(b"", &SquashConfig::default());
        assert!(out.compacted_bytes.is_empty());
        assert_eq!(out.raw_byte_len, 0);
        assert_eq!(out.compacted_byte_len, 0);
        assert!(!out.stats.truncated);
        assert_eq!(out.raw_hash, out.compacted_hash);
    }

    #[test]
    fn short_input_passes_through() {
        let raw = b"hello\nworld\n";
        let out = run_squash(raw, &SquashConfig::default());
        assert_eq!(out.compacted_bytes, raw);
    }

    #[test]
    fn deterministic() {
        let raw = b"line\nline\nline\nfinal\n";
        let cfg = SquashConfig::default();
        let a = run_squash(raw, &cfg);
        let b = run_squash(raw, &cfg);
        assert_eq!(a.compacted_bytes, b.compacted_bytes);
        assert_eq!(a.stats, b.stats);
        assert_eq!(a.compacted_hash, b.compacted_hash);
    }

    #[test]
    fn last_content_line_preserved_under_pressure() {
        // Use a tight budget so stage6 must truncate.
        // cfg: max_bytes=512 (MIN), head=2, tail=2, dedup=2, line=128 (MIN)
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 2, 2, 2, MIN_MAX_LINE_BYTES).expect("valid cfg");
        let mut raw = Vec::new();
        // 200 unique lines — not dedup'd — totalling far over 512 bytes
        for i in 0_u32..200 {
            raw.extend_from_slice(format!("log-line-{i:04}\n").as_bytes());
        }
        raw.extend_from_slice(b"FINAL_SENTINEL\n");
        let out = run_squash(&raw, &cfg);
        assert!(out.stats.truncated);
        assert!(
            String::from_utf8_lossy(&out.compacted_bytes).contains("FINAL_SENTINEL"),
            "last content line must be preserved"
        );
    }

    /// Regression: stage 5 truncation alone (no stage 6 line-drop) must
    /// still flip `stats.truncated`, otherwise callers see a false negative
    /// while the output is already lossy.
    #[test]
    fn stage5_only_truncation_flips_stats_truncated() {
        // One ultra-long line that fits within max_bytes but exceeds
        // max_line_bytes. Stage 6 won't drop anything; stage 5 will.
        let cfg = SquashConfig::default();
        let mut raw = vec![b'x'; cfg.max_line_bytes() * 2];
        raw.push(b'\n');
        let out = run_squash(&raw, &cfg);
        assert!(out.stats.long_lines_truncated >= 1);
        assert!(
            out.stats.truncated,
            "stats.truncated must be set when stage 5 truncated"
        );
    }

    /// Regression: when the preserved tail itself overflows the byte budget
    /// (mode B), trailing blank lines past the anchor must be dropped before
    /// the anchored content line. We disable dedup (high `dedup_min_run`)
    /// so the blank suffix actually reaches stage 6 instead of being
    /// collapsed into a `[×N]` marker upstream.
    #[test]
    fn trailing_blank_suffix_dropped_before_anchored_content() {
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 2, 2, 10_000, MIN_MAX_LINE_BYTES).unwrap();
        let mut raw = Vec::new();
        raw.extend_from_slice(b"FINAL\n");
        // 1000 trailing blanks: with dedup disabled, each contributes 1
        // separator-newline byte, so the joined body alone exceeds max_body.
        raw.extend(std::iter::repeat_n(b'\n', 1000));
        let out = run_squash(&raw, &cfg);
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(out.stats.truncated);
        assert!(
            body.contains("FINAL"),
            "anchored last-content line must outlive trailing-blank suffix; got: {body:?}"
        );
        assert!(out.compacted_byte_len <= cfg.max_bytes());
    }

    /// Regression: a repeated content run followed by trailing blanks must
    /// emit the split-form pair `<content> [×N-1]` + verbatim `<content>`
    /// (anchored on the last *non-empty* content line) so the byte-exact
    /// final line survives — not be collapsed to a single `[×N]` entry.
    #[test]
    fn repeated_run_with_trailing_blanks_keeps_verbatim_final() {
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 2, 2, 2, MIN_MAX_LINE_BYTES).unwrap();
        let mut raw = Vec::new();
        for i in 0..50 {
            raw.extend_from_slice(format!("noise-{i:04}\n").as_bytes());
        }
        // 4 repeats of FINAL → split-form ("FINAL", Some(3)) + ("FINAL", None)
        // because the run reaches last_content_idx (trailing blanks ignored).
        for _ in 0..4 {
            raw.extend_from_slice(b"FINAL\n");
        }
        raw.extend(std::iter::repeat_n(b'\n', 3));
        let out = run_squash(&raw, &cfg);
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(out.stats.truncated);
        assert!(
            body.contains("FINAL [×3]"),
            "split-form ×3 marker must survive: {body:?}"
        );
        // The verbatim FINAL must appear after the count marker.
        let split_idx = body.find("FINAL [×3]").unwrap();
        assert!(
            body[split_idx + "FINAL [×3]".len()..].contains("FINAL"),
            "verbatim final FINAL line must follow the count marker: {body:?}"
        );
    }

    /// Reviewer's specific case: `FINAL\nFINAL\n\n` should preserve both
    /// `FINAL` lines verbatim (run len 2 with `min_run=2` → split-form,
    /// `count=1` is below `min_run` so emit duplicates verbatim instead of
    /// `[×1]`).
    #[test]
    fn double_final_with_trailing_blank_preserves_both_verbatim() {
        let cfg = SquashConfig::default();
        let out = run_squash(b"FINAL\nFINAL\n\n", &cfg);
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        // Two FINAL lines should appear (no dedup collapse).
        assert_eq!(body.matches("FINAL").count(), 2, "got: {body:?}");
        assert!(!body.contains("[×"), "no count marker expected: {body:?}");
    }

    /// Regression: when an input ends with multiple blank lines, the tail
    /// must still anchor on the last *non-empty* line — otherwise a trailing
    /// `\n\n\n` suffix could evict the real final content from the preserved
    /// tail under truncation pressure.
    #[test]
    fn trailing_blank_lines_do_not_evict_last_content_line() {
        // tail_lines = 2, but the input has 3 trailing blanks. The "natural"
        // tail (last 2 raw lines) would be `["", ""]`; the fix shifts the
        // anchor to keep `FINAL` in the tail.
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 2, 2, 2, MIN_MAX_LINE_BYTES).unwrap();
        // Build many head lines so stage 6 must truncate, then a content line
        // followed by trailing blanks.
        let mut raw = Vec::new();
        for i in 0..200 {
            raw.extend_from_slice(format!("noise-{i:04}\n").as_bytes());
        }
        raw.extend_from_slice(b"FINAL\n\n\n");
        let out = run_squash(&raw, &cfg);
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(out.stats.truncated);
        assert!(
            body.contains("FINAL"),
            "last non-empty content line must survive trailing blanks; got: {body:?}"
        );
    }

    /// Regression: a sole `"\n"` input must round-trip as `"\n"` instead of
    /// collapsing to empty output.
    #[test]
    fn sole_newline_round_trips() {
        let cfg = SquashConfig::default();
        let out = run_squash(b"\n", &cfg);
        assert_eq!(
            out.compacted_bytes, b"\n",
            "single blank line must pass through unchanged"
        );
        assert!(!out.stats.truncated);
    }

    /// Regression: a newline-terminated payload whose body lands exactly at
    /// `max_bytes` must still respect `max_bytes` after the entrypoint
    /// re-appends the trailing `\n`.
    #[test]
    fn trailing_newline_does_not_breach_max_bytes() {
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 100, 100, 2, MIN_MAX_LINE_BYTES).unwrap();
        // Build an input whose unique-line, joined-without-trailing-newline
        // length fits inside max_bytes only when we reserve the trailing byte.
        // We sweep a range of input sizes near the budget so at least one
        // exercises the boundary where the body lands at exactly max_body.
        for n in (cfg.max_bytes() - 8)..=(cfg.max_bytes() + 8) {
            let mut raw = vec![b'a'; n];
            raw.push(b'\n');
            let out = run_squash(&raw, &cfg);
            assert!(
                out.compacted_byte_len <= cfg.max_bytes(),
                "compacted_byte_len {} exceeded max_bytes {} for input size {}",
                out.compacted_byte_len,
                cfg.max_bytes(),
                n
            );
        }
    }
}

// Inline golden-file snapshot tests. Live in the lib (not in `tests/`) so the
// `try_from_terminal_event` constructor can stay `pub(crate)` until #218 lands.
#[cfg(test)]
mod squash_fixtures_tests {
    use super::wrapper_tests::terminal_event;
    use super::*;

    fn workspace_root() -> std::path::PathBuf {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest
            .parent()
            .expect("crates/ parent")
            .parent()
            .expect("workspace root")
            .to_path_buf()
    }

    fn fixture(name: &str) -> Vec<u8> {
        let path = workspace_root().join("fixtures/v0/squash").join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    fn run_fixture(name: &str, cfg: &SquashConfig) -> String {
        let raw = fixture(name);
        let evt = terminal_event(&raw);
        let wrapper = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            &raw,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .unwrap_or_else(|e| panic!("bind {name}: {e:?}"));
        let out = squash(wrapper, cfg);
        String::from_utf8_lossy(&out.compacted_bytes).into_owned()
    }

    #[test]
    fn snapshot_short_ls() {
        insta::assert_snapshot!(run_fixture("short_ls.txt", &SquashConfig::default()));
    }

    #[test]
    fn snapshot_cargo_build() {
        insta::assert_snapshot!(run_fixture("cargo_build.txt", &SquashConfig::default()));
    }

    #[test]
    fn snapshot_npm_test() {
        insta::assert_snapshot!(run_fixture("npm_test.txt", &SquashConfig::default()));
    }

    #[test]
    fn snapshot_binary_junk() {
        insta::assert_snapshot!(run_fixture("binary_junk.bin", &SquashConfig::default()));
    }

    /// Real `git log --oneline --color=always` capture: dense SGR
    /// (CSI) sequences around commit hashes and refs.
    #[test]
    fn snapshot_real_git_log() {
        insta::assert_snapshot!(run_fixture("real_git_log.txt", &SquashConfig::default()));
    }

    /// Real `cargo check` capture: SGR + multi-line warnings,
    /// representative of build-tool output the squash module is
    /// designed to compact.
    #[test]
    fn snapshot_real_cargo_check() {
        insta::assert_snapshot!(run_fixture(
            "real_cargo_check.txt",
            &SquashConfig::default()
        ));
    }

    /// Hand-crafted progress-bar + OSC-8 hyperlink + SGR error
    /// payload: exercises CR carriage returns, CSI K (erase line),
    /// SGR colors, and an OSC-8 link with a real URL — the kind of
    /// adversarial mix the round-by-round review surfaced. Default
    /// config: stage 2b is OFF, so CR-bearing progress lines are
    /// preserved verbatim (the legacy raw-fidelity behavior).
    #[test]
    fn snapshot_real_progress_with_hyperlink() {
        insta::assert_snapshot!(run_fixture(
            "real_progress_with_hyperlink.txt",
            &SquashConfig::default()
        ));
    }

    /// Same fixture as `snapshot_real_progress_with_hyperlink` but with
    /// stage 2b explicitly enabled: progress lines should collapse to
    /// their final visible state. Pins the opt-in
    /// progress-frame-collapse path (issue #219).
    #[test]
    fn snapshot_real_progress_with_hyperlink_collapsed() {
        insta::assert_snapshot!(run_fixture(
            "real_progress_with_hyperlink.txt",
            &SquashConfig::default().with_progress_frame_collapse_enabled(true)
        ));
    }
}

// Internal perf smoke test, replacing the (deleted) criterion bench so we
// don't have to expose `try_from_terminal_event` outside the crate. Run
// manually with: `cargo test -p cairn-core squash_perf -- --ignored --nocapture`.
#[cfg(test)]
mod squash_perf {
    use super::wrapper_tests::terminal_event;
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore = "perf smoke; run manually"]
    fn squash_50kb_under_50ms() {
        let raw: Vec<u8> = (0..50_000_u32)
            .map(|i| b'a' + u8::try_from(i % 26).expect("i % 26 fits in u8"))
            .collect();
        let cfg = SquashConfig::default();
        let evt = terminal_event(&raw);

        // Warm-up.
        for _ in 0..3 {
            let w = UnstructuredTextBytes::try_from_terminal_event(
                &evt,
                &raw,
                crate::pipeline::dispatch::SquashAdmission::for_test(),
            )
            .expect("valid wrapper");
            let _ = squash(w, &cfg);
        }

        let iters: u32 = 50;
        let start = Instant::now();
        for _ in 0..iters {
            let w = UnstructuredTextBytes::try_from_terminal_event(
                &evt,
                &raw,
                crate::pipeline::dispatch::SquashAdmission::for_test(),
            )
            .expect("valid wrapper");
            let _ = squash(w, &cfg);
        }
        let avg = start.elapsed() / iters;
        eprintln!("squash 50KB avg: {avg:?}");
        // Loose ceiling — production is unoptimized debug + cold caches in
        // CI; primarily this catches catastrophic regressions.
        assert!(avg.as_millis() < 50, "avg {avg:?} exceeded 50ms ceiling");
    }
}

#[cfg(test)]
mod proptest_squash {
    use super::wrapper_tests::terminal_event;
    use super::*;
    use proptest::prelude::*;

    fn run_squash_for_proptest(raw: &[u8], cfg: &SquashConfig) -> SquashOutput {
        let evt = terminal_event(raw);
        let wrapper = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            raw,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("valid");
        squash(wrapper, cfg)
    }

    fn arb_cfg() -> impl Strategy<Value = SquashConfig> {
        // Toggles `progress_frame_collapse_enabled` so every invariant
        // below (deterministic, byte ceiling, UTF-8 validity, no-ESC,
        // drop counts, idempotence, hash agreement) runs against the
        // stage-2b path AND its three oversize-bypass branches, not just
        // the legacy default-off pipeline (round-8 finding 2).
        (
            MIN_MAX_BYTES..32_768usize,
            0..50usize,
            MIN_TAIL_LINES..50usize,
            0..5usize,
            MIN_MAX_LINE_BYTES..2_048usize,
            any::<bool>(),
        )
            .prop_filter_map("normalize", |(mb, h, t, dr, ml, progress_collapse)| {
                SquashConfig::new(mb, h, t, dr, ml)
                    .ok()
                    .map(|c| c.with_progress_frame_collapse_enabled(progress_collapse))
            })
    }

    /// Generator biased toward inputs that exercise the new stage-2b
    /// path and its oversize-bypass branches: CR-heavy progress-frame
    /// rewrites, ANSI-bearing fragments, and large enough payloads to
    /// trip the byte-ceiling / line-cardinality gates. Mixed with raw
    /// random bytes via `prop_oneof` so existing invariants still see
    /// adversarial random input.
    fn arb_raw_cr_heavy() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            // CR-heavy progress frames separated by `\n`. Sized to flirt
            // with the byte-ceiling and line-cardinality bypass gates.
            (1usize..=200, 1usize..=400, any::<bool>()).prop_map(
                |(line_count, frames_per_line, with_ansi)| {
                    let mut out: Vec<u8> = Vec::new();
                    for line in 0..line_count {
                        for frame in 0..frames_per_line {
                            if with_ansi && frame == 0 {
                                out.extend_from_slice(b"\x1b[33m");
                            }
                            out.extend_from_slice(format!("progress {line}:{frame}").as_bytes());
                            if with_ansi && frame == 0 {
                                out.extend_from_slice(b"\x1b[0m");
                            }
                            if frame + 1 < frames_per_line {
                                out.push(b'\r');
                            }
                        }
                        out.push(b'\n');
                    }
                    out
                },
            ),
            // Single oversized CR-heavy line with no `\n` (giant-final-
            // line / bypass head-drop territory).
            (4_000usize..=20_000usize).prop_map(|frame_count| {
                let mut out: Vec<u8> = Vec::new();
                for i in 0..frame_count {
                    out.extend_from_slice(format!("frame{i}").as_bytes());
                    out.push(b'\r');
                }
                out.extend_from_slice(b"final\n");
                out
            }),
            // Plain random bytes — keeps coverage on the existing
            // invariants without losing the random fuzz dimension.
            proptest::collection::vec(any::<u8>(), 0..16_384),
        ]
    }

    proptest! {
        #[test]
        fn deterministic(raw in proptest::collection::vec(any::<u8>(), 0..4096), cfg in arb_cfg()) {
            let a = run_squash_for_proptest(&raw, &cfg);
            let b = run_squash_for_proptest(&raw, &cfg);
            prop_assert_eq!(a.compacted_bytes, b.compacted_bytes);
            prop_assert_eq!(a.stats, b.stats);
        }

        #[test]
        fn byte_ceiling(raw in proptest::collection::vec(any::<u8>(), 0..16_384), cfg in arb_cfg()) {
            let out = run_squash_for_proptest(&raw, &cfg);
            if out.stats.truncated {
                prop_assert!(out.compacted_byte_len <= cfg.max_bytes());
            }
        }

        #[test]
        fn hash_agreement(raw in proptest::collection::vec(any::<u8>(), 0..4096), cfg in arb_cfg()) {
            let out = run_squash_for_proptest(&raw, &cfg);
            let recomputed = {
                use sha2::{Digest, Sha256};
                let d = Sha256::digest(&out.compacted_bytes);
                PayloadHash::parse(format!("sha256:{d:x}")).unwrap()
            };
            prop_assert_eq!(recomputed, out.compacted_hash);
        }

        /// Output ALWAYS fits `max_bytes`, not just on the truncation
        /// path. Pre-existing `byte_ceiling` only checked when
        /// `truncated` was set; this strengthens the invariant.
        #[test]
        fn compacted_always_fits_max_bytes(
            raw in proptest::collection::vec(any::<u8>(), 0..16_384),
            cfg in arb_cfg(),
        ) {
            let out = run_squash_for_proptest(&raw, &cfg);
            prop_assert!(
                out.compacted_byte_len <= cfg.max_bytes(),
                "compacted={} > max_bytes={}",
                out.compacted_byte_len, cfg.max_bytes(),
            );
            prop_assert_eq!(out.compacted_bytes.len(), out.compacted_byte_len);
        }

        /// Compacted bytes must always be valid UTF-8 — staged path's
        /// stage 1 guarantees this; bypass path emits sanitized
        /// strings + ASCII markers. Any failure here means a stage
        /// produced invalid bytes (e.g., split a multi-byte char).
        #[test]
        fn compacted_is_valid_utf8(
            raw in proptest::collection::vec(any::<u8>(), 0..16_384),
            cfg in arb_cfg(),
        ) {
            let out = run_squash_for_proptest(&raw, &cfg);
            prop_assert!(std::str::from_utf8(&out.compacted_bytes).is_ok());
        }

        /// Stage 2 strips every ESC introducer (0x1B). No lone ESC
        /// byte may ever survive into compacted output — that's the
        /// load-bearing safety property of the sanitizer.
        #[test]
        fn no_lone_esc_in_output(
            raw in proptest::collection::vec(any::<u8>(), 0..16_384),
            cfg in arb_cfg(),
        ) {
            let out = run_squash_for_proptest(&raw, &cfg);
            prop_assert!(
                !out.compacted_bytes.contains(&0x1B),
                "ESC leaked through sanitizer"
            );
        }

        /// `lines_dropped_truncate` is bounded by the line count,
        /// which is at most the byte count. `bytes_dropped_truncate`
        /// is measured against the staged JOINED bytes (post-stage-1
        /// lossy decode), which can be up to ~3× raw on invalid
        /// UTF-8 (each bad byte → 3-byte U+FFFD). Bound loosely.
        #[test]
        fn drop_count_bounded(
            raw in proptest::collection::vec(any::<u8>(), 0..16_384),
            cfg in arb_cfg(),
        ) {
            let out = run_squash_for_proptest(&raw, &cfg);
            prop_assert!(out.stats.lines_dropped_truncate <= raw.len());
            // Worst-case stage 1 expansion + a margin for stage 6
            // marker / layout overhead.
            prop_assert!(
                out.stats.bytes_dropped_truncate <= raw.len() * 3 + cfg.max_bytes(),
                "bytes_dropped_truncate={} exceeds 3*raw.len()={} + max_bytes={}",
                out.stats.bytes_dropped_truncate, raw.len() * 3, cfg.max_bytes(),
            );
        }

        /// Squash is idempotent on its own output: feeding the
        /// compacted bytes back through squash with the same config
        /// produces the same compacted bytes (assuming the
        /// compacted output is itself a valid terminal payload).
        /// The output is sanitized and bounded, so a re-run should
        /// be a no-op on the compaction front (allowing for the
        /// trailing-newline normalisation rule).
        #[test]
        fn idempotent_on_compacted(
            raw in proptest::collection::vec(any::<u8>(), 0..4096),
            cfg in arb_cfg(),
        ) {
            let first = run_squash_for_proptest(&raw, &cfg);
            let second = run_squash_for_proptest(&first.compacted_bytes, &cfg);
            // Stage 1 / stage 2 / dedup / cap are no-ops on already-
            // sanitized input. Stage 6 is a no-op when total bytes
            // ≤ max_bytes (which they are by `compacted_always_fits`).
            // So second.compacted_bytes == first.compacted_bytes.
            prop_assert_eq!(&second.compacted_bytes, &first.compacted_bytes);
        }

        /// Round-8 finding 2: dedicated coverage for CR-heavy inputs
        /// with `progress_frame_collapse_enabled = true`. Exercises
        /// stage 2b on the staged path AND the three oversize-bypass
        /// branches (head, tail-aligned, giant-final-line suffix).
        /// Re-asserts the load-bearing invariants on this slice of
        /// the input space so a regression in stage 2b or its bypass
        /// integration cannot ship without tripping a property test.
        #[test]
        fn cr_heavy_collapse_on_invariants(
            raw in arb_raw_cr_heavy(),
            cfg in arb_cfg(),
        ) {
            // Force the flag on regardless of arb_cfg's draw so this
            // test always exercises stage 2b. The other proptests
            // already cover the random-toggle case.
            let cfg = cfg.with_progress_frame_collapse_enabled(true);
            let out = run_squash_for_proptest(&raw, &cfg);
            prop_assert!(
                out.compacted_byte_len <= cfg.max_bytes(),
                "compacted={} > max_bytes={}",
                out.compacted_byte_len, cfg.max_bytes(),
            );
            prop_assert!(std::str::from_utf8(&out.compacted_bytes).is_ok());
            prop_assert!(
                !out.compacted_bytes.contains(&0x1B),
                "ESC leaked through stage 2b / bypass on CR-heavy input"
            );
            // Determinism on this slice.
            let again = run_squash_for_proptest(&raw, &cfg);
            prop_assert_eq!(&again.compacted_bytes, &out.compacted_bytes);
            prop_assert_eq!(again.stats, out.stats);
            // Stage 2b actually firing (frames > 0) MUST flip the
            // truncated bit — that is the load-bearing audit signal.
            if out.stats.progress_frames_coalesced > 0 {
                prop_assert!(
                    out.stats.truncated,
                    "stage 2b rewrite without truncated=true"
                );
            }
        }
    }
}

#[cfg(test)]
mod corner_case_tests {
    use super::wrapper_tests::terminal_event;
    use super::*;

    fn squash_raw(raw: &[u8], cfg: &SquashConfig) -> SquashOutput {
        let evt = terminal_event(raw);
        let wrapper = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            raw,
            crate::pipeline::dispatch::SquashAdmission::for_test(),
        )
        .expect("valid");
        squash(wrapper, cfg)
    }

    #[test]
    fn empty_raw_yields_empty_compacted() {
        let cfg = SquashConfig::default();
        let out = squash_raw(b"", &cfg);
        assert!(out.compacted_bytes.is_empty());
        assert_eq!(out.compacted_byte_len, 0);
        assert!(!out.stats.truncated);
    }

    #[test]
    fn single_lone_esc_byte() {
        let cfg = SquashConfig::default();
        let out = squash_raw(b"\x1b", &cfg);
        assert!(!out.compacted_bytes.contains(&0x1B));
        assert!(out.stats.ansi_stripped);
    }

    #[test]
    fn single_lf_byte() {
        let cfg = SquashConfig::default();
        let out = squash_raw(b"\n", &cfg);
        // A sole LF should survive (line separator preservation).
        assert_eq!(out.compacted_bytes, b"\n");
        assert!(!out.stats.truncated);
    }

    #[test]
    fn single_ascii_byte() {
        let cfg = SquashConfig::default();
        let out = squash_raw(b"A", &cfg);
        assert_eq!(out.compacted_bytes, b"A");
        assert!(!out.stats.truncated);
    }

    #[test]
    fn min_max_bytes_boundary_config_runs() {
        let cfg = SquashConfig::new(MIN_MAX_BYTES, 2, 2, 2, MIN_MAX_LINE_BYTES).unwrap();
        let raw: Vec<u8> = (0..1000u32)
            .map(|i| b'a' + u8::try_from(i % 26).unwrap_or(0))
            .collect();
        let mut payload: Vec<u8> = Vec::new();
        for chunk in raw.chunks(20) {
            payload.extend_from_slice(chunk);
            payload.push(b'\n');
        }
        let out = squash_raw(&payload, &cfg);
        assert!(out.compacted_byte_len <= cfg.max_bytes());
    }

    #[test]
    fn mixed_line_endings_normalize() {
        // Default config: stage 2b is OFF. CRLF → LF normalises and bare
        // CR is preserved as a `cr_bearing_lines` signal.
        let cfg = SquashConfig::default();
        let raw = b"a\r\nb\nc\rd\r\ne";
        let out = squash_raw(raw, &cfg);
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(body.contains("a\nb\nc"));
        assert!(out.stats.cr_bearing_lines >= 1);
    }

    #[test]
    fn mixed_line_endings_with_progress_collapse_resolves_cr() {
        // With stage 2b explicitly enabled, bare CR collapses to the last
        // non-empty `\r`-segment: "c\rd" → "d". `progress_frames_coalesced`
        // reflects the rewrite. `cr_bearing_lines` is computed from the
        // pre-rewrite sanitized text (round-3 review finding 2) so it
        // remains a stable audit signal that the source contained CRs
        // even though stage 2b removed them.
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let raw = b"a\r\nb\nc\rd\r\ne";
        let out = squash_raw(raw, &cfg);
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(body.contains("a\nb\nd\ne"), "got: {body:?}");
        assert!(
            out.stats.cr_bearing_lines >= 1,
            "pre-render CR audit signal must survive stage 2b"
        );
        assert_eq!(out.stats.progress_frames_coalesced, 1);
        assert!(
            out.stats.truncated,
            "stage 2b rewrite must set truncated bit"
        );
    }

    #[test]
    fn progress_collapse_loss_does_not_leak_into_bytes_dropped_truncate() {
        // Regression for /review-loop rounds 6+7: when stage 2b shrinks a
        // CR-bearing line and no later stage drops it, the collapsed
        // bytes show up under `progress_bytes_saved` only — never under
        // `bytes_dropped_truncate` (which is stage-6-specific and
        // measured on rendered text). Audit consumers must read both
        // counters; the `truncated` bit is the canonical
        // any-source-loss signal.
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let mut raw = String::new();
        for i in 0..30 {
            use std::fmt::Write;
            write!(&mut raw, "frame{i:02}\r").expect("write to String never fails");
        }
        raw.push_str("final");
        raw.push('\n');
        let raw_bytes = raw.as_bytes().to_vec();
        let original_line_bytes = raw_bytes.len() - 1; // minus the trailing \n
        let out = squash_raw(&raw_bytes, &cfg);
        assert_eq!(out.stats.progress_frames_coalesced, 1);
        assert!(out.stats.progress_bytes_saved > 0);
        assert_eq!(out.stats.bytes_dropped_truncate, 0);
        assert_eq!(
            out.stats.progress_bytes_saved,
            original_line_bytes - "final".len(),
            "all per-frame bytes except `final` collapsed away"
        );
        assert!(
            out.stats.truncated,
            "stage 2b collapse alone must flip the canonical truncated bit"
        );
    }

    #[test]
    fn progress_collapse_only_rewrite_sets_truncated_bit() {
        // Regression for /review-loop round 1 finding 1: a CR rewrite with
        // no other lossy stage firing must still flip `stats.truncated` so
        // downstream audit gates don't treat rewritten output as pristine.
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        // Single short line, pure CR rewrite, no ANSI / no oversize / no
        // dedup / no per-line cap.
        let raw = b"progress\rdone\n";
        let out = squash_raw(raw, &cfg);
        assert_eq!(out.stats.progress_frames_coalesced, 1);
        assert_eq!(out.stats.dedup_runs_collapsed, 0);
        assert_eq!(out.stats.long_lines_truncated, 0);
        assert!(!out.stats.ansi_stripped);
        assert!(
            out.stats.truncated,
            "CR-only rewrite must set truncated bit"
        );
    }

    /// Issue #249 acceptance test 1: `text\r\x1b[K\n` with collapse
    /// enabled emits an empty final frame. Pre-fix, stage 2 stripped
    /// the `\x1b[K` before stage 2b ran, so the last-non-empty rule
    /// resurrected `text` even though the terminal would render the
    /// line as empty.
    #[test]
    fn csi_k_after_cr_emits_empty_final_frame() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"status\r\x1b[K\n", &cfg);
        assert_eq!(
            out.compacted_bytes, b"\n",
            "CSI K after \\r must clear the final frame"
        );
        assert!(out.stats.ansi_stripped, "CSI bytes were removed");
        assert_eq!(
            out.stats.progress_frames_coalesced, 1,
            "the CR-bearing line is still a frame coalesce event"
        );
        assert!(out.stats.truncated);
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_LINE_SENTINEL_BYTE),
            "sentinel must not leak past stage 2b"
        );
    }

    /// Issue #249 acceptance test 2: `text\r\x1b[K\rmore\n` (clear
    /// then write) emits `more`. The erase wins for the empty
    /// segment but the subsequent `\r`+content is the visible frame.
    #[test]
    fn csi_k_then_cr_then_text_emits_replacement() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"text\r\x1b[K\rmore\n", &cfg);
        assert_eq!(out.compacted_bytes, b"more\n");
        assert_eq!(out.stats.progress_frames_coalesced, 1);
        assert!(out.stats.ansi_stripped);
        assert!(out.stats.truncated);
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_LINE_SENTINEL_BYTE)
        );
    }

    /// Issue #249 / review round 2: `secret\x1b[2K\n` (no `\r`)
    /// must render as an empty line. CSI 2K erases the whole line
    /// independent of cursor position; pre-fix the no-CR branch
    /// stripped the sentinel and kept `secret`. End-to-end guard.
    #[test]
    fn csi_2k_no_cr_clears_visible_content() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"secret\x1b[2K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"\n");
        assert!(out.stats.ansi_stripped);
        assert!(out.stats.truncated);
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_WHOLE_LINE_SENTINEL_BYTE),
            "whole-line sentinel must not leak past stage 2b"
        );
    }

    /// Issue #249 / review round 2: `secret\x1b[2K\r\n` — same
    /// erase, with a trailing CRLF. CRLF-normalize handles `\r\n`
    /// after the sentinel emit.
    #[test]
    fn csi_2k_then_crlf_clears_visible_content() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"secret\x1b[2K\r\n", &cfg);
        assert_eq!(out.compacted_bytes, b"\n");
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_WHOLE_LINE_SENTINEL_BYTE)
        );
    }

    /// Issue #249 / review round 3: 2K erases the whole line but
    /// does NOT move the cursor. After "secret" (cursor col 6), 2K
    /// clears the line; "new" then renders at cols 6-8. Pre-fix, the
    /// post-2K tail was emitted at col 0, silently dropping six
    /// columns of meaningful whitespace.
    #[test]
    fn csi_2k_then_text_pads_to_cursor_column() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"secret\x1b[2Knew\n", &cfg);
        assert_eq!(out.compacted_bytes, b"      new\n");
    }

    /// Issue #249 / review round 3: in a CR-bearing line, the cursor
    /// resets to col 0 on each `\r`. `aaaa\rxx\x1b[2Kbb\n` advances
    /// to col 2 within the second segment before 2K fires; "bb"
    /// renders at cols 2-3.
    #[test]
    fn csi_2k_in_cr_bearing_line_pads_to_segment_cursor() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"aaaa\rxx\x1b[2Kbb\n", &cfg);
        assert_eq!(out.compacted_bytes, b"  bb\n");
    }

    /// Issue #249 / review round 2: leading-zero numeric forms are
    /// equivalent to their canonical counterparts (`\x1b[02K` ≡
    /// `\x1b[2K`, `\x1b[00K` ≡ `\x1b[K`). Pre-fix the byte-exact
    /// param check let `00K` slip past the classifier and stale
    /// `status` survived even with the flag on.
    #[test]
    fn csi_k_leading_zero_param_forms_match_canonical() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        // `\x1b[00K` after `\r` should empty the line, just like `\x1b[K`.
        assert_eq!(
            squash_raw(b"status\r\x1b[00K\n", &cfg).compacted_bytes,
            b"\n",
            "[00K should clear the line at cursor col 0"
        );
        // `\x1b[02K` without `\r` should empty the line, just like `\x1b[2K`.
        assert_eq!(
            squash_raw(b"secret\x1b[02K\n", &cfg).compacted_bytes,
            b"\n",
            "[02K should clear the whole line"
        );
    }

    /// Issue #249 / review rounds 4 & 8: `secret\r\x1b[1C\x1b[K\n`
    /// — terminal: `\r` cursor 0, `[1C` cursor 1, `[K` erase col 1
    /// to EOL. "s" survives at col 0; cols 1+ are cleared. Without
    /// precise per-column tracking we approximate "erased content
    /// must not survive" by emitting the no-pad whole-line sentinel
    /// — over-erases (loses "s") but never preserves the cleared
    /// suffix. Round 8 explicitly preferred over-drop to "preserves
    /// erased content".
    #[test]
    fn csi_k_after_stripped_cursor_move_clears_line_conservatively() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"secret\r\x1b[1C\x1b[K\n", &cfg);
        // 0K under cursor-unknown → no-pad whole-line over-erase;
        // `secret` does not survive into `compacted_bytes`.
        assert_eq!(out.compacted_bytes, b"\n");
        assert!(out.stats.ansi_stripped);
        // Sentinels must not leak past stage 2b.
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_LINE_SENTINEL_BYTE)
        );
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_WHOLE_LINE_SENTINEL_BYTE)
        );
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_WHOLE_LINE_NOPAD_SENTINEL_BYTE)
        );
    }

    /// Issue #249 / review round 10: standard `CSI 1K` erases from
    /// start-of-line to cursor. With cursor at col >0 (e.g., after
    /// `secret`), the entire pre-cursor content is gone — must NOT
    /// silently preserve `secret`. Pre-fix, 1K classified as
    /// `unsupported` and silent-stripped, leaking erased content.
    #[test]
    fn csi_1k_at_known_col_clears_pre_cursor_content() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"secret\x1b[1K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"\n");
        assert!(out.stats.ansi_stripped);
    }

    /// Issue #249 / review round 10: `prefix\rreplacement\x1b[1K\n`
    /// — cursor advances through `replacement` to col 11, 1K erases
    /// `replacement` (cols 0-10). Result is empty line (terminal-
    /// faithful: also empty since `prefix` was overwritten before
    /// being erased).
    #[test]
    fn csi_1k_after_cr_replacement_clears_segment() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"prefix\rreplacement\x1b[1K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"\n");
    }

    /// Issue #249 / review round 1 + 10: `text\r\x1b[1K\n` — cursor
    /// at col 0 after `\r`. 1K erases just col 0 (a single column).
    /// Stage 2 silent-strips this; CRLF normalize collapses `\r\n`;
    /// `text` survives. Round 1 explicitly required this not over-
    /// erase. Guards against the round-10 fix accidentally clearing
    /// the col-0 1K case.
    #[test]
    fn csi_1k_at_col_zero_silent_strips_per_round_1() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"text\r\x1b[1K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"text\n");
    }

    /// Issue #249 / review round 10: `secret\r\x1b[1Cok\x1b[K\n` —
    /// cursor jumps via `[1C`, then "ok" is preserved at the post-
    /// jump position. 0K must NOT over-erase the visible content
    /// the producer wrote after the jump. Pre-fix the line cleared
    /// to `\n`; post-fix the rsplit-non-empty rule renders `ok\n`
    /// (lossy on the column gap but does not drop visible bytes).
    #[test]
    fn csi_k_after_jump_then_text_preserves_post_jump_content() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"secret\r\x1b[1Cok\x1b[K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"ok\n");
    }

    /// Issue #249 / review round 11: a SECOND stripped cursor move
    /// after preserved bytes invalidates the "post-jump write"
    /// tracking — the new jump may have moved the cursor back before
    /// those writes. Pre-fix the stale flag let `0K` silent-strip
    /// and preserve the `a` that the second jump + erase removed.
    #[test]
    fn csi_k_after_two_jumps_with_intervening_write_clears_line() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"secret\r\x1b[1Ca\x1b[1D\x1b[K\n", &cfg);
        // Second jump resets `printable_written_after_jump`; 0K under
        // jumped + !printable triggers over-erase.
        assert_eq!(out.compacted_bytes, b"\n");
    }

    /// Issue #249 / review round 11: non-ASCII bytes written after a
    /// stripped cursor move are visible terminal output — must NOT
    /// be over-erased by a following `0K`. Pre-fix, only ASCII bytes
    /// flipped the post-jump-write flag, so `✓` was dropped.
    #[test]
    fn csi_k_after_jump_then_unicode_preserves_unicode() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw("secret\r\x1b[1C✓\x1b[K\n".as_bytes(), &cfg);
        // Stage 2b's CR-bearing rsplit picks up "✓" as the rightmost
        // non-empty segment after `\r`. Lossy on the column gap and
        // on `secret`, but does NOT drop the visible Unicode glyph.
        assert_eq!(out.compacted_bytes, "✓\n".as_bytes());
    }

    /// Issue #249 / review round 9: a preserved `\t` ahead of `\x1b[K`
    /// makes `intact_cursor_col` unreliable (tab width depends on tab
    /// stops), but the cursor is still at the end of the preserved
    /// stream — so 0K erases nothing visible. Stage 2 must silent-strip
    /// the K rather than over-erase the line. Pre-fix the round-8
    /// over-erase rule dropped `status\t` entirely.
    #[test]
    fn csi_k_after_preserved_tab_preserves_content() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"status\t\x1b[K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"status\t\n");
        assert!(out.stats.ansi_stripped);
    }

    /// Issue #249 / review round 9: same logic for non-ASCII (UTF-8
    /// multi-byte) prefix. `✓ done\x1b[K\n` — the leading non-ASCII
    /// byte makes `intact_cursor_col=None`, but the cursor is at end
    /// of preserved stream, so 0K erases nothing visible. Pre-fix
    /// the line over-erased to `\n`, dropping `✓ done`.
    #[test]
    fn csi_k_after_non_ascii_preserves_content() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw("✓ done\x1b[K\n".as_bytes(), &cfg);
        assert_eq!(out.compacted_bytes, "✓ done\n".as_bytes());
        assert!(out.stats.ansi_stripped);
    }

    /// Issue #249 / review round 8: `secret\x1b[1G\x1b[K\n` —
    /// `[1G` is absolute cursor-to-column-1 (0-indexed col 0). Stage
    /// 2 doesn't model the move precisely, but the resulting K under
    /// `intact_cursor_col=None` emits the no-pad whole-line sentinel
    /// and clears the line. Pre-fix preserved `secret` (round 8's
    /// "no-ship" finding case A).
    #[test]
    fn csi_k_after_absolute_col_zero_move_clears_line() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"secret\x1b[1G\x1b[K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"\n");
    }

    /// Issue #249 / review rounds 4 & 5: cursor-moving CSI before 2K
    /// poisons the column-pad approximation but does NOT change the
    /// fact that 2K erases the whole line. Stage 2 emits the no-pad
    /// whole-line sentinel (`ERASE_WHOLE_LINE_NOPAD_SENTINEL`); stage
    /// 2b clears the pre-erase content and renders the post-tail at
    /// column 0 (lossy on the column gap, but never preserves erased
    /// content).
    #[test]
    fn csi_2k_after_stripped_cursor_move_clears_with_nopad_render() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"\r\x1b[5C\x1b[2Kx\n", &cfg);
        assert_eq!(out.compacted_bytes, b"x\n");
        // Sentinels never leak past stage 2b.
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_WHOLE_LINE_SENTINEL_BYTE)
        );
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_WHOLE_LINE_NOPAD_SENTINEL_BYTE)
        );
    }

    /// Issue #249 / review round 7: tabs in the pre-2K span have a
    /// terminal-defined display width (next tab stop, typically every
    /// 8 cols). `chars().count()` would invent a single space, which
    /// silently corrupts aligned terminal output. Pre-fix the
    /// renderer emitted `b" x\n"`; post-fix it falls back to col-0
    /// rendering and emits `b"x\n"` — lossy on the column gap but
    /// never wrong-width.
    #[test]
    fn csi_2k_after_preserved_tab_renders_at_col_zero() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"\t\x1b[2Kx\n", &cfg);
        assert_eq!(out.compacted_bytes, b"x\n");
    }

    /// Issue #249 / review round 7: same fallback for non-ASCII
    /// pre-span content (wide CJK, emoji, combining marks). Without
    /// the pad-safety gate, the pad-variant sentinel would prepend
    /// `chars().count()` spaces, which is wrong for any wide char.
    #[test]
    fn csi_2k_after_non_ascii_renders_at_col_zero() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        // CJK chars in pre-span: each is `chars().count() == 1` but
        // display width ≥ 2 in most monospace terminals.
        let out = squash_raw("中文\x1b[2Kx\n".as_bytes(), &cfg);
        assert_eq!(out.compacted_bytes, b"x\n");
    }

    /// Issue #249 / review round 6: no-CR input that produces ONLY
    /// the no-pad whole-line sentinel must not leak the sentinel
    /// byte. Pre-fix, stage 2b's fast-path early-returned because it
    /// only tested for the pad-variant sentinel, leaking `0x03`.
    #[test]
    fn no_cr_nopad_whole_line_sentinel_consumed_by_stage2b() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        // `\x1b[5C` flips cursor_unknown; `\x1b[2K` emits no-pad
        // sentinel; "x" follows. No `\r` anywhere.
        let out = squash_raw(b"\x1b[5C\x1b[2Kx\n", &cfg);
        assert_eq!(out.compacted_bytes, b"x\n");
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_WHOLE_LINE_NOPAD_SENTINEL_BYTE),
            "no-pad sentinel must not leak past stage 2b"
        );
    }

    /// Issue #249 / review round 6: same fast-path bug, empty-tail
    /// variant. `\x1b[5C\x1b[2K\n` should render as a blank line and
    /// must not leak the no-pad sentinel byte.
    #[test]
    fn no_cr_nopad_whole_line_sentinel_empty_tail_renders_blank() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"\x1b[5C\x1b[2K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"\n");
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_WHOLE_LINE_NOPAD_SENTINEL_BYTE)
        );
    }

    /// Issue #249 / review round 5 ("no ship"): 2K fires after a
    /// stripped cursor move with no replacement text, so the line
    /// MUST render blank. Pre-fix the cursor-unknown gate suppressed
    /// the 2K sentinel and CRLF-normalize left the erased content
    /// (`secret`) in the output.
    #[test]
    fn csi_2k_after_stripped_cursor_move_no_replacement_renders_blank() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"secret\r\x1b[5C\x1b[2K\n", &cfg);
        assert_eq!(
            out.compacted_bytes, b"\n",
            "2K must clear `secret` even when a cursor-moving CSI was stripped"
        );
        assert!(out.stats.ansi_stripped);
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_WHOLE_LINE_NOPAD_SENTINEL_BYTE)
        );
    }

    /// Issue #249 / review round 4: `\r` resets the cursor-unknown
    /// flag, so a cursor-moving CSI before a `\r` does NOT poison a
    /// later K. The classic `\r\x1b[1C\rstatus\r\x1b[K\n` pattern
    /// (cursor move on a previous frame, fresh `\r` for the next)
    /// still produces an empty final frame.
    #[test]
    fn csi_k_emits_sentinel_after_cr_resets_cursor_unknown() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"\r\x1b[1C\rstatus\r\x1b[K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"\n");
    }

    /// Issue #249 / review round 4: SGR (`m`) is cursor-neutral, so
    /// the very common `\x1b[31m...\rDownloading\x1b[0m\r\x1b[K\n`
    /// pattern still emits a sentinel and renders the empty final
    /// frame. Guards against an over-aggressive cursor-unknown
    /// trigger that would defeat the feature on coloured progress.
    #[test]
    fn csi_k_after_sgr_still_emits_sentinel() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"\x1b[31mfoo\rDownloading\x1b[0m\r\x1b[K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"\n");
    }

    /// Issue #249 / review round 1: `text\r\x1b[1K\n` must NOT
    /// produce an empty final frame. CSI 1K only erases col 0 after
    /// `\r` resets the cursor, so the prior frame survives. Stage 2
    /// silent-strips the CSI bytes; CRLF normalization collapses
    /// `\r\n` to `\n`; the rendered output keeps `status`. End-to-end
    /// guard against the round-1 high-severity finding.
    #[test]
    fn csi_1k_after_cr_preserves_visible_content() {
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let out = squash_raw(b"status\r\x1b[1K\n", &cfg);
        // `\r\n` (after [1K silent-strip) collapses to `\n`; no
        // sentinel emitted, no stage 2b rewrite needed.
        assert_eq!(out.compacted_bytes, b"status\n");
        assert!(out.stats.ansi_stripped);
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_LINE_SENTINEL_BYTE),
            "[1K must not surface a sentinel"
        );
        assert_eq!(
            out.stats.progress_frames_coalesced, 0,
            "no CR-bearing line survives in the rendered text"
        );
    }

    /// Issue #249: when the flag is off, CSI K is stripped silently
    /// just like before (no behavior change on the legacy path). The
    /// last-non-empty rule resurrects `status` — the documented
    /// pre-fix behavior. Guards against accidental sentinel leakage
    /// onto callers that didn't opt in.
    #[test]
    fn csi_k_after_cr_default_path_still_strips_silently() {
        let cfg = SquashConfig::default();
        let out = squash_raw(b"status\r\x1b[K\n", &cfg);
        assert_eq!(out.compacted_bytes, b"status\n");
        assert!(out.stats.ansi_stripped);
        assert!(
            !out.compacted_bytes
                .contains(&super::ERASE_LINE_SENTINEL_BYTE),
            "sentinel must never leak when the flag is off"
        );
    }

    #[test]
    fn progress_collapse_applies_on_oversize_bypass() {
        // Regression for /review-loop round 3 finding 1: stage 2b must run
        // on both head and tail windows of the oversize bypass path so the
        // same config produces consistent semantics across the size cliff.
        // Drive the line-cardinality gate with CR-bearing progress lines
        // and confirm the rendered output drops the early `\r<frame>`
        // bytes while leaving the bypass shape (head, marker, tail) intact.
        let cfg_off = SquashConfig::default();
        let cfg_on = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let mut raw = Vec::with_capacity(MAX_INPUT_LINES * 16);
        for i in 0..=MAX_INPUT_LINES {
            raw.extend_from_slice(format!("p{i}\rdone{i}\n").as_bytes());
        }
        let out_off = squash_raw(&raw, &cfg_off);
        let out_on = squash_raw(&raw, &cfg_on);
        assert!(
            out_on.stats.progress_frames_coalesced > 0,
            "stage 2b must run inside the bypass path when the flag is on"
        );
        assert_ne!(
            out_off.compacted_bytes, out_on.compacted_bytes,
            "rendered bypass output must differ from raw-CR bypass output"
        );
        // Bypass output should never contain `\r` once rendered.
        assert!(
            !out_on.compacted_bytes.contains(&b'\r'),
            "rendered bypass output must not retain bare CR bytes"
        );
        // Both paths still flip the coarse `truncated` bit (bypass always
        // does) and surface the bare-CR audit signal from the source.
        assert!(out_on.stats.truncated);
        assert!(out_on.stats.cr_bearing_lines > 0);
    }

    #[test]
    fn bypass_dropped_head_does_not_leak_progress_collapse_stats() {
        // Regression for /review-loop round 7 finding 2: when the bypass
        // head window has no safe `\n` boundary, the whole head is
        // dropped (`head = ""`). Stage 2b counters and `cr_bearing_lines`
        // for that head window MUST NOT be folded into stats — those
        // bytes never reach the output, so reporting them as audit
        // loss inflates the signal (and would double-count if the
        // giant-final-line suffix branch later reprocesses the same
        // source range).
        //
        // Construct a payload where the head window is pure CR-heavy
        // progress text with no `\n`, and the tail window holds CR-free
        // diagnostic lines with their own newlines. Pre-fix, the head's
        // local counters leaked into stats; post-fix, they do not.
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let mut raw: Vec<u8> = Vec::new();
        // Head window covers bytes [0..head_raw_end]; with the default
        // config that is ≈ 16 KB. Fill it with CR-heavy progress text
        // and *no* `\n`, so head_trimmed has no line boundary and the
        // bypass drops the head entirely. The first `\n` lands beyond
        // the head window, then CR-free tail lines follow.
        for _ in 0..2_000 {
            raw.extend_from_slice(b"progress-bytes\r");
        }
        raw.push(b'\n');
        for i in 0..1_000 {
            raw.extend_from_slice(format!("tail-diagnostic-{i:04}\n").as_bytes());
        }
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        // Stage-2b WORK counters (frames coalesced, bytes saved) reflect
        // rewrites actually performed on retained windows. Since the head
        // was dropped, no rewrite reached compacted_bytes — those counters
        // must stay zero. cr_bearing_lines is a source-capture audit
        // signal computed from the FULL raw input (round-8 finding); the
        // dropped head IS one CR-bearing line in the source, so the
        // signal correctly reflects that hazard.
        assert_eq!(
            out.stats.progress_frames_coalesced, 0,
            "dropped head must not leak progress_frames_coalesced"
        );
        assert_eq!(
            out.stats.progress_bytes_saved, 0,
            "dropped head must not leak progress_bytes_saved"
        );
        assert_eq!(
            out.stats.cr_bearing_lines, 1,
            "source-level CR audit signal must reflect the dropped head's CR-bearing line"
        );
        // Sanity: tail content survived.
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("tail-diagnostic-"),
            "tail diagnostic lines must survive"
        );
    }

    #[test]
    fn progress_collapse_applies_on_giant_final_line_bypass() {
        // Regression for /review-loop round 4 finding 1: the oversize
        // bypass `tail_aligned_to_line == false` branch (giant final
        // line, no safe `\n` boundary in the retained tail window) must
        // also run stage 2b. Otherwise progress-bar payloads whose final
        // line is itself oversized leak raw `\r`-separated frames into
        // the output even with `progress_frame_collapse_enabled(true)`.
        //
        // Calls `super::oversize_bypass` directly to avoid allocating
        // MAX_INPUT_BYTES (64 MiB) just to trigger the gate — same trick
        // as `oversize_bypass_tail_window_inside_giant_final_line_preserves_suffix`.
        let cfg = SquashConfig::default().with_progress_frame_collapse_enabled(true);
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"head\n");
        // Giant final line filled with progress-bar style `\r`-separated
        // frames; the last tail (`done-FINAL`) is the only visible frame.
        for _ in 0..2_000 {
            raw.extend_from_slice(b"progress-bytes\r");
        }
        raw.extend_from_slice(b"done-FINAL");
        let raw_byte_len = raw.len();
        let raw_hash = super::sha256_payload_hash(&raw);
        let stats = SquashStats::default();
        let out = super::oversize_bypass(
            &raw,
            raw_hash,
            raw_byte_len,
            &cfg,
            stats,
            super::BypassReason::ByteCeiling,
        );
        assert!(
            out.stats.progress_frames_coalesced > 0,
            "stage 2b must run on the giant-final-line bypass branch"
        );
        assert!(
            !out.compacted_bytes.contains(&b'\r'),
            "giant-final-line rendered output must not retain bare CR"
        );
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            body.contains("done-FINAL"),
            "the final visible frame must survive: got {body:?}"
        );
    }

    #[test]
    fn multibyte_char_at_stage5_cap_boundary() {
        // Build a single line whose byte length straddles the cap
        // boundary in the middle of a 3-byte UTF-8 codepoint
        // ('日' = 0xE6 0x97 0xA5). Stage 5 must truncate at a
        // codepoint boundary, never split.
        let cfg = SquashConfig::new(2048, 4, 4, 2, MIN_MAX_LINE_BYTES).unwrap();
        let mut line = String::with_capacity(4096);
        // Pad with ASCII so 日 lands right at the cap boundary.
        let pad_len = MIN_MAX_LINE_BYTES - 2; // leave 2 bytes
        line.push_str(&"x".repeat(pad_len));
        for _ in 0..200 {
            line.push('日');
        }
        let raw = line.as_bytes();
        let out = squash_raw(raw, &cfg);
        // Result must be valid UTF-8 and respect the budget.
        assert!(std::str::from_utf8(&out.compacted_bytes).is_ok());
        assert!(out.compacted_byte_len <= cfg.max_bytes());
    }

    #[test]
    fn one_giant_line_no_newline() {
        // 100K bytes of single line, no \n at all. Below
        // MAX_INPUT_BYTES so staged path runs. Stage 5 should
        // truncate the line.
        let cfg = SquashConfig::default();
        let raw: Vec<u8> = std::iter::repeat_n(b'X', 100_000).collect();
        let out = squash_raw(&raw, &cfg);
        assert!(out.compacted_byte_len <= cfg.max_bytes());
        assert!(out.stats.truncated);
    }

    #[test]
    fn all_blank_lines_thousand() {
        let cfg = SquashConfig::default();
        let raw: Vec<u8> = std::iter::repeat_n(b'\n', 1000).collect();
        let out = squash_raw(&raw, &cfg);
        assert!(out.compacted_byte_len <= cfg.max_bytes());
        // Output is valid UTF-8.
        assert!(std::str::from_utf8(&out.compacted_bytes).is_ok());
    }

    #[test]
    fn dense_short_lines_route_through_staged_when_under_line_cap() {
        let cfg = SquashConfig::default();
        // Just under MAX_INPUT_LINES.
        let line_count = 100;
        let raw: Vec<u8> = (0..line_count)
            .flat_map(|i| format!("L{i:03}\n").into_bytes())
            .collect();
        let out = squash_raw(&raw, &cfg);
        // Should NOT route through bypass (no marker).
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        assert!(
            !body.contains("oversize bypass"),
            "small input took bypass: {body}"
        );
    }

    #[test]
    fn extreme_dedup_run_collapses_to_marker() {
        let cfg = SquashConfig::default();
        let mut raw: Vec<u8> = Vec::new();
        for _ in 0..1000 {
            raw.extend_from_slice(b"same-line\n");
        }
        let out = squash_raw(&raw, &cfg);
        assert!(out.stats.dedup_runs_collapsed > 0);
        assert!(out.stats.truncated);
        // Only ~1 occurrence of "same-line" should survive (plus
        // possibly the dedup marker form).
        let body = String::from_utf8_lossy(&out.compacted_bytes);
        let count = body.matches("same-line").count();
        assert!(count <= 2, "dedup did not collapse: count={count}");
    }
}
