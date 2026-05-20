#![allow(missing_docs)]

use cairn_core::domain::session_tree::{BranchKind, MergeStrategy, SessionTree, SessionTreeError};
use cairn_core::domain::{RecordId, SessionId};

fn sid(raw: &str) -> SessionId {
    SessionId::parse(raw).expect("valid session id")
}

fn rid(raw: &str) -> RecordId {
    RecordId::parse(raw).expect("valid record id")
}

#[test]
fn flat_session_is_one_branch_tree() {
    let root = sid("01JTS6R4J70000000000000000");
    let tree = SessionTree::flat(root.clone());

    assert_eq!(tree.root(), &root);
    assert_eq!(
        tree.lineage(&root).expect("root lineage"),
        vec![root.clone()]
    );
    assert_eq!(tree.parent(&root).expect("root parent"), None);
    assert!(tree.children(&root).expect("root children").is_empty());
    assert!(tree.merges().is_empty());
}

#[test]
fn flat_session_tree_round_trips_as_v0_1_compatible_snapshot() {
    let root = sid("01JTS6R4J70000000000000000");
    let tree = SessionTree::flat(root.clone());

    let value = serde_json::to_value(&tree).expect("serialize flat tree");
    assert_eq!(value["root"], root.as_str());
    assert_eq!(
        value["nodes"][root.as_str()]["session_id"],
        root.as_str(),
        "flat compatibility snapshot keeps the original session id"
    );
    assert_eq!(
        value["nodes"][root.as_str()]["parent"],
        serde_json::Value::Null
    );
    assert_eq!(
        value["nodes"][root.as_str()]["children"],
        serde_json::json!([])
    );
    assert_eq!(value["merges"], serde_json::json!([]));

    let restored: SessionTree = serde_json::from_value(value).expect("restore flat tree");
    restored.validate().expect("restored flat tree validates");
    assert_eq!(
        restored.lineage(&root).expect("restored lineage"),
        vec![root]
    );
}

#[test]
fn fork_preserves_parent_child_lineage() {
    let root = sid("01JTS6R4J70000000000000000");
    let child = sid("01JTS6R4J70000000000000001");
    let grandchild = sid("01JTS6R4J70000000000000002");
    let mut tree = SessionTree::flat(root.clone());

    tree.fork(&root, child.clone(), "turn-4")
        .expect("fork child");
    tree.clone_session(&child, grandchild.clone())
        .expect("clone grandchild");

    assert_eq!(
        tree.lineage(&grandchild).expect("grandchild lineage"),
        vec![root.clone(), child.clone(), grandchild.clone()]
    );
    assert_eq!(
        tree.children(&root).expect("root children"),
        vec![child.clone()]
    );
    assert_eq!(
        tree.parent(&child)
            .expect("child parent")
            .expect("child parent")
            .kind,
        BranchKind::Fork
    );
    assert_eq!(
        tree.parent(&grandchild)
            .expect("grandchild parent")
            .expect("grandchild parent")
            .kind,
        BranchKind::Clone
    );
}

#[test]
fn subtree_preorder_returns_parent_before_children_in_insertion_order() {
    let root = sid("01JTS6R4J70000000000000000");
    let first = sid("01JTS6R4J70000000000000001");
    let second = sid("01JTS6R4J70000000000000002");
    let grandchild = sid("01JTS6R4J70000000000000003");
    let mut tree = SessionTree::flat(root.clone());

    tree.fork(&root, first.clone(), "turn-4")
        .expect("fork first child");
    tree.fork(&root, second.clone(), "turn-5")
        .expect("fork second child");
    tree.tool_spawn(&first, grandchild.clone(), "turn-6", "call-review")
        .expect("tool branch");

    assert_eq!(
        tree.subtree_preorder(&root).expect("root subtree"),
        vec![root.clone(), first.clone(), grandchild.clone(), second]
    );
    assert_eq!(
        tree.subtree_preorder(&first).expect("child subtree"),
        vec![first, grandchild]
    );
}

#[test]
fn tool_spawned_branch_preserves_tool_call_metadata() {
    let root = sid("01JTS6R4J70000000000000000");
    let child = sid("01JTS6R4J70000000000000001");
    let mut tree = SessionTree::flat(root.clone());

    tree.tool_spawn(&root, child.clone(), "turn-4", "call-search")
        .expect("tool spawned branch");

    let parent = tree
        .parent(&child)
        .expect("child parent")
        .expect("child parent");
    assert_eq!(parent.kind, BranchKind::ToolSpawned);
    assert_eq!(parent.at_turn_id, "turn-4");
    assert_eq!(parent.tool_call_id.as_deref(), Some("call-search"));
    assert_eq!(
        tree.lineage(&child).expect("child lineage"),
        vec![root, child]
    );
}

