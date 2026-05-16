//! Pure session-tree model (brief §5.7).
//!
//! This module owns the storage-agnostic semantics for v0.3 session trees:
//! a v0.1 flat session is represented as a one-node tree, forks/clones carry
//! explicit parentage, and merges are recorded as auditable events. Persistence
//! and `cairn.sessiontree.v1` dispatch are intentionally out of scope here.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::domain::{RecordId, SessionId};

/// Errors raised by the pure session-tree model before any storage mutation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum SessionTreeError {
    /// The requested session id is not present in the tree.
    #[error("session tree: unknown session `{session_id}`")]
    UnknownSession {
        /// Missing session id.
        session_id: SessionId,
    },
    /// A branch tried to insert a session id already present in the tree.
    #[error("session tree: duplicate session `{session_id}`")]
    DuplicateSession {
        /// Duplicate session id.
        session_id: SessionId,
    },
    /// A merge tried to fold a session into itself.
    #[error("session tree: cannot merge session `{session_id}` into itself")]
    SelfMerge {
        /// Reused source/destination session id.
        session_id: SessionId,
    },
    /// Parent/child pointers or node keys are internally inconsistent.
    #[error("session tree: malformed link for `{session_id}`: {message}")]
    MalformedLink {
        /// Session id whose link metadata failed validation.
        session_id: SessionId,
        /// Static validation reason.
        message: &'static str,
    },
    /// A branch boundary string was empty.
    #[error("session tree: `{field}` must not be empty")]
    EmptyField {
        /// Empty field name.
        field: &'static str,
    },
}

/// Relationship from a parent session to a child session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BranchKind {
    /// Copy-on-write child whose history inherits through a named turn.
    Fork,
    /// Full-copy child at the parent's latest turn.
    Clone,
    /// Branch spawned by a tool call from a parent turn.
    ToolSpawned,
}

/// Merge representation for an auditable session-tree merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum MergeStrategy {
    /// Fold the branch outcome back as a reasoning summary record.
    ReasoningSummary {
        /// Record id of the reasoning summary that explains the merge.
        summary_record_id: RecordId,
    },
    /// Splice branch turns into the destination after review.
    ControlledSplice {
        /// First source turn included in the splice.
        first_turn_id: String,
        /// Last source turn included in the splice.
        last_turn_id: String,
    },
}

/// Parent metadata carried by every non-root session node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionParent {
    /// Parent session id.
    pub session_id: SessionId,
    /// Turn boundary where the branch diverged.
    ///
    /// For [`BranchKind::Clone`], this is `"latest"` until the storage layer
    /// has a concrete latest-turn id to stamp.
    pub at_turn_id: String,
    /// Fork or clone relationship.
    pub kind: BranchKind,
    /// Tool call that spawned this branch. Present only for
    /// [`BranchKind::ToolSpawned`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// One node in a session tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTreeNode {
    /// This node's session id.
    pub session_id: SessionId,
    /// Parent metadata. `None` only for the tree root.
    pub parent: Option<SessionParent>,
    /// Direct child sessions in insertion order.
    pub children: Vec<SessionId>,
}

/// Explicit merge event between two existing sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMerge {
    /// Source branch being folded back.
    pub source: SessionId,
    /// Destination trunk/session receiving the merge.
    pub destination: SessionId,
    /// Explicit merge behavior.
    pub strategy: MergeStrategy,
    /// Destination turn where the merge was applied.
    pub applied_at_turn_id: String,
}

/// Storage-agnostic session tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTree {
    root: SessionId,
    nodes: BTreeMap<SessionId, SessionTreeNode>,
    merges: Vec<SessionMerge>,
}

