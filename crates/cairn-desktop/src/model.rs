//! JSON DTOs used by the desktop GUI alpha.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// SRE report shown by the desktop dashboard.
pub type DesktopSreReport = cairn_core::domain::SreReport;

/// Summary of the loaded desktop vault.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopVaultSummary {
    /// Stable vault id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Human-readable root path or fixture label.
    pub root: String,
    /// Number of records available to inspect.
    pub record_count: usize,
    /// Number of folders available to inspect.
    pub folder_count: usize,
}

/// Folder shown in the vault inspector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopFolder {
    /// Stable folder id.
    pub id: String,
    /// Folder display name.
    pub name: String,
    /// Parent folder id, when nested.
    pub parent_id: Option<String>,
}

/// Record summary shown in lists and graph nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRecordSummary {
    /// Stable record id.
    pub id: String,
    /// Record title.
    pub title: String,
    /// Owning folder id.
    pub folder_id: String,
    /// Record kind.
    pub kind: String,
    /// Tags projected for the GUI.
    pub tags: Vec<String>,
    /// Optimistic record version.
    pub version: u64,
    /// Confidence score displayed by the inspector.
    pub confidence: f64,
}

/// Full record detail shown in the editor pane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRecordDetail {
    /// Stable record id.
    pub id: String,
    /// Record title.
    pub title: String,
    /// Owning folder id.
    pub folder_id: String,
    /// Markdown body.
    pub body: String,
    /// Record kind.
    pub kind: String,
    /// Tags projected for the GUI.
    pub tags: Vec<String>,
    /// Optimistic record version.
    pub version: u64,
    /// Backend projection hash.
    pub backend_hash: String,
    /// Confidence score displayed by the inspector.
    pub confidence: f64,
    /// Source hash displayed by the inspector.
    pub source_hash: String,
    /// Linked record ids.
    pub links: Vec<String>,
}

/// Derived graph response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopGraph {
    /// Graph nodes.
    pub nodes: Vec<DesktopGraphNode>,
    /// Graph edges.
    pub edges: Vec<DesktopGraphEdge>,
}

/// Derived graph node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopGraphNode {
    /// Node id.
    pub id: String,
    /// Display label.
    pub label: String,
    /// Record kind.
    pub kind: String,
    /// Optional group or folder id.
    pub group: String,
}

/// Derived graph edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopGraphEdge {
    /// Edge id.
    pub id: String,
    /// Source record id.
    pub source: String,
    /// Target record id.
    pub target: String,
    /// Relationship label.
    pub label: String,
}

/// Session tree response exposed to GUI adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionTree {
    /// Root session id.
    pub root: String,
    /// Session nodes in parent-before-child order.
    pub nodes: Vec<DesktopSessionTreeNode>,
    /// Explicit merge events.
    pub merges: Vec<DesktopSessionTreeMerge>,
}

/// Session tree node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionTreeNode {
    /// Session id.
    pub id: String,
    /// Parent session id, absent for the root.
    pub parent_id: Option<String>,
    /// Branch kind such as `fork`, `clone`, or `tool_spawned`.
    pub branch_kind: Option<String>,
    /// Turn boundary where this branch diverged.
    pub at_turn_id: Option<String>,
    /// Tool call that spawned this branch, if any.
    pub tool_call_id: Option<String>,
    /// Child session ids.
    pub children: Vec<String>,
}

/// Session tree merge event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSessionTreeMerge {
    /// Source branch.
    pub source: String,
    /// Destination session.
    pub destination: String,
    /// Merge strategy.
    pub strategy: String,
    /// Reasoning summary record id for `reasoning_summary` merges.
    pub summary_record_id: Option<String>,
    /// First source turn for `controlled_splice` merges.
    pub first_turn_id: Option<String>,
    /// Last source turn for `controlled_splice` merges.
    pub last_turn_id: Option<String>,
    /// Destination turn where the merge landed.
    pub applied_at_turn_id: String,
}

/// Search result shown in the search panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopSearchResult {
    /// Matched record id.
    pub record_id: String,
    /// Matched title.
    pub title: String,
    /// Snippet with matching text.
    pub snippet: String,
    /// Deterministic fixture score.
    pub score: f64,
}

/// Lint finding shown in the lint panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopLintFinding {
    /// Stable finding id.
    pub id: String,
    /// Severity such as info, warning, or error.
    pub severity: String,
    /// Optional related record.
    pub record_id: Option<String>,
    /// Human-readable message.
    pub message: String,
}

/// Reconcile preview request from the renderer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReconcilePreviewRequest {
    /// Target record id.
    pub target_id: String,
    /// Expected record version.
    pub expected_version: u64,
    /// Backend hash the edit was based on.
    pub backend_hash: String,
    /// Proposed field diff.
    pub field_diff: BTreeMap<String, serde_json::Value>,
}

/// Reconcile preview response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReconcilePreview {
    /// Whether the preview can be applied.
    pub accepted: bool,
    /// Target record id.
    pub target_id: String,
    /// Expected record version.
    pub expected_version: u64,
    /// Mutable fields that passed policy.
    pub mutable_diff: BTreeMap<String, serde_json::Value>,
    /// Rejected fields and reason codes.
    pub rejected_fields: Vec<DesktopRejectedField>,
}

/// Rejected reconcile field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRejectedField {
    /// Field name.
    pub field: String,
    /// Stable rejection code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
}

/// Reconcile apply request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReconcileApplyRequest {
    /// Preview request to apply.
    #[serde(flatten)]
    pub preview: DesktopReconcilePreviewRequest,
}

/// Reconcile apply result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopReconcileApplyResult {
    /// Whether the apply succeeded.
    pub accepted: bool,
    /// Updated record when accepted.
    pub record: Option<DesktopRecordDetail>,
    /// Rejections when not accepted.
    pub rejected_fields: Vec<DesktopRejectedField>,
}