#[test]
fn branch_insertion_rejects_missing_parent_and_duplicate_child() {
    let root = sid("01JTS6R4J70000000000000000");
    let child = sid("01JTS6R4J70000000000000001");
    let missing = sid("01JTS6R4J70000000000000002");
    let mut tree = SessionTree::flat(root.clone());

    let err = tree
        .fork(&missing, child.clone(), "turn-4")
        .expect_err("missing parent rejected");
    assert_eq!(
        err,
        SessionTreeError::UnknownSession {
            session_id: missing.clone()
        }
    );

    tree.fork(&root, child.clone(), "turn-4")
        .expect("first fork");
    let err = tree
        .clone_session(&root, child.clone())
        .expect_err("duplicate child rejected");
    assert_eq!(
        err,
        SessionTreeError::DuplicateSession {
            session_id: child.clone()
        }
    );

    let err = tree
        .subtree_preorder(&missing)
        .expect_err("missing subtree root rejected");
    assert_eq!(
        err,
        SessionTreeError::UnknownSession {
            session_id: missing
        }
    );
}

#[test]
fn merges_are_explicit_and_auditable() {
    let root = sid("01JTS6R4J70000000000000000");
    let branch = sid("01JTS6R4J70000000000000001");
    let summary = rid("01JTS6R4J70000000000000002");
    let mut tree = SessionTree::flat(root.clone());
    tree.fork(&root, branch.clone(), "turn-4")
        .expect("fork branch");

    let merge = tree
        .record_merge(
            branch.clone(),
            root.clone(),
            MergeStrategy::ReasoningSummary {
                summary_record_id: summary.clone(),
            },
            "turn-8",
        )
        .expect("merge branch");

    assert_eq!(merge.source, branch);
    assert_eq!(merge.destination, root);
    assert_eq!(merge.applied_at_turn_id.as_str(), "turn-8");
    assert_eq!(
        merge.strategy,
        MergeStrategy::ReasoningSummary {
            summary_record_id: summary
        }
    );
    assert_eq!(tree.merges(), &[merge]);
}

#[test]
fn controlled_splice_merge_records_source_turn_range() {
    let root = sid("01JTS6R4J70000000000000000");
    let branch = sid("01JTS6R4J70000000000000001");
    let mut tree = SessionTree::flat(root.clone());
    tree.fork(&root, branch.clone(), "turn-4")
        .expect("fork branch");

    let merge = tree
        .record_merge(
            branch.clone(),
            root.clone(),
            MergeStrategy::ControlledSplice {
                first_turn_id: "turn-5".to_owned(),
                last_turn_id: "turn-7".to_owned(),
            },
            "turn-8",
        )
        .expect("controlled splice");

    assert_eq!(merge.source, branch);
    assert_eq!(merge.destination, root);
    assert_eq!(
        merge.strategy,
        MergeStrategy::ControlledSplice {
            first_turn_id: "turn-5".to_owned(),
            last_turn_id: "turn-7".to_owned(),
        }
    );
    tree.validate().expect("controlled splice validates");
}

#[test]
fn controlled_splice_merge_rejects_empty_source_turn_range() {
    let root = sid("01JTS6R4J70000000000000000");
    let branch = sid("01JTS6R4J70000000000000001");
    let mut tree = SessionTree::flat(root.clone());
    tree.fork(&root, branch.clone(), "turn-4")
        .expect("fork branch");

    let err = tree
        .record_merge(
            branch,
            root,
            MergeStrategy::ControlledSplice {
                first_turn_id: String::new(),
                last_turn_id: "turn-7".to_owned(),
            },
            "turn-8",
        )
        .expect_err("empty splice range rejected");

    assert_eq!(
        err,
        SessionTreeError::EmptyField {
            field: "first_turn_id"
        }
    );
}

