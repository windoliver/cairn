//! Pure tree-aware read-window planning for session reads.
//!
//! Brief refs: §5.7 Sessions are trees, §7 Hot Memory. This module is pure:
//! callers pre-authorize records and hydrate [`SessionTree`] before planning.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{RecordId, SessionId, SessionTree, SessionTreeError};

/// Segment category assigned to a selected tree-read record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TreeReadSegmentKind {
    /// Record belongs to the requested branch/session.
    BranchLocal,
    /// Record belongs to an ancestor session in the requested lineage.
    AncestorContext,
    /// Body-free metadata describing a merge summary.
    MergeSummary,
    /// Body-free metadata describing sibling branches.
    SiblingSummary,
}

/// Pre-authorized record available to the read-window planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeReadRecord {
    /// Session that owns the record.
    pub session_id: SessionId,
    /// Optional turn id used for stable ordering inside a session.
    pub turn_id: Option<String>,
    /// Stable record id used as the final ordering tiebreaker.
    pub record_id: RecordId,
    /// Pre-authorized body text considered for the read window.
    pub body: String,
}

/// Budget accounting for a planned read window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeBudgetReport {
    /// Caller-provided body byte budget.
    pub budget_bytes: usize,
    /// Candidate records after lineage filtering, before trimming.
    pub records_in: usize,
    /// Records retained in the final window.
    pub records_out: usize,
    /// Candidate records skipped because of the byte budget.
    pub skipped_for_budget: usize,
    /// Whether any lineage candidate was skipped.
    pub trimmed: bool,
}

/// A selected record annotated with its tree segment role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeReadSelection {
    /// Tree segment role for the selected record.
    pub kind: TreeReadSegmentKind,
    /// Selected pre-authorized record.
    pub record: TreeReadRecord,
}

/// Planned tree-aware read window for a target session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeReadWindow {
    /// Root-to-target lineage, inclusive.
    pub ancestry_path: Vec<SessionId>,
    /// Selected records without segment metadata for compatibility callers.
    pub selected_records: Vec<TreeReadRecord>,
    /// Selected records with segment metadata.
    pub selections: Vec<TreeReadSelection>,
    /// Sibling session ids for body-free policy/debug metadata.
    pub sibling_sessions: Vec<SessionId>,
    /// Body-free merge notes for policy/debug metadata.
    pub merge_notes: Vec<String>,
    /// Budget accounting for the selection.
    pub budget: TreeBudgetReport,
}

/// Input required to plan a tree-aware read window.
#[derive(Debug, Clone, Copy)]
pub struct TreeReadWindowInput<'a> {
    /// Hydrated session tree.
    pub tree: &'a SessionTree,
    /// Session requested by the read surface.
    pub target_session: &'a SessionId,
    /// Pre-authorized candidate records.
    pub records: &'a [TreeReadRecord],
    /// Maximum selected body bytes.
    pub budget_bytes: usize,
}

/// Errors raised while planning a tree-aware read window.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TreeReadWindowError {
    /// Session tree lookup or validation failed.
    #[error(transparent)]
    SessionTree(#[from] SessionTreeError),
}

/// Plan a deterministic read window for `target_session`.
pub fn plan_tree_read_window(
    input: TreeReadWindowInput<'_>,
) -> Result<TreeReadWindow, TreeReadWindowError> {
    let ancestry_path = input.tree.lineage(input.target_session)?;
    let ancestry = ancestry_path.iter().cloned().collect::<BTreeSet<_>>();
    let mut selected = input
        .records
        .iter()
        .filter(|record| ancestry.contains(&record.session_id))
        .map(|record| TreeReadSelection {
            kind: if &record.session_id == input.target_session {
                TreeReadSegmentKind::BranchLocal
            } else {
                TreeReadSegmentKind::AncestorContext
            },
            record: record.clone(),
        })
        .collect::<Vec<_>>();

    selected.sort_by(compare_selection);
    let records_in = selected.len();
    let selected = trim_selections(selected, input.budget_bytes);
    let records_out = selected.len();
    let selected_records = selected
        .iter()
        .map(|selection| selection.record.clone())
        .collect();

    Ok(TreeReadWindow {
        ancestry_path,
        selected_records,
        selections: selected,
        sibling_sessions: siblings(input.tree, input.target_session)?,
        merge_notes: merge_notes(input.tree, input.target_session),
        budget: TreeBudgetReport {
            budget_bytes: input.budget_bytes,
            records_in,
            records_out,
            skipped_for_budget: records_in.saturating_sub(records_out),
            trimmed: records_out < records_in,
        },
    })
}

