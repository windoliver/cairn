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
        })
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

use crate::domain::capture::{CaptureEvent, CapturePayload, PayloadHash};
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
    /// event's `payload_ref` pointed at, and the sensor-supplied
    /// terminal context.
    ///
    /// # Stability
    /// `pub(crate)` until #218 lands. The current signature accepts
    /// `TerminalContext` as a side input, which lets a misbehaving caller
    /// reclassify a stored structured payload as interactive and lose
    /// machine-readable bytes through squash. Once `TerminalContext` is
    /// persisted on `CapturePayload::Terminal` (see #218), this becomes
    /// derivable from the event alone and the API stabilizes. The
    /// surrounding `pipeline` module is also `pub(crate)` until the
    /// dispatch driver (#217) is the sole entry point.
    ///
    /// # Errors
    /// `NotTerminalPayload`, `HashMismatch`, or
    /// `StructuredContextRejected` per the spec's caller contract.
    // Only reachable from #[cfg(test)] modules until #217 wires the
    // dispatch driver; default-feature non-test builds legitimately
    // leave it dead.
    #[allow(dead_code)]
    pub(crate) fn try_from_terminal_event(
        event: &CaptureEvent,
        raw: &'a [u8],
        context: TerminalContext,
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
        if !matches!(event.payload, CapturePayload::Terminal { .. }) {
            return Err(UnstructuredBindError::NotTerminalPayload);
        }
        if context != TerminalContext::InteractiveTty {
            return Err(UnstructuredBindError::StructuredContextRejected);
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

/// Sensor-supplied execution context for a Terminal payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TerminalContext {
    /// The terminal session is an interactive TTY; output is unstructured.
    InteractiveTty,
    /// Non-interactive session or structured (machine-readable) output;
    /// squash must be bypassed.
    NonInteractiveOrStructured,
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
            TerminalContext::InteractiveTty,
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
            TerminalContext::InteractiveTty,
        )
        .unwrap_err();
        assert!(matches!(err, UnstructuredBindError::HashMismatch));
    }

    #[test]
    fn rejects_structured_context() {
        let bytes = b"hello\n";
        let evt = terminal_event(bytes);
        let err = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            bytes,
            TerminalContext::NonInteractiveOrStructured,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            UnstructuredBindError::StructuredContextRejected
        ));
    }

    #[test]
    fn accepts_terminal_interactive_tty_with_matching_hash() {
        let bytes = b"hello\n";
        let evt = terminal_event(bytes);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            bytes,
            TerminalContext::InteractiveTty,
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
            TerminalContext::InteractiveTty,
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
            TerminalContext::InteractiveTty,
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
            TerminalContext::InteractiveTty,
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
            TerminalContext::InteractiveTty,
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
        oversized[..5].copy_from_slice(b"HEADX");
        let n = oversized.len();
        oversized[n - 5..].copy_from_slice(b"YTAIL");
        let evt = terminal_event(&oversized);
        let wrapped = UnstructuredTextBytes::try_from_terminal_event(
            &evt,
            &oversized,
            TerminalContext::InteractiveTty,
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
            TerminalContext::InteractiveTty,
        )
        .unwrap_err();
        assert!(
            matches!(err, UnstructuredBindError::EventValidationFailed(_)),
            "got: {err:?}"
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
    /// Number of lines containing at least one bare `\r` after CRLF normalize.
    pub cr_bearing_lines: usize,
    /// Number of dedup runs collapsed in stage 4.
    pub dedup_runs_collapsed: usize,
    /// Number of lines dropped by stage 6 head/tail truncation.
    pub lines_dropped_truncate: usize,
    /// Total bytes dropped by stage 6 head/tail truncation.
    pub bytes_dropped_truncate: usize,
    /// Number of lines that exceeded `max_line_bytes` and were truncated in stage 5.
    pub long_lines_truncated: usize,
    /// True iff `compacted_bytes` is NOT a verbatim sanitization-free
    /// copy of the input — i.e., any lossy transform acted: stage 2
    /// ANSI/OSC strip, stage 2 OSC recovery drop, stage 4 dedup
    /// collapse, stage 5 per-line cap, stage 6 head/tail truncation,
    /// or the oversize bypass. Downstream code that gates fallback /
    /// raw-retention / warning banners on a single coarse bit should
    /// use this; per-stage counters give the breakdown.
    pub truncated: bool,
    /// Bytes discarded by stage-2 OSC recovery on unterminated escape
    /// sequences (introducer + body up to the next `\n`, or to EOF if
    /// no `\n` exists). Distinct from `bytes_dropped_truncate` so audit
    /// consumers can tell sanitization-driven loss apart from
    /// budget-driven truncation. Hardening for truncated terminal
    /// captures whose final diagnostic line followed a stray `ESC ]`.
    pub osc_recovery_bytes_dropped: usize,
}

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
fn stage2_ansi_strip(input: &str, stripped: &mut bool, osc_recovery_dropped: &mut usize) -> String {
    let bytes = input.as_bytes();
    let mut normalized: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        let b = bytes[i];

        if b == 0x1B {
            // ESC: look ahead for CSI or OSC
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
                        let mut j = i + 2;
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
                            i = j + 1;
                        } else {
                            // Truncated CSI (no final byte before EOF):
                            // drop ESC only so outer loop processes
                            // `[`, params, and surviving payload.
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
                        }
                    }
                    // Lone ESC or unrecognised: drop ESC byte only
                    _ => {
                        i += 1;
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
            } else {
                *stripped = true;
            }
            i += 1;
        } else {
            normalized.push(b);
            i += 1;
        }
    }

    // Second pass: CRLF → LF (bare CR is preserved)
    let mut result: Vec<u8> = Vec::with_capacity(normalized.len());
    let mut j = 0;
    while j < normalized.len() {
        if normalized[j] == b'\r' && j + 1 < normalized.len() && normalized[j + 1] == b'\n' {
            // CRLF → LF
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

    fn s2(input: &str) -> (String, bool) {
        let mut stripped = false;
        let mut osc_dropped = 0usize;
        let out = stage2_ansi_strip(input, &mut stripped, &mut osc_dropped);
        (out, stripped)
    }

    /// Helper that also returns the OSC-recovery-dropped byte count.
    fn s2_with_osc(input: &str) -> (String, bool, usize) {
        let mut stripped = false;
        let mut osc_dropped = 0usize;
        let out = stage2_ansi_strip(input, &mut stripped, &mut osc_dropped);
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
    if tail_start > last_content_idx {
        tail_start = last_content_idx;
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
    let tail_take = lines.len() - tail_start;
    let tail_slice = &lines[tail_start..];
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
    let mut current_tail_end = lines.len();

    if let Some(head_budget) = signed_head_budget {
        head_take = cfg.head_lines().min(tail_start);
        let mut head_bytes: usize =
            lines[..head_take].iter().map(String::len).sum::<usize>() + head_take.saturating_sub(1);
        while head_bytes > head_budget && head_take > 0 {
            head_take -= 1;
            head_bytes = lines[..head_take].iter().map(String::len).sum::<usize>()
                + head_take.saturating_sub(1);
        }
        for line in &lines[head_take..current_tail_start] {
            dropped_lines += 1;
            // Each dropped line also removes its trailing LF separator
            // from the joined body, so account for it in audit metadata.
            dropped_bytes += line.len() + 1;
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

    let decoded = stage1_lossy_utf8(raw_bytes);
    let stage2 = stage2_ansi_strip(
        &decoded,
        &mut stats.ansi_stripped,
        &mut stats.osc_recovery_bytes_dropped,
    );
    let (raw_lines_borrow, trailing_newline) = stage3_split_lines(&stage2);
    let raw_lines: Vec<String> = raw_lines_borrow.iter().map(|s| (*s).to_string()).collect();

    stats.cr_bearing_lines = raw_lines.iter().filter(|l| l.contains('\r')).count();

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
    // the staged path: ANSI strip, OSC recovery drop, dedup collapse,
    // per-line cap. Stage 6's drop loop sets it again on tail/head trim.
    if stats.ansi_stripped
        || stats.osc_recovery_bytes_dropped > 0
        || stats.dedup_runs_collapsed > 0
        || long_lines_count > 0
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
        BypassReason::ByteCeiling => 0, // not rendered for this branch
    };
    let marker = reason.marker(raw_byte_len, line_count);
    let marker_bytes = marker.as_bytes();
    let budget = max_body.saturating_sub(marker_bytes.len() + 2 /* two LFs */);
    let half = budget / 2;
    let head_raw_end = (half * 2).min(raw_bytes.len());
    let head_decoded = stage1_lossy_utf8(&raw_bytes[..head_raw_end]);
    let mut head_stripped = false;
    let head_sanitized = stage2_ansi_strip(
        &head_decoded,
        &mut head_stripped,
        &mut stats.osc_recovery_bytes_dropped,
    );
    stats.ansi_stripped |= head_stripped;
    // CR-bearing detection on the sanitized head: bare `\r` in the
    // retained content is a renderer-safety hazard the module
    // documents, and the bypass path must surface that signal to
    // callers just like the staged path does.
    stats.cr_bearing_lines += head_sanitized
        .split('\n')
        .filter(|l| l.contains('\r'))
        .count();
    let head_trimmed = trim_to_byte_budget_at_boundary(&head_sanitized, half);
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

    let tail_window = (half * 2).min(raw_bytes.len());
    let mut tail_raw_start = raw_bytes.len() - tail_window;
    let mut tail_aligned_to_line = false;
    if let Some(rel) = raw_bytes[tail_raw_start..].iter().position(|&b| b == b'\n') {
        tail_raw_start += rel + 1;
        tail_aligned_to_line = true;
    }
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
        );
        stats.cr_bearing_lines += tail_sanitized
            .split('\n')
            .filter(|l| l.contains('\r'))
            .count();
        stats.ansi_stripped |= tail_stripped;
        let tail_trimmed = trim_to_byte_budget_at_boundary_from_end(&tail_sanitized, half);
        // The raw slice was line-aligned (begins after a `\n`), so an
        // UN-trimmed tail starts on a line boundary. Only realign if
        // `trim_to_byte_budget_at_boundary_from_end` actually shaved
        // bytes off the front — that's when the first byte can sit
        // mid-line again (Round-7 finding).
        let was_trimmed = tail_trimmed.len() < tail_sanitized.len();
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
        if final_line_truncated {
            let final_marker = b"[\xe2\x80\xa6final-line truncated\xe2\x80\xa6]\n";
            compacted_bytes.extend_from_slice(final_marker);
        }
        compacted_bytes.extend_from_slice(tail_aligned.as_bytes());
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
            // No newline in emitted tail (final_line_truncated case):
            // the raw span ≈ the bytes we emitted from the source's
            // last line. Capped at the post-line-align span so we
            // never claim more preserved bytes than exist.
            tail_source_bytes = tail_aligned.len().min(raw_byte_len - tail_raw_start);
        }
    } else if !raw_bytes.is_empty() {
        let degraded = b"[\xe2\x80\xa6tail dropped: oversized single-line payload (no line boundary in retained window)\xe2\x80\xa6]";
        compacted_bytes.extend_from_slice(degraded);
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
            TerminalContext::InteractiveTty,
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
            TerminalContext::InteractiveTty,
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
                TerminalContext::InteractiveTty,
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
                TerminalContext::InteractiveTty,
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
            TerminalContext::InteractiveTty,
        )
        .expect("valid");
        squash(wrapper, cfg)
    }

    fn arb_cfg() -> impl Strategy<Value = SquashConfig> {
        (
            MIN_MAX_BYTES..32_768usize,
            0..50usize,
            MIN_TAIL_LINES..50usize,
            0..5usize,
            MIN_MAX_LINE_BYTES..2_048usize,
        )
            .prop_filter_map("normalize", |(mb, h, t, dr, ml)| {
                SquashConfig::new(mb, h, t, dr, ml).ok()
            })
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
    }
}
