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

/// Minimum permitted `max_bytes`. Derived from `2 × MIN_MAX_LINE_BYTES
/// + MARKER_MAX_LEN + LAYOUT_OVERHEAD` so the tail-locked pair always
/// fits.
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
