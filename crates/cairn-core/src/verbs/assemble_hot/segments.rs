//! Pure helper functions for `assemble_hot` segments. Brief §5, §7.

use sha2::{Digest, Sha256};

use crate::generated::verbs::assemble_hot::AssembleHotData;
use crate::generated::verbs::assemble_hot::{HotRecipeStep, HotSegment, SegmentStability};

/// Hard upper bound on `segments.len()` at the wire boundary. Mirrors
/// the schema's `maxItems: 64`. Defends generic decoders against
/// amplification.
pub const MAX_SEGMENTS: usize = 64;

/// Hard upper bound on `data.bytes` (and therefore on every segment
/// `byte_start`/`byte_end`) at the wire boundary. Mirrors the schema's
/// `bytes.maximum: 4194304` (4 MiB). Defends the trust boundary against
/// oversized-allocation / oversized-hash `DoS` payloads.
pub const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Default stability hint per recipe step. Constants, not config.
#[must_use]
pub const fn default_stability(step: HotRecipeStep) -> SegmentStability {
    match step {
        HotRecipeStep::Purpose | HotRecipeStep::Index => SegmentStability::Stable1h,
        HotRecipeStep::PinnedFeedback
        | HotRecipeStep::TopSalienceProject
        | HotRecipeStep::ActivePlaybook => SegmentStability::Stable5m,
        HotRecipeStep::RecentUserSignal => SegmentStability::Volatile,
    }
}