fn compare_selection(a: &TreeReadSelection, b: &TreeReadSelection) -> std::cmp::Ordering {
    segment_sort_rank(a.kind)
        .cmp(&segment_sort_rank(b.kind))
        .then_with(|| a.record.session_id.cmp(&b.record.session_id))
        .then_with(|| a.record.turn_id.cmp(&b.record.turn_id))
        .then_with(|| a.record.record_id.cmp(&b.record.record_id))
}

fn segment_sort_rank(kind: TreeReadSegmentKind) -> u8 {
    match kind {
        TreeReadSegmentKind::AncestorContext => 0,
        TreeReadSegmentKind::BranchLocal => 1,
        TreeReadSegmentKind::MergeSummary => 2,
        TreeReadSegmentKind::SiblingSummary => 3,
    }
}

fn trim_selections(
    selections: Vec<TreeReadSelection>,
    budget_bytes: usize,
) -> Vec<TreeReadSelection> {
    let mut used = 0usize;
    let mut out = Vec::with_capacity(selections.len());
    for selection in selections {
        let len = selection.record.body.len();
        if used.saturating_add(len) <= budget_bytes {
            used = used.saturating_add(len);
            out.push(selection);
        }
    }
    out
}

fn siblings(tree: &SessionTree, target: &SessionId) -> Result<Vec<SessionId>, TreeReadWindowError> {
    let Some(parent) = tree.parent(target)? else {
        return Ok(Vec::new());
    };
    let mut siblings = tree
        .children(&parent.session_id)?
        .into_iter()
        .filter(|candidate| candidate != target)
        .collect::<Vec<_>>();
    siblings.sort();
    Ok(siblings)
}

