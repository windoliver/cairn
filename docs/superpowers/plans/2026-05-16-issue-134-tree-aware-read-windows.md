# Issue 134 Tree-Aware Read Windows Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a reusable tree-aware read-window planner and wire it into the first read surfaces for issue #134.

**Architecture:** `cairn-core` owns pure planning over `SessionTree` and pre-authorized records. `cairn-cli` loads authorized records from SQLite, calls the planner, and emits existing response shapes plus body-free policy/debug metadata. SQLite remains persistence-only and no public `cairn.sessiontree.v1` verbs are advertised.

**Tech Stack:** Rust 2024, `cairn-core` pure domain module, `cairn-cli` verb wiring, `SqliteMemoryStore` session-tree methods, `cargo nextest`, existing generated `cairn.mcp.v1` envelope types.

---

## File Structure

- Create: `crates/cairn-core/src/domain/tree_read_window.rs`
  - Pure input/output types and deterministic planner.
  - No adapter imports, no async, no filesystem.
- Modify: `crates/cairn-core/src/domain/mod.rs`
  - Export planner types.
- Modify: `crates/cairn-cli/src/verbs/retrieve.rs`
  - Hydrate tree metadata for `retrieve --session`, load authorized records by planned session ids, use selected records, and append `tree.*` policy traces.
- Modify: `crates/cairn-cli/src/verbs/assemble_hot.rs`
  - Use the planner to expand session-scoped `user_signal` loading and add safe `--explain` tree choice notes through existing debug/policy trace surfaces.
- Modify: `crates/cairn-cli/src/verbs/summarize.rs`
  - Reuse the planner when summarize source records share one session id and tree metadata is available.
- Test: unit tests inside `crates/cairn-core/src/domain/tree_read_window.rs`
  - Pure planner behavior.
- Test: existing CLI integration tests under `crates/cairn-cli/tests/`
  - `retrieve --session` tree fixture and `assemble_hot --session --explain` metadata fixture if current helpers make this practical without broad fixture rewrites.

---

### Task 1: Add Core Planner Types

**Files:**
- Create: `crates/cairn-core/src/domain/tree_read_window.rs`
- Modify: `crates/cairn-core/src/domain/mod.rs`

- [ ] **Step 1: Write the failing flat-session compatibility test**

Add this test module to the new file first:

```rust
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
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p cairn-core flat_session_selects_branch_local_records_in_turn_order
```

Expected: compile failure because `tree_read_window`, `TreeReadRecord`, `TreeReadWindowInput`, and `plan_tree_read_window` do not exist yet.

- [ ] **Step 3: Add the minimal planner API**

Create `crates/cairn-core/src/domain/tree_read_window.rs`:

```rust
//! Pure tree-aware read-window planning for session reads.
//!
//! Brief refs: §5.7 Sessions are trees, §7 Hot Memory. This module is pure:
//! callers pre-authorize records and hydrate [`SessionTree`] before planning.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{RecordId, SessionId, SessionTree, SessionTreeError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TreeReadSegmentKind {
    BranchLocal,
    AncestorContext,
    MergeSummary,
    SiblingSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeReadRecord {
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub record_id: RecordId,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeBudgetReport {
    pub budget_bytes: usize,
    pub records_in: usize,
    pub records_out: usize,
    pub skipped_for_budget: usize,
    pub trimmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeReadSelection {
    pub kind: TreeReadSegmentKind,
    pub record: TreeReadRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeReadWindow {
    pub ancestry_path: Vec<SessionId>,
    pub selected_records: Vec<TreeReadRecord>,
    pub selections: Vec<TreeReadSelection>,
    pub sibling_sessions: Vec<SessionId>,
    pub merge_notes: Vec<String>,
    pub budget: TreeBudgetReport,
}

pub struct TreeReadWindowInput<'a> {
    pub tree: &'a SessionTree,
    pub target_session: &'a SessionId,
    pub records: &'a [TreeReadRecord],
    pub budget_bytes: usize,
}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TreeReadWindowError {
    #[error(transparent)]
    SessionTree(#[from] SessionTreeError),
}

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
    let selected_records = selected.iter().map(|s| s.record.clone()).collect();

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
    a.kind
        .cmp(&b.kind)
        .then_with(|| a.record.session_id.cmp(&b.record.session_id))
        .then_with(|| a.record.turn_id.cmp(&b.record.turn_id))
        .then_with(|| a.record.record_id.cmp(&b.record.record_id))
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

fn siblings(
    tree: &SessionTree,
    target: &SessionId,
) -> Result<Vec<SessionId>, TreeReadWindowError> {
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
```