/// Errors returned by [`build_segments`] and the validators in this module.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssembleHotValidationError {
    /// `recipe.len()` and `bodies.len()` disagree.
    #[error("recipe.len() {recipe} != bodies.len() {bodies}")]
    RecipeBodiesLenMismatch {
        /// Number of steps in the recipe slice.
        recipe: usize,
        /// Number of body strings supplied.
        bodies: usize,
    },
    /// A segment's `byte_start` is greater than its `byte_end`.
    #[error("segment {index}: byte_start {start} > byte_end {end}")]
    DescendingRange {
        /// Zero-based segment index.
        index: usize,
        /// The offending `byte_start`.
        start: u64,
        /// The offending `byte_end`.
        end: u64,
    },
    /// A segment's `byte_start` does not equal the previous segment's `byte_end`.
    #[error("segment {index}: byte_start {start} != previous byte_end {prev_end}")]
    NonContiguous {
        /// Zero-based segment index.
        index: usize,
        /// The offending `byte_start`.
        start: u64,
        /// The `byte_end` of the preceding segment.
        prev_end: u64,
    },
    /// The first segment's `byte_start` is not zero.
    #[error("segments[0].byte_start {start} != 0")]
    DoesNotStartAtZero {
        /// The actual `byte_start` of segment 0.
        start: u64,
    },
    /// The last segment's `byte_end` does not equal `prefix.len()`.
    #[error("segments.last().byte_end {end} != prefix.len() {prefix_len}")]
    DoesNotCoverPrefix {
        /// The actual `byte_end` of the last segment.
        end: u64,
        /// `prefix.len()` as a `u64`.
        prefix_len: u64,
    },
    /// `data.bytes` disagrees with `prefix.len()`.
    #[error("data.bytes {bytes} != prefix.len() {prefix_len}")]
    BytesMismatch {
        /// The value of `data.bytes`.
        bytes: u64,
        /// `prefix.len()` as a `u64`.
        prefix_len: u64,
    },
    /// A segment's SHA-256 `content_hash` does not match the corresponding
    /// byte range of `prefix`.
    #[error("segment {index}: content_hash mismatch")]
    HashMismatch {
        /// Zero-based segment index.
        index: usize,
    },
    /// A segment's `byte_end` exceeds the length of `prefix`.
    #[error("segment {index}: byte_end {end} > prefix.len() {prefix_len}")]
    OutOfBounds {
        /// Zero-based segment index.
        index: usize,
        /// The offending `byte_end`.
        end: u64,
        /// `prefix.len()` as a `u64`.
        prefix_len: u64,
    },
    /// The `step` field of a segment does not match the recipe at that
    /// position (checked by [`validate_with_recipe`]).
    #[error("segment {index}: expected step {expected:?}, got {got:?}")]
    StepMismatch {
        /// Zero-based segment index.
        index: usize,
        /// The step the recipe prescribed.
        expected: HotRecipeStep,
        /// The step that was actually present.
        got: HotRecipeStep,
    },
    /// The number of segments does not equal the number of expected recipe
    /// steps (checked by [`validate_with_recipe`]).
    #[error("segments.len() {got} != expected recipe.len() {expected}")]
    RecipeLenMismatch {
        /// Number of steps in the expected recipe.
        expected: usize,
        /// Number of segments in the data.
        got: usize,
    },
    /// `segments` is `None`; recipe-alignment validation requires segments.
    #[error("legacy producer did not emit segments; cannot validate against recipe")]
    LegacyProducerSegmentsAbsent,
    /// `segments` is `Some(vec![])` but `prefix` or `bytes` is non-empty.
    #[error(
        "Some(vec![]) requires prefix == \"\" and bytes == 0, got bytes {bytes}, prefix.len() {prefix_len}"
    )]
    EmptySegmentsRequiresEmptyPrefix {
        /// The value of `data.bytes`.
        bytes: u64,
        /// `prefix.len()` as a `u64`.
        prefix_len: u64,
    },
    /// A segment's stability hint does not match the default for its step.
    #[error(
        "segment {index}: stability {got:?} does not match default for step {step:?} ({expected:?})"
    )]
    StabilityMismatch {
        /// Zero-based segment index.
        index: usize,
        /// The recipe step that owns this segment.
        step: HotRecipeStep,
        /// The stability that was present in the segment.
        got: SegmentStability,
        /// The stability that [`default_stability`] prescribes for this step.
        expected: SegmentStability,
    },
    /// `segments.len()` exceeds [`MAX_SEGMENTS`].
    #[error("segments.len() {got} exceeds maximum {max}")]
    TooManySegments {
        /// Actual segment count.
        got: usize,
        /// Hard upper bound ([`MAX_SEGMENTS`]).
        max: usize,
    },
    /// `data.bytes` (or a segment offset) exceeds [`MAX_BYTES`] (4 MiB).
    #[error("byte length {got} exceeds maximum {max}")]
    BytesExceedsMax {
        /// The offending byte length / offset.
        got: u64,
        /// Hard upper bound ([`MAX_BYTES`]).
        max: u64,
    },
    /// A segment offset falls inside a multi-byte UTF-8 code point in `prefix`.
    /// Slicing on this offset would panic, so the validator rejects up front.
    #[error("segment {index}: offset {offset} is not a UTF-8 char boundary in prefix")]
    NotCharBoundary {
        /// Zero-based segment index.
        index: usize,
        /// The offending byte offset.
        offset: u64,
    },
}