fn merge_notes(tree: &SessionTree, target: &SessionId) -> Vec<String> {
    let mut notes = tree
        .merges()
        .iter()
        .filter(|merge| &merge.source == target || &merge.destination == target)
        .map(|merge| {
            format!(
                "source={} destination={} applied_at={}",
                merge.source.as_str(),
                merge.destination.as_str(),
                merge.applied_at_turn_id
            )
        })
        .collect::<Vec<_>>();
    notes.sort();
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::session_tree::SessionTree;
    use crate::domain::{RecordId, SessionId};

    fn sid(raw: &str) -> SessionId {
        SessionId::parse(raw).expect("valid session id")
    }

    fn rid(raw: &str) -> RecordId {
        RecordId::parse(raw).expect("valid record id")
    }

    fn item(session_id: &str, turn_id: &str, record_id: &str, body: &str) -> TreeReadRecord {
        TreeReadRecord {
            session_id: sid(session_id),
            turn_id: Some(turn_id.to_owned()),
            record_id: rid(record_id),
            body: body.to_owned(),
        }
    }

    #[test]
    fn flat_session_selects_branch_local_records_in_turn_order() {
        let root = sid("root");
        let tree = SessionTree::flat(root.clone());
        let records = vec![
            item("root", "turn-2", "01JTS6R4J70000000000000001", "second"),
            item("root", "turn-1", "01JTS6R4J70000000000000002", "first"),
        ];

        let window = plan_tree_read_window(TreeReadWindowInput {
            tree: &tree,
            target_session: &root,
            records: &records,
            budget_bytes: 1024,
        })
        .expect("plan window");

        assert_eq!(window.ancestry_path, vec![root]);
        assert_eq!(
            window
                .selected_records
                .iter()
                .map(|r| r.body.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert!(!window.budget.trimmed);
    }

    #[test]
    fn branch_session_includes_ancestor_then_branch_local_records() {
        let root = sid("root");
        let branch = sid("branch");
        let mut tree = SessionTree::flat(root.clone());
        tree.fork(&root, branch.clone(), "turn-2").expect("fork");
        let records = vec![
            item("branch", "turn-3", "01JTS6R4J70000000000000003", "branch"),
            item("root", "turn-1", "01JTS6R4J70000000000000004", "ancestor"),
        ];

        let window = plan_tree_read_window(TreeReadWindowInput {
            tree: &tree,
            target_session: &branch,
            records: &records,
            budget_bytes: 1024,
        })
        .expect("plan window");

        assert_eq!(window.ancestry_path, vec![root, branch]);
        assert_eq!(
            window
                .selections
                .iter()
                .map(|s| (s.kind, s.record.body.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (TreeReadSegmentKind::AncestorContext, "ancestor"),
                (TreeReadSegmentKind::BranchLocal, "branch"),
            ]
        );
    }

    #[test]
    fn sibling_and_merge_metadata_are_body_free_and_sorted() {
        let root = sid("root");
        let branch_a = sid("branch-a");
        let branch_b = sid("branch-b");
        let branch_c = sid("branch-c");
        let mut tree = SessionTree::flat(root.clone());
        tree.fork(&root, branch_c.clone(), "turn-2")
            .expect("fork c");
        tree.fork(&root, branch_b.clone(), "turn-2")
            .expect("fork b");
        tree.fork(&root, branch_a.clone(), "turn-2")
            .expect("fork a");
        tree.record_merge(
            branch_a.clone(),
            branch_c.clone(),
            crate::domain::MergeStrategy::ControlledSplice {
                first_turn_id: "turn-3".to_owned(),
                last_turn_id: "turn-4".to_owned(),
            },
            "turn-z",
        )
        .expect("merge c");
        tree.record_merge(
            root.clone(),
            branch_a.clone(),
            crate::domain::MergeStrategy::ControlledSplice {
                first_turn_id: "turn-6".to_owned(),
                last_turn_id: "turn-7".to_owned(),
            },
            "turn-a",
        )
        .expect("merge root");
        let records = vec![
            item("branch-a", "turn-3", "01JTS6R4J70000000000000007", "branch body"),
            item(
                "branch-b",
                "turn-3",
                "01JTS6R4J70000000000000008",
                "sibling body",
            ),
            item("root", "turn-1", "01JTS6R4J70000000000000009", "root body"),
        ];

        let window = plan_tree_read_window(TreeReadWindowInput {
            tree: &tree,
            target_session: &branch_a,
            records: &records,
            budget_bytes: 1024,
        })
        .expect("plan window");

        assert_eq!(window.sibling_sessions, vec![branch_b, branch_c]);
        assert_eq!(
            window.merge_notes,
            vec![
                "source=branch-a destination=branch-c applied_at=turn-z".to_owned(),
                "source=root destination=branch-a applied_at=turn-a".to_owned(),
            ]
        );
        let metadata = format!("{:?} {:?}", window.sibling_sessions, window.merge_notes);
        assert!(!metadata.contains("branch body"));
        assert!(!metadata.contains("sibling body"));
        assert!(!metadata.contains("root body"));
    }

    #[test]
    fn budget_trimming_is_deterministic_and_preserves_priority_order() {
        let root = sid("root");
        let branch = sid("branch");
        let mut tree = SessionTree::flat(root.clone());
        tree.fork(&root, branch.clone(), "turn-2").expect("fork");
        let records = vec![
            item(
                "root",
                "turn-1",
                "01JTS6R4J70000000000000005",
                "ancestor-long",
            ),
            item("branch", "turn-3", "01JTS6R4J70000000000000006", "branch"),
        ];
        let shuffled_records = vec![records[1].clone(), records[0].clone()];

        let window = plan_tree_read_window(TreeReadWindowInput {
            tree: &tree,
            target_session: &branch,
            records: &records,
            budget_bytes: "branch".len(),
        })
        .expect("plan window");
        let shuffled_window = plan_tree_read_window(TreeReadWindowInput {
            tree: &tree,
            target_session: &branch,
            records: &shuffled_records,
            budget_bytes: "branch".len(),
        })
        .expect("plan shuffled window");

        let selected_bodies = window
            .selected_records
            .iter()
            .map(|r| r.body.as_str())
            .collect::<Vec<_>>();
        assert_eq!(selected_bodies, vec!["branch"]);
        assert_eq!(
            selected_bodies,
            shuffled_window
                .selected_records
                .iter()
                .map(|r| r.body.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            window
                .selections
                .iter()
                .map(|selection| selection.kind)
                .collect::<Vec<_>>(),
            shuffled_window
                .selections
                .iter()
                .map(|selection| selection.kind)
                .collect::<Vec<_>>()
        );
        assert!(window.budget.trimmed);
        assert_eq!(window.budget.skipped_for_budget, 1);
    }
}
