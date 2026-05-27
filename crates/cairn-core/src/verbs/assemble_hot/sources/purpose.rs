//! `Purpose` source: pass-through `purpose.md` content.

use crate::verbs::assemble_hot::inclusion::LoadedSegment;
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

/// Render the purpose segment by copying `inputs.purpose_md` verbatim.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    LoadedSegment {
        body: inputs.purpose_md.to_owned(),
        included: Vec::new(),
        excluded: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Rfc3339Timestamp;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn empty_inputs(purpose: &str) -> HotMemoryInputs<'_> {
        HotMemoryInputs {
            purpose_md: purpose,
            index_md: "",
            pinned_candidates: &[],
            project_candidates: &[],
            playbook_candidates: &[],
            skill_graph_snapshot: None,
            rolling_summary_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn passes_purpose_md_through_verbatim() {
        let s = select(&empty_inputs("# Purpose\nI am a purpose.\n"));
        assert_eq!(s.body, "# Purpose\nI am a purpose.\n");
        assert!(s.included.is_empty());
        assert!(s.excluded.is_empty());
    }

    #[test]
    fn empty_purpose_emits_empty_segment() {
        let s = select(&empty_inputs(""));
        assert!(s.body.is_empty());
    }
}