/// Build (`prefix`, `segments`) from a configured recipe and parallel
/// bodies. See §5 of the design spec for the full producer contract.
///
/// ```
/// # use cairn_core::verbs::assemble_hot::{build_segments, default_stability};
/// # use cairn_core::generated::verbs::assemble_hot::{HotRecipeStep, SegmentStability};
/// let recipe = [HotRecipeStep::Purpose, HotRecipeStep::RecentUserSignal];
/// let bodies = ["...", "..."];
/// let (_prefix, segments) = build_segments(&recipe, &bodies).unwrap();
/// for w in segments.windows(2) {
///     if (w[0].stability, w[1].stability)
///        == (SegmentStability::Stable1h, SegmentStability::Volatile)
///     {
///         // fictional: Cache::breakpoint(w[0].byte_end);
///     }
/// }
/// ```
pub fn build_segments(
    recipe: &[HotRecipeStep],
    bodies: &[&str],
) -> Result<(String, Vec<HotSegment>), AssembleHotValidationError> {
    if recipe.len() != bodies.len() {
        return Err(AssembleHotValidationError::RecipeBodiesLenMismatch {
            recipe: recipe.len(),
            bodies: bodies.len(),
        });
    }

    // Amplification guard: reject oversized recipes BEFORE allocating /
    // hashing. The wire-cap check in validate_segments is a safety net;
    // this is the producer-side defense against a malformed config.
    if recipe.len() > MAX_SEGMENTS {
        return Err(AssembleHotValidationError::TooManySegments {
            got: recipe.len(),
            max: MAX_SEGMENTS,
        });
    }

    let total: u64 = bodies.iter().map(|b| b.len() as u64).sum();
    if total > MAX_BYTES {
        return Err(AssembleHotValidationError::BytesExceedsMax {
            got: total,
            max: MAX_BYTES,
        });
    }

    let prefix: String = bodies.iter().copied().collect();
    let mut segments = Vec::with_capacity(recipe.len());
    let mut cursor: u64 = 0;
    for (step, body) in recipe.iter().zip(bodies.iter()) {
        let start = cursor;
        let end = cursor + body.len() as u64;
        let mut hasher = Sha256::new();
        hasher.update(body.as_bytes());
        let content_hash = format!("{:x}", hasher.finalize());
        segments.push(HotSegment {
            step: *step,
            byte_start: start,
            byte_end: end,
            stability: default_stability(*step),
            content_hash,
        });
        cursor = end;
    }
    Ok((prefix, segments))
}

/// Wire invariants of `AssembleHotData` independent of `segments`.
pub fn validate_base(data: &AssembleHotData) -> Result<(), AssembleHotValidationError> {
    if data.bytes > MAX_BYTES {
        return Err(AssembleHotValidationError::BytesExceedsMax {
            got: data.bytes,
            max: MAX_BYTES,
        });
    }
    let prefix_len = data.prefix.len() as u64;
    if data.bytes != prefix_len {
        return Err(AssembleHotValidationError::BytesMismatch {
            bytes: data.bytes,
            prefix_len,
        });
    }
    Ok(())
}

/// Segment-layer invariants. See §5 of the design spec for the full table.
pub fn validate_segments(data: &AssembleHotData) -> Result<(), AssembleHotValidationError> {
    let Some(segments) = &data.segments else {
        return Ok(());
    };
    let prefix_len = data.prefix.len() as u64;

    if segments.is_empty() {
        if !data.prefix.is_empty() || data.bytes != 0 {
            return Err(
                AssembleHotValidationError::EmptySegmentsRequiresEmptyPrefix {
                    bytes: data.bytes,
                    prefix_len,
                },
            );
        }
        return Ok(());
    }

    if prefix_len > MAX_BYTES {
        return Err(AssembleHotValidationError::BytesExceedsMax {
            got: prefix_len,
            max: MAX_BYTES,
        });
    }

    if segments.len() > MAX_SEGMENTS {
        return Err(AssembleHotValidationError::TooManySegments {
            got: segments.len(),
            max: MAX_SEGMENTS,
        });
    }

    // First pass: structural checks (range, contiguity, bounds, starts-at-zero,
    // stability). Hash verification is deferred to the second pass so that
    // structural errors (e.g. DescendingRange) take precedence over hash errors,
    // and coverage (DoesNotCoverPrefix) is checked before attempting hash slices.
    let mut prev_end: u64 = 0;
    for (i, s) in segments.iter().enumerate() {
        if s.byte_start > s.byte_end {
            return Err(AssembleHotValidationError::DescendingRange {
                index: i,
                start: s.byte_start,
                end: s.byte_end,
            });
        }
        if i == 0 && s.byte_start != 0 {
            return Err(AssembleHotValidationError::DoesNotStartAtZero {
                start: s.byte_start,
            });
        }
        if i > 0 && s.byte_start != prev_end {
            return Err(AssembleHotValidationError::NonContiguous {
                index: i,
                start: s.byte_start,
                prev_end,
            });
        }
        if s.byte_end > prefix_len {
            return Err(AssembleHotValidationError::OutOfBounds {
                index: i,
                end: s.byte_end,
                prefix_len,
            });
        }
        // UTF-8 boundary check: a hostile payload can satisfy all numeric
        // checks above and still cut through a multibyte code point. Slicing
        // a non-boundary index would panic in the second pass; reject here.
        #[allow(clippy::cast_possible_truncation)]
        let start_usize = s.byte_start as usize;
        #[allow(clippy::cast_possible_truncation)]
        let end_usize = s.byte_end as usize;
        if !data.prefix.is_char_boundary(start_usize) {
            return Err(AssembleHotValidationError::NotCharBoundary {
                index: i,
                offset: s.byte_start,
            });
        }
        if !data.prefix.is_char_boundary(end_usize) {
            return Err(AssembleHotValidationError::NotCharBoundary {
                index: i,
                offset: s.byte_end,
            });
        }
        let expected_stability = default_stability(s.step);
        if s.stability != expected_stability {
            return Err(AssembleHotValidationError::StabilityMismatch {
                index: i,
                step: s.step,
                got: s.stability,
                expected: expected_stability,
            });
        }
        prev_end = s.byte_end;
    }

    if prev_end != prefix_len {
        return Err(AssembleHotValidationError::DoesNotCoverPrefix {
            end: prev_end,
            prefix_len,
        });
    }

    // Second pass: hash verification (only after structural checks pass).
    for (i, s) in segments.iter().enumerate() {
        // SAFETY on the usize cast: byte_end <= prefix.len() (checked in the
        // structural pass above), so the value always fits in usize.
        #[allow(clippy::cast_possible_truncation)]
        let slice = &data.prefix[s.byte_start as usize..s.byte_end as usize];
        let mut h = Sha256::new();
        h.update(slice.as_bytes());
        let actual_hash = format!("{:x}", h.finalize());
        if s.content_hash != actual_hash {
            return Err(AssembleHotValidationError::HashMismatch { index: i });
        }
    }

    Ok(())
}

