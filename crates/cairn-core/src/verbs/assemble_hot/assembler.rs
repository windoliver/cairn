//! Stub-body `HotMemoryAssembler`. Walks `HotMemoryConfig.recipe`,
//! calls a stub `load_step_body` returning `""` for every step, and
//! returns a fully validated `AssembleHotData`. Real source loading is
//! the missing-half of issue #193 — that PR replaces `load_step_body`
//! and changes nothing else.

use super::segments::{AssembleHotValidationError, build_segments};
use crate::config::HotMemoryConfig;
use crate::generated::verbs::assemble_hot::{AssembleHotData, HotRecipeStep};

/// Errors returned by [`assemble_hot`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AssembleHotError {
    /// Segment construction or validation failed.
    #[error("segment construction: {0}")]
    Segments(#[from] AssembleHotValidationError),
}

/// Run the hot-memory recipe and return a validated `AssembleHotData`.
pub fn assemble_hot(config: &HotMemoryConfig) -> Result<AssembleHotData, AssembleHotError> {
    let recipe: Vec<HotRecipeStep> = config
        .recipe
        .iter()
        .copied()
        .map(HotRecipeStep::from)
        .collect();
    let bodies: Vec<String> = recipe.iter().copied().map(load_step_body).collect();
    let bodies_refs: Vec<&str> = bodies.iter().map(String::as_str).collect();
    let (prefix, segments) = build_segments(&recipe, &bodies_refs)?;
    Ok(AssembleHotData {
        bytes: prefix.len() as u64,
        prefix,
        segments: Some(segments),
    })
}

/// Load the body for one recipe step. Stub: always `""`. The
/// missing-half of issue #193 replaces this single function with the real
/// `SQLite` + markdown loader; nothing else here changes.
fn load_step_body(_step: HotRecipeStep) -> String {
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotMemoryConfig;

    #[test]
    fn assemble_hot_default_config_returns_six_zero_length_segments() {
        let cfg = HotMemoryConfig::default();
        let data = assemble_hot(&cfg).unwrap();
        assert_eq!(data.prefix, "");
        assert_eq!(data.bytes, 0);
        let segments = data.segments.expect("segments emitted");
        assert_eq!(segments.len(), cfg.recipe.len());
        for s in &segments {
            assert_eq!(s.byte_start, 0);
            assert_eq!(s.byte_end, 0);
        }
    }

    #[test]
    fn assemble_hot_empty_recipe() {
        let mut cfg = HotMemoryConfig::default();
        cfg.recipe.clear();
        let data = assemble_hot(&cfg).unwrap();
        assert_eq!(data.prefix, "");
        assert_eq!(data.segments, Some(vec![]));
    }

    #[test]
    fn assemble_hot_output_round_trips_through_deserialize() {
        let cfg = HotMemoryConfig::default();
        let data = assemble_hot(&cfg).unwrap();
        let json = serde_json::to_string(&data).unwrap();
        let back: crate::generated::verbs::assemble_hot::AssembleHotData =
            serde_json::from_str(&json).unwrap();
        assert_eq!(back, data);
    }
}