Add the module export in `crates/cairn-core/src/domain/mod.rs`:

```rust
pub mod tree_read_window;

pub use tree_read_window::{
    TreeBudgetReport, TreeReadRecord, TreeReadSegmentKind, TreeReadSelection, TreeReadWindow,
    TreeReadWindowError, TreeReadWindowInput, plan_tree_read_window,
};
```

Remove unused imports if the compiler reports them.

- [ ] **Step 4: Run the test to verify it passes**

Run:

```bash
cargo test -p cairn-core flat_session_selects_branch_local_records_in_turn_order
```

Expected: one matching test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/tree_read_window.rs crates/cairn-core/src/domain/mod.rs
git commit -m "feat(core): add tree read window planner"
```

---

### Task 2: Add Lineage, Sibling, Merge, and Budget Tests

**Files:**
- Modify: `crates/cairn-core/src/domain/tree_read_window.rs`

- [ ] **Step 1: Write failing tests for tree behavior**

Append these tests to the existing test module:

```rust
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
            (TreeReadSegmentKind::BranchLocal, "branch"),
            (TreeReadSegmentKind::AncestorContext, "ancestor"),
        ]
    );
}

#[test]
fn sibling_and_merge_metadata_are_body_free_and_sorted() {
    let root = sid("root");
    let branch_a = sid("branch-a");
    let branch_b = sid("branch-b");
    let mut tree = SessionTree::flat(root.clone());
    tree.fork(&root, branch_b.clone(), "turn-2").expect("fork b");
    tree.fork(&root, branch_a.clone(), "turn-2").expect("fork a");
    tree.record_merge(
        branch_a.clone(),
        root.clone(),
        crate::domain::MergeStrategy::ControlledSplice {
            first_turn_id: "turn-3".to_owned(),
            last_turn_id: "turn-4".to_owned(),
        },
        "turn-5",
    )
    .expect("merge");

    let window = plan_tree_read_window(TreeReadWindowInput {
        tree: &tree,
        target_session: &branch_a,
        records: &[],
        budget_bytes: 1024,
    })
    .expect("plan window");

    assert_eq!(window.sibling_sessions, vec![branch_b]);
    assert_eq!(window.merge_notes.len(), 1);
    assert!(window.merge_notes[0].contains("applied_at=turn-5"));
    assert!(!window.merge_notes[0].contains("branch body"));
}

