//! Tool-squash: compact verbose terminal output before extraction
//! (brief §5.2 Tool-squash row, issue #72). See
//! `docs/superpowers/specs/2026-04-27-issue-72-tool-squash-design.md`.
//!
//! Pure function. No I/O. Deterministic: same `(raw, cfg)` always
//! produces byte-identical `compacted_bytes`.

#![allow(clippy::module_name_repetitions)] // Squash* names are intentional

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

// Compile-time invariant: MIN_MAX_BYTES must hold the tail-locked pair
// + skip-marker + layout newlines.
const _: () = assert!(
    MIN_MAX_BYTES >= 2 * MIN_MAX_LINE_BYTES + MARKER_MAX_LEN + LAYOUT_OVERHEAD
);

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
        let needed = 2 * max_line_bytes + MARKER_MAX_LEN + LAYOUT_OVERHEAD;
        if needed > max_bytes {
            return Err(SquashConfigError::LineCapExceedsLayoutBudget {
                line: max_line_bytes,
                marker: MARKER_MAX_LEN,
                overhead: LAYOUT_OVERHEAD,
                total: max_bytes,
            });
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
    /// # Errors
    /// `NotTerminalPayload`, `HashMismatch`, or
    /// `StructuredContextRejected` per the spec's caller contract.
    pub fn try_from_terminal_event(
        event: &CaptureEvent,
        raw: &'a [u8],
        context: TerminalContext,
    ) -> Result<Self, UnstructuredBindError> {
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
#[derive(Debug, Clone, Copy, thiserror::Error)]
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
        assert!(matches!(err, SquashConfigError::MaxLineBytesTooSmall { .. }));
    }

    #[test]
    fn rejects_tail_lines_below_min() {
        let err = SquashConfig::new(16384, 100, 1, 2, 4096).unwrap_err();
        assert!(matches!(err, SquashConfigError::TailLinesTooSmall { .. }));
    }

    #[test]
    fn rejects_cross_field_budget_violation() {
        let max_bytes = MIN_MAX_BYTES; // 512
        let max_line_bytes = 200; // 2*200+128+4 = 532 > 512
        let err = SquashConfig::new(max_bytes, 100, 100, 2, max_line_bytes).unwrap_err();
        assert!(matches!(err, SquashConfigError::LineCapExceedsLayoutBudget { .. }));
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
        PayloadHash::parse(format!("sha256:{digest:x}"))
            .expect("sha256 string is well-formed")
    }

    fn ts() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-04-27T00:00:00Z").expect("valid timestamp")
    }

    fn terminal_event(payload_bytes: &[u8]) -> CaptureEvent {
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
    /// True iff any stage 5 or stage 6 truncation occurred.
    pub truncated: bool,
}

use std::borrow::Cow;

/// Stage 1: lossy UTF-8 decode. Invalid byte sequences become
/// U+FFFD; valid input passes through borrowed.
#[allow(dead_code)] // Used by higher stages in later tasks
fn stage1_lossy_utf8(raw: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(raw)
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
#[allow(dead_code)] // Used by squash() entrypoint in Task 13
#[allow(clippy::expect_used)] // invariant: only ASCII control bytes/ESC sequences removed; UTF-8 preserved
fn stage2_ansi_strip(input: &str, stripped: &mut bool) -> String {
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
                    0x5B => {
                        i += 2; // skip ESC [
                        // skip parameter bytes 0x30-0x3F
                        while i < bytes.len() && (0x30..=0x3F).contains(&bytes[i]) {
                            i += 1;
                        }
                        // skip intermediate bytes 0x20-0x2F
                        while i < bytes.len() && (0x20..=0x2F).contains(&bytes[i]) {
                            i += 1;
                        }
                        // skip final byte 0x40-0x7E (if present)
                        if i < bytes.len() && (0x40..=0x7E).contains(&bytes[i]) {
                            i += 1;
                        }
                        // malformed CSI (no final byte): already consumed params/intermediates
                    }
                    // OSC: ESC ] ... terminated by BEL (0x07) or ESC \ (0x1B 0x5C)
                    0x5D => {
                        i += 2; // skip ESC ]
                        loop {
                            if i >= bytes.len() {
                                // unterminated OSC: drop to EOF
                                break;
                            }
                            if bytes[i] == 0x07 {
                                // BEL terminator
                                i += 1;
                                break;
                            }
                            if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == 0x5C {
                                // ST (String Terminator): ESC \
                                i += 2;
                                break;
                            }
                            i += 1;
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
        let out = stage2_ansi_strip(input, &mut stripped);
        (out, stripped)
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
}

/// Stage 3: split on `\n`. Returns `(lines, trailing_newline_flag)`.
/// Interior empty segments are preserved as empty lines; a trailing
/// `\n` produces an empty final segment that is NOT a line.
#[allow(dead_code)] // Used by squash() entrypoint in Task 13
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

/// Stage 4: consecutive-run dedup on full source lines. Split-form last-line
/// exemption: when the final repeat run reaches the input's last line, the
/// final line is preserved verbatim and earlier duplicates collapse to
/// `<line> [×N-1]`. CR-bearing lines never collapse.
#[allow(dead_code)] // Used by squash() entrypoint in Task 13
fn stage4_dedup(
    lines: &[String],
    min_run: usize,
    collapsed_runs: &mut usize,
) -> Vec<String> {
    if lines.is_empty() || min_run < 2 {
        return lines.to_vec();
    }
    let last_idx = lines.len() - 1;
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        let mut j = i + 1;
        while j < lines.len() && &lines[j] == line {
            j += 1;
        }
        let run_len = j - i;
        let run_contains_last = j - 1 == last_idx;
        let cr_bearing = line.contains('\r');

        if run_len >= min_run && !cr_bearing {
            if run_contains_last {
                let count = run_len - 1;
                if count >= min_run {
                    out.push(format!("{line} [×{count}]"));
                    *collapsed_runs += 1;
                } else {
                    for _ in 0..count {
                        out.push(line.clone());
                    }
                }
                out.push(line.clone());
            } else {
                out.push(format!("{line} [×{run_len}]"));
                *collapsed_runs += 1;
            }
        } else {
            for _ in 0..run_len {
                out.push(line.clone());
            }
        }
        i = j;
    }
    out
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