impl SessionTree {
    /// Represent a v0.1 flat session as the simplest one-branch tree.
    #[must_use]
    pub fn flat(root: SessionId) -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            root.clone(),
            SessionTreeNode {
                session_id: root.clone(),
                parent: None,
                children: Vec::new(),
            },
        );
        Self {
            root,
            nodes,
            merges: Vec::new(),
        }
    }

    /// Tree root session id.
    #[must_use]
    pub fn root(&self) -> &SessionId {
        &self.root
    }

    /// Direct parent metadata for `session_id`, or `None` for the root.
    pub fn parent(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<&SessionParent>, SessionTreeError> {
        let node = self
            .nodes
            .get(session_id)
            .ok_or_else(|| SessionTreeError::UnknownSession {
                session_id: session_id.clone(),
            })?;
        Ok(node.parent.as_ref())
    }

    /// Direct children for `session_id`.
    pub fn children(&self, session_id: &SessionId) -> Result<Vec<SessionId>, SessionTreeError> {
        let node = self
            .nodes
            .get(session_id)
            .ok_or_else(|| SessionTreeError::UnknownSession {
                session_id: session_id.clone(),
            })?;
        Ok(node.children.clone())
    }

    /// Return a stable parent-before-children traversal for a subtree.
    pub fn subtree_preorder(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<SessionId>, SessionTreeError> {
        self.require_session(session_id)?;
        let mut out = Vec::new();
        let mut stack = vec![session_id.clone()];
        while let Some(current) = stack.pop() {
            let node =
                self.nodes
                    .get(&current)
                    .ok_or_else(|| SessionTreeError::UnknownSession {
                        session_id: current.clone(),
                    })?;
            out.push(current);
            stack.extend(node.children.iter().rev().cloned());
        }
        Ok(out)
    }

    /// Merge events in append order.
    #[must_use]
    pub fn merges(&self) -> &[SessionMerge] {
        &self.merges
    }

    /// Validate tree invariants after hydration from storage or JSON.
    ///
    /// Constructors maintain these invariants for in-memory writes; this
    /// method protects deserialized snapshots before callers trust lineage.
    pub fn validate(&self) -> Result<(), SessionTreeError> {
        self.validate_root()?;
        self.validate_nodes()?;
        self.validate_all_lineages()?;
        self.validate_merges()?;

        Ok(())
    }

    fn validate_root(&self) -> Result<(), SessionTreeError> {
        let root = self
            .nodes
            .get(&self.root)
            .ok_or_else(|| SessionTreeError::UnknownSession {
                session_id: self.root.clone(),
            })?;
        if root.parent.is_some() {
            return Err(SessionTreeError::MalformedLink {
                session_id: self.root.clone(),
                message: "root must not have a parent",
            });
        }
        if root.session_id != self.root {
            return Err(SessionTreeError::MalformedLink {
                session_id: self.root.clone(),
                message: "root node id must match tree root",
            });
        }
        Ok(())
    }

    fn validate_nodes(&self) -> Result<(), SessionTreeError> {
        for (id, node) in &self.nodes {
            if &node.session_id != id {
                return Err(SessionTreeError::MalformedLink {
                    session_id: id.clone(),
                    message: "node key must match session_id",
                });
            }
            self.validate_child_links(id, node)?;

            if id == &self.root {
                continue;
            }
            self.validate_parent_link(id, node)?;
        }
        Ok(())
    }

    fn validate_child_links(
        &self,
        id: &SessionId,
        node: &SessionTreeNode,
    ) -> Result<(), SessionTreeError> {
        let mut seen_children = BTreeSet::new();
        for child in &node.children {
            if !seen_children.insert(child.clone()) {
                return Err(SessionTreeError::MalformedLink {
                    session_id: id.clone(),
                    message: "children must not contain duplicates",
                });
            }
            let child_node =
                self.nodes
                    .get(child)
                    .ok_or_else(|| SessionTreeError::UnknownSession {
                        session_id: child.clone(),
                    })?;
            let Some(parent) = &child_node.parent else {
                return Err(SessionTreeError::MalformedLink {
                    session_id: child.clone(),
                    message: "child must point back to parent",
                });
            };
            if &parent.session_id != id {
                return Err(SessionTreeError::MalformedLink {
                    session_id: child.clone(),
                    message: "child parent does not match containing node",
                });
            }
        }
        Ok(())
    }

    fn validate_parent_link(
        &self,
        id: &SessionId,
        node: &SessionTreeNode,
    ) -> Result<(), SessionTreeError> {
        let parent = node
            .parent
            .as_ref()
            .ok_or_else(|| SessionTreeError::MalformedLink {
                session_id: id.clone(),
                message: "non-root node must have a parent",
            })?;
        self.require_session(&parent.session_id)?;
        non_empty(parent.at_turn_id.clone(), "at_turn_id")?;
        Self::validate_parent_branch_metadata(id, parent)?;
        let parent_node =
            self.nodes
                .get(&parent.session_id)
                .ok_or_else(|| SessionTreeError::UnknownSession {
                    session_id: parent.session_id.clone(),
                })?;
        if !parent_node.children.iter().any(|child| child == id) {
            return Err(SessionTreeError::MalformedLink {
                session_id: id.clone(),
                message: "parent children must include child",
            });
        }
        Ok(())
    }

    fn validate_parent_branch_metadata(
        id: &SessionId,
        parent: &SessionParent,
    ) -> Result<(), SessionTreeError> {
        match parent.kind {
            BranchKind::ToolSpawned => {
                let tool_call_id =
                    parent
                        .tool_call_id
                        .clone()
                        .ok_or(SessionTreeError::EmptyField {
                            field: "tool_call_id",
                        })?;
                non_empty(tool_call_id, "tool_call_id")?;
            }
            BranchKind::Fork | BranchKind::Clone => {
                if parent.tool_call_id.is_some() {
                    return Err(SessionTreeError::MalformedLink {
                        session_id: id.clone(),
                        message: "tool_call_id is only valid for tool-spawned branches",
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_all_lineages(&self) -> Result<(), SessionTreeError> {
        for id in self.nodes.keys() {
            self.lineage(id)?;
        }
        Ok(())
    }

    fn validate_merges(&self) -> Result<(), SessionTreeError> {
        for merge in &self.merges {
            if merge.source == merge.destination {
                return Err(SessionTreeError::SelfMerge {
                    session_id: merge.source.clone(),
                });
            }
            self.require_session(&merge.source)?;
            self.require_session(&merge.destination)?;
            non_empty(merge.applied_at_turn_id.clone(), "applied_at_turn_id")?;
            validate_merge_strategy(&merge.strategy)?;
        }
        Ok(())
    }

    /// Create a copy-on-write child session from `from` at `at_turn_id`.
    pub fn fork(
        &mut self,
        from: &SessionId,
        child: SessionId,
        at_turn_id: impl Into<String>,
    ) -> Result<(), SessionTreeError> {
        self.add_child(from, child, BranchKind::Fork, at_turn_id, None)
    }

    /// Create a full-copy child session from `from`.
    pub fn clone_session(
        &mut self,
        from: &SessionId,
        child: SessionId,
    ) -> Result<(), SessionTreeError> {
        self.add_child(from, child, BranchKind::Clone, "latest", None)
    }

    /// Create a branch spawned by a tool call from `from` at `at_turn_id`.
    pub fn tool_spawn(
        &mut self,
        from: &SessionId,
        child: SessionId,
        at_turn_id: impl Into<String>,
        tool_call_id: impl Into<String>,
    ) -> Result<(), SessionTreeError> {
        let tool_call_id = non_empty(tool_call_id.into(), "tool_call_id")?;
        self.add_child(
            from,
            child,
            BranchKind::ToolSpawned,
            at_turn_id,
            Some(tool_call_id),
        )
    }

    /// Return the root-to-session lineage, inclusive.
    pub fn lineage(&self, session_id: &SessionId) -> Result<Vec<SessionId>, SessionTreeError> {
        let mut lineage = Vec::new();
        let mut current = session_id.clone();
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(current.clone()) {
                return Err(SessionTreeError::MalformedLink {
                    session_id: session_id.clone(),
                    message: "lineage must not contain cycles",
                });
            }
            let node =
                self.nodes
                    .get(&current)
                    .ok_or_else(|| SessionTreeError::UnknownSession {
                        session_id: current.clone(),
                    })?;
            lineage.push(node.session_id.clone());
            let Some(parent) = &node.parent else {
                break;
            };
            current = parent.session_id.clone();
        }
        lineage.reverse();
        if lineage.first() != Some(&self.root) {
            return Err(SessionTreeError::MalformedLink {
                session_id: session_id.clone(),
                message: "lineage must reach root",
            });
        }
        Ok(lineage)
    }

    /// Append an explicit merge event between two existing sessions.
    pub fn record_merge(
        &mut self,
        source: SessionId,
        destination: SessionId,
        strategy: MergeStrategy,
        applied_at_turn_id: impl Into<String>,
    ) -> Result<SessionMerge, SessionTreeError> {
        if source == destination {
            return Err(SessionTreeError::SelfMerge { session_id: source });
        }
        self.require_session(&source)?;
        self.require_session(&destination)?;
        validate_merge_strategy(&strategy)?;
        let applied_at_turn_id = non_empty(applied_at_turn_id.into(), "applied_at_turn_id")?;
        let merge = SessionMerge {
            source,
            destination,
            strategy,
            applied_at_turn_id,
        };
        self.merges.push(merge.clone());
        Ok(merge)
    }

    fn add_child(
        &mut self,
        from: &SessionId,
        child: SessionId,
        kind: BranchKind,
        at_turn_id: impl Into<String>,
        tool_call_id: Option<String>,
    ) -> Result<(), SessionTreeError> {
        self.require_session(from)?;
        if self.nodes.contains_key(&child) {
            return Err(SessionTreeError::DuplicateSession { session_id: child });
        }
        let at_turn_id = non_empty(at_turn_id.into(), "at_turn_id")?;
        let child_node = SessionTreeNode {
            session_id: child.clone(),
            parent: Some(SessionParent {
                session_id: from.clone(),
                at_turn_id,
                kind,
                tool_call_id,
            }),
            children: Vec::new(),
        };
        self.nodes.insert(child.clone(), child_node);
        let parent = self
            .nodes
            .get_mut(from)
            .ok_or_else(|| SessionTreeError::UnknownSession {
                session_id: from.clone(),
            })?;
        parent.children.push(child);
        Ok(())
    }

    fn require_session(&self, session_id: &SessionId) -> Result<(), SessionTreeError> {
        if self.nodes.contains_key(session_id) {
            Ok(())
        } else {
            Err(SessionTreeError::UnknownSession {
                session_id: session_id.clone(),
            })
        }
    }
}

fn non_empty(value: String, field: &'static str) -> Result<String, SessionTreeError> {
    if value.is_empty() {
        Err(SessionTreeError::EmptyField { field })
    } else {
        Ok(value)
    }
}

fn validate_merge_strategy(strategy: &MergeStrategy) -> Result<(), SessionTreeError> {
    match strategy {
        MergeStrategy::ReasoningSummary { .. } => Ok(()),
        MergeStrategy::ControlledSplice {
            first_turn_id,
            last_turn_id,
        } => {
            non_empty(first_turn_id.clone(), "first_turn_id")?;
            non_empty(last_turn_id.clone(), "last_turn_id")?;
            Ok(())
        }
    }
}
