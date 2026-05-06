//! Pure helper functions for `assemble_hot` segments. Brief §5, §7.

use crate::generated::verbs::assemble_hot::{HotRecipeStep, HotSegment, SegmentStability};

/// Hard upper bound on `segments.len()` at the wire boundary. Mirrors
/// the schema's `maxItems: 64`. Defends generic decoders against
/// amplification.
pub const MAX_SEGMENTS: usize = 64;

/// Default stability hint per recipe step. Constants, not config.
pub const fn default_stability(step: HotRecipeStep) -> SegmentStability {
    match step {
        HotRecipeStep::Purpose | HotRecipeStep::Index => SegmentStability::Stable1h,
        HotRecipeStep::PinnedFeedback
        | HotRecipeStep::TopSalienceProject
        | HotRecipeStep::ActivePlaybook => SegmentStability::Stable5m,
        HotRecipeStep::RecentUserSignal => SegmentStability::Volatile,
    }
}

/// Errors returned by `build_segments` and the validators in this module.
/// Will grow in Task 6.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssembleHotValidationError {
    #[error("recipe.len() {recipe} != bodies.len() {bodies}")]
    RecipeBodiesLenMismatch { recipe: usize, bodies: usize },
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
    use sha2::{Digest, Sha256};

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

#[cfg(test)]
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
        let recipe = [Purpose, Index, PinnedFeedback, TopSalienceProject, ActivePlaybook, RecentUserSignal];
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
        assert_eq!(err, AssembleHotValidationError::RecipeBodiesLenMismatch { recipe: 2, bodies: 1 });
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
}