/// Convenience: `validate_base` then `validate_segments`.
pub fn validate(data: &AssembleHotData) -> Result<(), AssembleHotValidationError> {
    validate_base(data)?;
    validate_segments(data)?;
    Ok(())
}

/// Like `validate`, plus recipe-alignment assertions.
pub fn validate_with_recipe(
    data: &AssembleHotData,
    expected: &[HotRecipeStep],
) -> Result<(), AssembleHotValidationError> {
    validate(data)?;
    let segments = data
        .segments
        .as_ref()
        .ok_or(AssembleHotValidationError::LegacyProducerSegmentsAbsent)?;
    if segments.len() != expected.len() {
        return Err(AssembleHotValidationError::RecipeLenMismatch {
            expected: expected.len(),
            got: segments.len(),
        });
    }
    for (i, (s, exp)) in segments.iter().zip(expected.iter()).enumerate() {
        if s.step != *exp {
            return Err(AssembleHotValidationError::StepMismatch {
                index: i,
                expected: *exp,
                got: s.step,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
// Truncation casts are safe in tests: byte offsets come from `build_segments`
// which is bounded by the length of in-test strings (always fits in usize).
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::generated::verbs::assemble_hot::HotRecipeStep::*;
    use crate::generated::verbs::assemble_hot::SegmentStability::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn build_segments_empty_recipe() {
        let (prefix, segments) = build_segments(&[], &[]).unwrap();
        assert_eq!(prefix, "");
        assert!(segments.is_empty());
    }

    #[test]
    fn default_stability_matches_brief() {
        assert_eq!(default_stability(Purpose), Stable1h);
        assert_eq!(default_stability(Index), Stable1h);
        assert_eq!(default_stability(PinnedFeedback), Stable5m);
        assert_eq!(default_stability(TopSalienceProject), Stable5m);
        assert_eq!(default_stability(ActivePlaybook), Stable5m);
        assert_eq!(default_stability(RecentUserSignal), Volatile);
    }

    #[test]
    fn build_segments_all_empty_bodies() {
        let recipe = [
            Purpose,
            Index,
            PinnedFeedback,
            TopSalienceProject,
            ActivePlaybook,
            RecentUserSignal,
        ];
        let bodies = ["", "", "", "", "", ""];
        let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
        assert_eq!(prefix, "");
        assert_eq!(segments.len(), 6);
        for s in &segments {
            assert_eq!(s.byte_start, 0);
            assert_eq!(s.byte_end, 0);
        }
    }

    #[test]
    fn build_segments_rejects_len_mismatch() {
        let err = build_segments(&[Purpose, Index], &["a"]).unwrap_err();
        assert_eq!(
            err,
            AssembleHotValidationError::RecipeBodiesLenMismatch {
                recipe: 2,
                bodies: 1
            }
        );
    }

    #[test]
    fn build_segments_aligns_steps_to_recipe() {
        let recipe = [Purpose, Index, RecentUserSignal];
        let bodies = ["p", "i", "r"];
        let (_, segments) = build_segments(&recipe, &bodies).unwrap();
        for (i, expected) in recipe.iter().enumerate() {
            assert_eq!(segments[i].step, *expected);
        }
    }

    #[test]
    fn build_segments_single_slot() {
        let (prefix, segments) = build_segments(&[Purpose], &["hello"]).unwrap();
        assert_eq!(prefix, "hello");
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].byte_start, 0);
        assert_eq!(segments[0].byte_end, 5);
    }

    #[test]
    fn content_hash_matches_segment_bytes() {
        let recipe = [Purpose, Index];
        let bodies = ["alpha", "beta"];
        let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
        for s in &segments {
            let slice = &prefix[s.byte_start as usize..s.byte_end as usize];
            let mut h = Sha256::new();
            h.update(slice.as_bytes());
            assert_eq!(s.content_hash, format!("{:x}", h.finalize()));
        }
    }

    #[test]
    fn build_segments_handles_non_ascii() {
        let body = "héllo·世界";
        let (prefix, segments) = build_segments(&[Purpose], &[body]).unwrap();
        let s = &segments[0];
        assert_eq!(s.byte_end - s.byte_start, body.len() as u64);
        assert_eq!(&prefix[s.byte_start as usize..s.byte_end as usize], body);
        let mut h = Sha256::new();
        h.update(body.as_bytes());
        assert_eq!(s.content_hash, format!("{:x}", h.finalize()));
    }

    fn well_formed_data() -> AssembleHotData {
        let recipe = [Purpose, Index];
        let bodies = ["alpha", "beta"];
        let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
        AssembleHotData {
            bytes: prefix.len() as u64,
            prefix,
            segments: Some(segments),
        }
    }

    #[test]
    fn validate_base_accepts_well_formed() {
        assert!(validate_base(&well_formed_data()).is_ok());
    }

    #[test]
    fn validate_base_rejects_bytes_mismatch() {
        let mut d = well_formed_data();
        d.bytes = 999;
        assert!(matches!(
            validate_base(&d),
            Err(AssembleHotValidationError::BytesMismatch { bytes: 999, .. })
        ));
    }

    #[test]
    fn validate_segments_accepts_canonical_empty() {
        let d = AssembleHotData {
            bytes: 0,
            prefix: String::new(),
            segments: Some(vec![]),
        };
        assert!(validate_segments(&d).is_ok());
    }

    #[test]
    fn validate_segments_rejects_empty_with_non_empty_prefix() {
        let d = AssembleHotData {
            bytes: 3,
            prefix: "abc".into(),
            segments: Some(vec![]),
        };
        assert!(matches!(
            validate_segments(&d),
            Err(
                AssembleHotValidationError::EmptySegmentsRequiresEmptyPrefix {
                    bytes: 3,
                    prefix_len: 3
                }
            )
        ));
    }

    #[test]
    fn validate_segments_accepts_none() {
        let d = AssembleHotData {
            bytes: 0,
            prefix: String::new(),
            segments: None,
        };
        assert!(validate_segments(&d).is_ok());
    }

    #[test]
    fn validate_segments_rejects_descending_range() {
        let mut d = well_formed_data();
        let segs = d.segments.as_mut().unwrap();
        segs[0].byte_start = 10;
        segs[0].byte_end = 5;
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::DescendingRange { .. })
        ));
    }

    #[test]
    fn validate_segments_rejects_non_contiguous() {
        let mut d = well_formed_data();
        let segs = d.segments.as_mut().unwrap();
        segs[1].byte_start += 1;
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::NonContiguous { .. })
        ));
    }

    #[test]
    fn validate_segments_rejects_does_not_start_at_zero() {
        let mut d = well_formed_data();
        d.segments.as_mut().unwrap()[0].byte_start = 1;
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::DoesNotStartAtZero { start: 1 })
        ));
    }

    #[test]
    fn validate_segments_rejects_does_not_cover_prefix() {
        let mut d = well_formed_data();
        let segs = d.segments.as_mut().unwrap();
        let last = segs.last_mut().unwrap();
        last.byte_end -= 1;
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::DoesNotCoverPrefix { .. })
        ));
    }

    #[test]
    fn validate_segments_rejects_hash_mismatch() {
        let mut d = well_formed_data();
        let segs = d.segments.as_mut().unwrap();
        segs[0].content_hash = "0".repeat(64);
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::HashMismatch { index: 0 })
        ));
    }

    #[test]
    fn validate_segments_rejects_out_of_bounds() {
        let mut d = well_formed_data();
        let segs = d.segments.as_mut().unwrap();
        let last = segs.last_mut().unwrap();
        last.byte_end = (d.prefix.len() as u64) + 100;
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn validate_segments_rejects_stability_mismatch() {
        let mut d = well_formed_data();
        d.segments.as_mut().unwrap()[0].stability = Volatile; // Purpose should be Stable1h
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::StabilityMismatch { index: 0, .. })
        ));
    }

    #[test]
    fn validate_base_rejects_bytes_exceeds_max() {
        let d = AssembleHotData {
            bytes: MAX_BYTES + 1,
            prefix: String::new(),
            segments: None,
        };
        assert!(matches!(
            validate_base(&d),
            Err(AssembleHotValidationError::BytesExceedsMax {
                got,
                max: MAX_BYTES,
            }) if got == MAX_BYTES + 1
        ));
    }

    #[test]
    fn validate_segments_rejects_not_char_boundary() {
        // Two-byte UTF-8 char "é" (0xC3 0xA9). A hostile payload claims the
        // first segment ends at offset 1 — the middle of the code point.
        // Hash is set to whatever; structural pass must reject before slicing.
        let prefix = "é".to_string();
        let prefix_len = prefix.len() as u64; // 2
        let s0 = HotSegment {
            step: Purpose,
            byte_start: 0,
            byte_end: 1,
            stability: Stable1h,
            content_hash: "0".repeat(64),
        };
        let s1 = HotSegment {
            step: Index,
            byte_start: 1,
            byte_end: prefix_len,
            stability: Stable1h,
            content_hash: "0".repeat(64),
        };
        let d = AssembleHotData {
            bytes: prefix_len,
            prefix,
            segments: Some(vec![s0, s1]),
        };
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::NotCharBoundary {
                index: 0,
                offset: 1
            })
        ));
    }

    #[test]
    fn validate_segments_rejects_too_many() {
        // build_segments now rejects > MAX_SEGMENTS up front (amplification
        // guard), so construct the oversized AssembleHotData directly to
        // exercise validate_segments' wire-side check.
        let zero_hash = "0".repeat(64);
        let segs: Vec<HotSegment> = (0..65)
            .map(|_| HotSegment {
                step: Purpose,
                byte_start: 0,
                byte_end: 0,
                stability: Stable1h,
                content_hash: zero_hash.clone(),
            })
            .collect();
        let d = AssembleHotData {
            bytes: 0,
            prefix: String::new(),
            segments: Some(segs),
        };
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::TooManySegments { got: 65, max: 64 })
        ));
    }

    #[test]
    fn build_segments_rejects_too_many_recipe_steps() {
        // Producer-side defense: oversized recipes must fail before the
        // assembler allocates / hashes any data.
        let recipe: Vec<HotRecipeStep> = vec![Purpose; 65];
        let bodies: Vec<&str> = vec![""; 65];
        let err = build_segments(&recipe, &bodies).unwrap_err();
        assert!(matches!(
            err,
            AssembleHotValidationError::TooManySegments { got: 65, max: 64 }
        ));
    }

    #[test]
    fn validate_with_recipe_rejects_step_mismatch() {
        let d = well_formed_data();
        let expected = [Purpose, RecentUserSignal];
        assert!(matches!(
            validate_with_recipe(&d, &expected),
            Err(AssembleHotValidationError::StepMismatch { index: 1, .. })
        ));
    }

    #[test]
    fn validate_with_recipe_rejects_recipe_len_mismatch() {
        let d = well_formed_data();
        assert!(matches!(
            validate_with_recipe(&d, &[Purpose]),
            Err(AssembleHotValidationError::RecipeLenMismatch {
                expected: 1,
                got: 2
            })
        ));
    }

    #[test]
    fn validate_with_recipe_rejects_legacy_absent() {
        let d = AssembleHotData {
            bytes: 0,
            prefix: String::new(),
            segments: None,
        };
        assert!(matches!(
            validate_with_recipe(&d, &[]),
            Err(AssembleHotValidationError::LegacyProducerSegmentsAbsent)
        ));
    }

    #[test]
    fn build_segments_output_validates() {
        let recipe = [Purpose, Index, RecentUserSignal];
        let bodies = ["a", "bb", "ccc"];
        let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
        let d = AssembleHotData {
            bytes: prefix.len() as u64,
            prefix,
            segments: Some(segments),
        };
        validate(&d).unwrap();
        validate_with_recipe(&d, &recipe).unwrap();
    }

    use proptest::prelude::*;

    fn step_strategy() -> impl Strategy<Value = HotRecipeStep> {
        prop_oneof![
            Just(Purpose),
            Just(Index),
            Just(PinnedFeedback),
            Just(TopSalienceProject),
            Just(ActivePlaybook),
            Just(RecentUserSignal),
        ]
    }

    proptest! {
        #[test]
        fn coverage_invariant(chunks in proptest::collection::vec((step_strategy(), ".{0,32}"), 0..16)) {
            let recipe: Vec<HotRecipeStep> = chunks.iter().map(|(s, _)| *s).collect();
            let bodies: Vec<&str> = chunks.iter().map(|(_, b)| b.as_str()).collect();
            let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
            if segments.is_empty() {
                prop_assert_eq!(prefix.len(), 0);
            } else {
                prop_assert_eq!(segments[0].byte_start, 0);
                prop_assert_eq!(segments.last().unwrap().byte_end, prefix.len() as u64);
                for w in segments.windows(2) {
                    prop_assert_eq!(w[0].byte_end, w[1].byte_start);
                }
            }
        }

        #[test]
        fn hash_stability(chunks in proptest::collection::vec((step_strategy(), ".{0,32}"), 0..16)) {
            let recipe: Vec<HotRecipeStep> = chunks.iter().map(|(s, _)| *s).collect();
            let bodies: Vec<&str> = chunks.iter().map(|(_, b)| b.as_str()).collect();
            let a = build_segments(&recipe, &bodies).unwrap();
            let b = build_segments(&recipe, &bodies).unwrap();
            prop_assert_eq!(a, b);
        }

        #[test]
        fn utf8_boundary_safety(chunks in proptest::collection::vec((step_strategy(), ".{0,32}"), 0..16)) {
            let recipe: Vec<HotRecipeStep> = chunks.iter().map(|(s, _)| *s).collect();
            let bodies: Vec<&str> = chunks.iter().map(|(_, b)| b.as_str()).collect();
            let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
            for s in &segments {
                let _ = &prefix[s.byte_start as usize..s.byte_end as usize];
            }
        }
    }
}
