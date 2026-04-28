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
