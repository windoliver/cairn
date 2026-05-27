//! `Index` source: pass-through `index.md` content.

use crate::verbs::assemble_hot::inclusion::LoadedSegment;
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

/// Render the index segment by copying `inputs.index_md` verbatim.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    LoadedSegment {
        body: inputs.index_md.to_owned(),
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

    fn empty_inputs(index: &str) -> HotMemoryInputs<'_> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: index,
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
    fn passes_index_md_through_verbatim() {
        let s = select(&empty_inputs("# Index\n- a.md\n"));
        assert_eq!(s.body, "# Index\n- a.md\n");
    }
}