#[test]
fn budget_trimming_is_deterministic_and_preserves_priority_order() {
    let root = sid("root");
    let branch = sid("branch");
    let mut tree = SessionTree::flat(root.clone());
    tree.fork(&root, branch.clone(), "turn-2").expect("fork");
    let records = vec![
        item("root", "turn-1", "01JTS6R4J70000000000000005", "ancestor-long"),
        item("branch", "turn-3", "01JTS6R4J70000000000000006", "branch"),
    ];

    let window = plan_tree_read_window(TreeReadWindowInput {
        tree: &tree,
        target_session: &branch,
        records: &records,
        budget_bytes: "branch".len(),
    })
    .expect("plan window");

    assert_eq!(
        window.selected_records.iter().map(|r| r.body.as_str()).collect::<Vec<_>>(),
        vec!["branch"]
    );
    assert!(window.budget.trimmed);
    assert_eq!(window.budget.skipped_for_budget, 1);
}
```

- [ ] **Step 2: Run the tests to verify failures where behavior is incomplete**

Run:

```bash
cargo test -p cairn-core tree_read_window
```

Expected before implementation adjustments: at least one assertion fails if Task 1 sorted ancestor before branch-local or did not trim by priority. If all pass because Task 1 already implemented these semantics, continue and treat this as the red check for currently covered behavior.

- [ ] **Step 3: Adjust planner ordering and trim behavior if needed**

Ensure `compare_selection` keeps priority order:

```rust
fn compare_selection(a: &TreeReadSelection, b: &TreeReadSelection) -> std::cmp::Ordering {
    a.kind
        .cmp(&b.kind)
        .then_with(|| a.record.session_id.cmp(&b.record.session_id))
        .then_with(|| a.record.turn_id.cmp(&b.record.turn_id))
        .then_with(|| a.record.record_id.cmp(&b.record.record_id))
}
```

Ensure the enum declaration order stays:

```rust
pub enum TreeReadSegmentKind {
    BranchLocal,
    AncestorContext,
    MergeSummary,
    SiblingSummary,
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run:

```bash
cargo test -p cairn-core tree_read_window
```

Expected: all `tree_read_window` tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/domain/tree_read_window.rs
git commit -m "test(core): cover tree read window semantics"
```

---

### Task 3: Wire Tree Windows Into `retrieve --session`

**Files:**
- Modify: `crates/cairn-cli/src/verbs/retrieve.rs`

- [ ] **Step 1: Write the failing CLI-local unit test**

Add a `#[cfg(test)] mod tree_window_tests` near the bottom of `retrieve.rs` with a focused helper test that does not require a full CLI process:

```rust
#[cfg(test)]
mod tree_window_tests {
    use super::*;
    use cairn_core::domain::{RecordId, SessionId, TreeReadRecord};

    fn sid(raw: &str) -> SessionId {
        SessionId::parse(raw).expect("valid session id")
    }

    fn rid(raw: &str) -> RecordId {
        RecordId::parse(raw).expect("valid record id")
    }

    #[test]
    fn tree_records_from_memory_records_preserve_trace_turn_ids() {
        let mut record = cairn_core::domain::record::tests_export::sample_record();
        record.id = rid("01JTS6R4J70000000000000007");
        record.scope.session_id = Some("branch".to_owned());
        record.body = "body".to_owned();
        record.extra_frontmatter.insert(
            "trace".to_owned(),
            serde_json::json!({ "turn_id": "turn-9" }),
        );

        let got = tree_read_records_from_memory_records(&[record]);

        assert_eq!(
            got,
            vec![TreeReadRecord {
                session_id: sid("branch"),
                turn_id: Some("turn-9".to_owned()),
                record_id: rid("01JTS6R4J70000000000000007"),
                body: "body".to_owned(),
            }]
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p cairn-cli tree_records_from_memory_records_preserve_trace_turn_ids
```

Expected: compile failure because `tree_read_records_from_memory_records` does not exist.

- [ ] **Step 3: Add conversion and policy-trace helpers**

In `retrieve.rs`, import the planner types:

```rust
use cairn_core::domain::{
    Identity, MemoryKind, MemoryRecord, RecordId, ScopeTuple, SessionId, TreeBudgetReport,
    TreeReadRecord, TreeReadWindow,
};
```

Add helpers:

```rust
fn tree_read_records_from_memory_records(records: &[MemoryRecord]) -> Vec<TreeReadRecord> {
    records
        .iter()
        .filter_map(|record| {
            let session_id = record.scope.session_id.as_ref()?;
            let session_id = SessionId::parse(session_id.clone()).ok()?;
            Some(TreeReadRecord {
                session_id,
                turn_id: trace_turn_id(record),
                record_id: record.id.clone(),
                body: record.body.clone(),
            })
        })
        .collect()
}

fn tree_trace_entries(window: &TreeReadWindow) -> Vec<ResponsePolicyTrace> {
    vec![
        ResponsePolicyTrace {
            detail: Some(format!(
                "path={} selected_records={} skipped_for_budget={}",
                window
                    .ancestry_path
                    .iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(">"),
                window.budget.records_out,
                window.budget.skipped_for_budget,
            )),
            gate: "tree.lineage".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
        ResponsePolicyTrace {
            detail: Some(format!("siblings={}", window.sibling_sessions.len())),
            gate: "tree.sibling_context".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
        ResponsePolicyTrace {
            detail: Some(format!("merges={}", window.merge_notes.len())),
            gate: "tree.merge_context".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
    ]
}
```

- [ ] **Step 4: Run the focused test to verify it passes**

Run:

```bash
cargo test -p cairn-cli tree_records_from_memory_records_preserve_trace_turn_ids
```

Expected: the test passes.

- [ ] **Step 5: Wire `retrieve_session` to load lineage records and plan**

Inside `retrieve_session`, after parsing `order` and `start`, replace the single-session load with a tree-aware path:

```rust
let target_session_id = match SessionId::parse(session_id.clone()) {
    Ok(id) => id,
    Err(e) => return super::signed::rejected_from_domain(ResponseVerb::Retrieve, e),
};
let tree = match store.get_session_tree(&target_session_id).await {
    Ok(tree) => tree,
    Err(e) if e.to_string().contains("capability unavailable") => None,
    Err(e) => return super::signed::aborted(ResponseVerb::Retrieve, format!("session tree: {e}")),
};

let mut records = if let Some(tree) = tree.as_ref() {
    let lineage = match tree.lineage(&target_session_id) {
        Ok(lineage) => lineage,
        Err(e) => return super::signed::aborted(ResponseVerb::Retrieve, format!("session tree: {e}")),
    };
    let mut out = Vec::new();
    for lineage_session in lineage {
        let mut session_args = scoped_list_args(auth);
        if let Some(scope) = &mut session_args.scope {
            scope.session_id = Some(lineage_session.as_str().to_owned());
        }
        match list_records(store, session_args).await {
            Ok(mut page_records) => out.append(&mut page_records),
            Err(resp) => return resp,
        }
    }
    out
} else {
    let mut args = scoped_list_args(auth);
    if let Some(scope) = &mut args.scope {
        scope.session_id = Some(session_id.clone());
    }
    match list_records(store, args).await {
        Ok(records) => records,
        Err(resp) => return resp,
    }
};
```

Before grouping, apply the planner when `tree.is_some()`:

```rust
let mut tree_trace = Vec::new();
if let Some(tree) = tree.as_ref() {
    let tree_records = tree_read_records_from_memory_records(&records);
    match cairn_core::domain::plan_tree_read_window(cairn_core::domain::TreeReadWindowInput {
        tree,
        target_session: &target_session_id,
        records: &tree_records,
        budget_bytes: read_budget_chars,
    }) {
        Ok(window) => {
            let selected_ids = window
                .selected_records
                .iter()
                .map(|record| record.record_id.clone())
                .collect::<std::collections::BTreeSet<_>>();
            records.retain(|record| selected_ids.contains(&record.id));
            tree_trace = tree_trace_entries(&window);
        }
        Err(e) => {
            return super::signed::aborted(
                ResponseVerb::Retrieve,
                format!("tree read window: {e}"),
            );
        }
    }
} else {
    records.retain(|record| record.scope.session_id.as_deref() == Some(session_id.as_str()));
}
```

Thread `tree_trace` into `committed_after_access` by adding an optional `extra_trace` parameter, or append it after building the response:

```rust
let mut resp = committed_after_access(...).await;
resp.policy_trace.extend(tree_trace);
resp
```

- [ ] **Step 6: Run retrieve tests**

Run:

```bash
cargo test -p cairn-cli retrieve_session
```

Expected: tests compile and any matching retrieve session tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/verbs/retrieve.rs
git commit -m "feat(cli): plan tree-aware session retrieval"
```

---

### Task 4: Add Retrieve Window Response Coverage

**Files:**
- Modify: `crates/cairn-cli/src/verbs/retrieve.rs`

- [ ] **Step 1: Write a failing response-shaping test**

Extend the `tree_window_tests` module from Task 3:

```rust
#[test]
fn tree_trace_entries_explain_path_without_record_bodies() {
    let root = sid("root");
    let branch = sid("branch");
    let mut tree = cairn_core::domain::SessionTree::flat(root.clone());
    tree.fork(&root, branch.clone(), "turn-2").expect("fork");
    let records = vec![
        TreeReadRecord {
            session_id: root.clone(),
            turn_id: Some("turn-1".to_owned()),
            record_id: rid("01JTS6R4J70000000000000008"),
            body: "ancestor secret body".to_owned(),
        },
        TreeReadRecord {
            session_id: branch.clone(),
            turn_id: Some("turn-3".to_owned()),
            record_id: rid("01JTS6R4J70000000000000009"),
            body: "branch secret body".to_owned(),
        },
    ];
    let window = cairn_core::domain::plan_tree_read_window(
        cairn_core::domain::TreeReadWindowInput {
            tree: &tree,
            target_session: &branch,
            records: &records,
            budget_bytes: 1024,
        },
    )
    .expect("window");

    let entries = tree_trace_entries(&window);

    assert!(entries.iter().any(|entry| entry.gate == "tree.lineage"));
    let joined = entries
        .iter()
        .filter_map(|entry| entry.detail.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("path=root>branch"));
    assert!(!joined.contains("secret body"));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p cairn-cli tree_trace_entries_explain_path_without_record_bodies
```

Expected: compile failure if Task 3 did not expose `tree_trace_entries`; otherwise failure if the detail leaks record body text or omits the lineage path.

- [ ] **Step 3: Update `tree_trace_entries` if needed**

Ensure the lineage detail uses only session ids and counts:

```rust
detail: Some(format!(
    "path={} selected_records={} skipped_for_budget={}",
    window
        .ancestry_path
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<_>>()
        .join(">"),
    window.budget.records_out,
    window.budget.skipped_for_budget,
)),
```

- [ ] **Step 4: Run the test to verify it passes**

Run:

```bash
cargo test -p cairn-cli tree_trace_entries_explain_path_without_record_bodies
```

Expected: test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/retrieve.rs
git commit -m "test(cli): cover tree-aware retrieve metadata"
```

---

### Task 5: Reuse Planner For Session-Scoped Hot Memory

**Files:**
- Modify: `crates/cairn-cli/src/verbs/assemble_hot.rs`

- [ ] **Step 1: Write failing helper test**

Add a test module near the bottom of `assemble_hot.rs`:

```rust
#[cfg(test)]
mod tree_hot_tests {
    use super::*;

    #[test]
    fn tree_policy_detail_is_metadata_only() {
        let detail = tree_policy_detail(2, 1, 3);
        assert_eq!(detail, "path_sessions=2 siblings=1 merges=3");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
cargo test -p cairn-cli tree_policy_detail_is_metadata_only
```

Expected: compile failure because `tree_policy_detail` does not exist.

- [ ] **Step 3: Add metadata helper and planner-backed session expansion**

Add:

```rust
fn tree_policy_detail(path_sessions: usize, siblings: usize, merges: usize) -> String {
    format!("path_sessions={path_sessions} siblings={siblings} merges={merges}")
}
```

In `load_records_for_kinds`, when `session_id.is_some()` and `kind == MemoryKind::UserSignal`, hydrate `get_session_tree`, compute lineage, and query user-signal records for each lineage session. Keep the existing single-session path for capability-unavailable stores.

The new branch should preserve authorization by calling existing `list_records_for_visibility` for each lineage session rather than directly querying the DB.

- [ ] **Step 4: Append policy trace metadata**

In `read_policy_trace` or immediately after `load_hot_bodies`, append a `ResponsePolicyTrace` when tree metadata was used:

```rust
ResponsePolicyTrace {
    detail: Some(tree_policy_detail(path_sessions, siblings, merges)),
    gate: "tree.branch_context".to_owned(),
    result: ResponsePolicyTraceResult::Pass,
}
```

Do not include raw record bodies, turn text, or unauthorized ids in the detail.

- [ ] **Step 5: Run focused tests**

Run:

```bash
cargo test -p cairn-cli tree_policy_detail_is_metadata_only
cargo test -p cairn-cli assemble_hot
```

Expected: focused helper test and existing assemble-hot tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/assemble_hot.rs
git commit -m "feat(cli): add tree context to hot memory reads"
```

---

### Task 6: Reuse Planner For Summarize Source Selection

**Files:**
- Modify: `crates/cairn-cli/src/verbs/summarize.rs`

- [ ] **Step 1: Inspect current summarize source loading**

Run:

```bash
sed -n '1,260p' crates/cairn-cli/src/verbs/summarize.rs
```

Confirm where source `MemoryRecord` values are loaded before
`cairn_core::verbs::summarize::render_summary_data`.

- [ ] **Step 2: Write failing helper test**

Add a local unit test in `summarize.rs` for a helper that extracts one common session id:

```rust
#[cfg(test)]
mod tree_summary_tests {
    use super::*;

    #[test]
    fn common_session_id_requires_all_records_to_match() {
        let mut a = cairn_core::domain::record::tests_export::sample_record();
        let mut b = cairn_core::domain::record::tests_export::sample_record();
        a.scope.session_id = Some("session-a".to_owned());
        b.scope.session_id = Some("session-a".to_owned());
        assert_eq!(common_session_id(&[a.clone(), b]).as_deref(), Some("session-a"));

        let mut c = a;
        c.scope.session_id = Some("session-c".to_owned());
        assert!(common_session_id(&[c]).is_some());
    }
}
```

If the desired behavior is "all records match or no tree planning", add a second assertion with two different session ids returning `None`:

```rust
let mut d = cairn_core::domain::record::tests_export::sample_record();
d.scope.session_id = Some("session-d".to_owned());
assert!(common_session_id(&[c, d]).is_none());
```

- [ ] **Step 3: Run the test to verify it fails**

Run:

```bash
cargo test -p cairn-cli common_session_id_requires_all_records_to_match
```

Expected: compile failure because `common_session_id` does not exist.

- [ ] **Step 4: Add helper and planner use**

Add:

```rust
fn common_session_id(records: &[MemoryRecord]) -> Option<String> {
    let mut ids = records
        .iter()
        .filter_map(|record| record.scope.session_id.as_deref());
    let first = ids.next()?.to_owned();
    ids.all(|id| id == first).then_some(first)
}
```

After source records are authorized and loaded, if `common_session_id` returns a session id and `get_session_tree` succeeds, run `plan_tree_read_window` with those records converted to `TreeReadRecord`. Retain only selected record ids before calling `render_summary_data`.

If `get_session_tree` returns capability unavailable, keep existing behavior. If it returns malformed tree metadata, abort the summarize response.

- [ ] **Step 5: Run summarize tests**

Run:

```bash
cargo test -p cairn-cli common_session_id_requires_all_records_to_match
cargo test -p cairn-cli summarize
```

Expected: focused helper and existing summarize tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/src/verbs/summarize.rs
git commit -m "feat(cli): apply tree windows to summarize"
```

---

### Task 7: Targeted Verification

**Files:**
- No source changes unless verification exposes a defect.

- [ ] **Step 1: Run core planner tests**

Run:

```bash
cargo test -p cairn-core tree_read_window
```

Expected: all tree-read-window tests pass.

- [ ] **Step 2: Run session-tree store tests**

Run:

```bash
cargo test -p cairn-store-sqlite sessions
```

Expected: existing #133 session tests pass.

- [ ] **Step 3: Run CLI focused tests**

Run:

```bash
cargo test -p cairn-cli retrieve
cargo test -p cairn-cli assemble_hot
cargo test -p cairn-cli summarize
```

Expected: focused CLI tests pass.

- [ ] **Step 4: Run workspace check**

Run:

```bash
cargo check --workspace --all-targets --locked
```

Expected: exit code 0.

- [ ] **Step 5: Run core boundary check**

Run:

```bash
./scripts/check-core-boundary.sh
```

Expected: exit code 0 and no `cairn-core` dependency violations.

- [ ] **Step 6: Commit verification fixes if any**

If any command fails, fix only the defect exposed by that command and rerun the failing command. Commit the fix:

```bash
git add <changed-files>
git commit -m "fix: stabilize tree-aware read windows"
```

---

## Plan Self-Review

- Spec coverage: Tasks 1-2 implement the pure planner, Task 3-4 cover `retrieve`, Task 5 covers `assemble_hot`, Task 6 covers `summarize`, Task 7 covers verification.
- Trace canvas remains out of scope and has no storage/workflow tasks here.
- No IDL change is planned. If implementation proves existing metadata surfaces are insufficient, stop and write a small IDL amendment plan before touching generated code.
- TDD order is explicit for every behavior-changing task.