#[test]
fn merge_rejects_unknown_endpoints_and_self_merge() {
    let root = sid("01JTS6R4J70000000000000000");
    let missing = sid("01JTS6R4J70000000000000001");
    let summary = rid("01JTS6R4J70000000000000002");
    let mut tree = SessionTree::flat(root.clone());

    let err = tree
        .record_merge(
            missing.clone(),
            root.clone(),
            MergeStrategy::ReasoningSummary {
                summary_record_id: summary.clone(),
            },
            "turn-8",
        )
        .expect_err("missing source rejected");
    assert_eq!(
        err,
        SessionTreeError::UnknownSession {
            session_id: missing
        }
    );

    let err = tree
        .record_merge(
            root.clone(),
            root.clone(),
            MergeStrategy::ReasoningSummary {
                summary_record_id: summary,
            },
            "turn-8",
        )
        .expect_err("self merge rejected");
    assert_eq!(err, SessionTreeError::SelfMerge { session_id: root });
}

#[test]
fn validate_accepts_consistent_tree_snapshot() {
    let root = sid("01JTS6R4J70000000000000000");
    let branch = sid("01JTS6R4J70000000000000001");
    let summary = rid("01JTS6R4J70000000000000002");
    let mut tree = SessionTree::flat(root.clone());

    tree.tool_spawn(&root, branch.clone(), "turn-4", "call-search")
        .expect("tool branch");
    tree.record_merge(
        branch,
        root,
        MergeStrategy::ReasoningSummary {
            summary_record_id: summary,
        },
        "turn-8",
    )
    .expect("merge");

    tree.validate().expect("consistent tree validates");
}

#[test]
fn validate_rejects_hydrated_tool_branch_without_tool_call_id() {
    let raw = serde_json::json!({
        "root": "01JTS6R4J70000000000000000",
        "nodes": {
            "01JTS6R4J70000000000000000": {
                "session_id": "01JTS6R4J70000000000000000",
                "parent": null,
                "children": ["01JTS6R4J70000000000000001"]
            },
            "01JTS6R4J70000000000000001": {
                "session_id": "01JTS6R4J70000000000000001",
                "parent": {
                    "session_id": "01JTS6R4J70000000000000000",
                    "at_turn_id": "turn-4",
                    "kind": "tool_spawned"
                },
                "children": []
            }
        },
        "merges": []
    });
    let tree: SessionTree = serde_json::from_value(raw).expect("deserialize malformed tree");

    let err = tree
        .validate()
        .expect_err("tool-spawned branch requires tool call id");
    assert_eq!(
        err,
        SessionTreeError::EmptyField {
            field: "tool_call_id"
        }
    );
}

#[test]
fn validate_rejects_hydrated_parent_cycle_disconnected_from_root() {
    let tree = disconnected_cycle_tree();

    let err = tree
        .validate()
        .expect_err("cyclic lineage must be rejected");
    assert_eq!(
        err,
        SessionTreeError::MalformedLink {
            session_id: sid("01JTS6R4J70000000000000001"),
            message: "lineage must not contain cycles",
        }
    );
}

#[test]
fn lineage_rejects_hydrated_parent_cycle() {
    let tree = disconnected_cycle_tree();

    let err = tree
        .lineage(&sid("01JTS6R4J70000000000000001"))
        .expect_err("cyclic lineage must be rejected");
    assert_eq!(
        err,
        SessionTreeError::MalformedLink {
            session_id: sid("01JTS6R4J70000000000000001"),
            message: "lineage must not contain cycles",
        }
    );
}

fn disconnected_cycle_tree() -> SessionTree {
    let raw = serde_json::json!({
        "root": "01JTS6R4J70000000000000000",
        "nodes": {
            "01JTS6R4J70000000000000000": {
                "session_id": "01JTS6R4J70000000000000000",
                "parent": null,
                "children": []
            },
            "01JTS6R4J70000000000000001": {
                "session_id": "01JTS6R4J70000000000000001",
                "parent": {
                    "session_id": "01JTS6R4J70000000000000002",
                    "at_turn_id": "turn-4",
                    "kind": "fork"
                },
                "children": ["01JTS6R4J70000000000000002"]
            },
            "01JTS6R4J70000000000000002": {
                "session_id": "01JTS6R4J70000000000000002",
                "parent": {
                    "session_id": "01JTS6R4J70000000000000001",
                    "at_turn_id": "turn-5",
                    "kind": "fork"
                },
                "children": ["01JTS6R4J70000000000000001"]
            }
        },
        "merges": []
    });
    serde_json::from_value(raw).expect("deserialize cyclic tree")
}
