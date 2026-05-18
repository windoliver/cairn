//! Task-trace canvas lint checks for issue #134.

use std::collections::HashSet;

use crate::contract::source_resolver::SourceResolver;
use crate::domain::projection::{ProjectionStatus, compare_projection};
use crate::generated::verbs::lint::{Finding, Kind, Severity};
use crate::verbs::lint::{LintRecord, finding, target_path};

/// Read-only trace-canvas snapshot supplied by adapters.
#[derive(Debug, Clone, Default)]
pub struct TraceCanvasLintSnapshot {
    /// Stored trace steps.
    pub steps: Vec<TraceCanvasLintStep>,
    /// Stored trace canvases.
    pub canvases: Vec<TraceCanvasLintCanvas>,
    /// Stored trace canvas nodes.
    pub nodes: Vec<TraceCanvasLintNode>,
}

/// Lint view of one `trace_steps` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCanvasLintStep {
    /// Stable trace-step id.
    pub step_id: String,
    /// Session id owning the step.
    pub session_id: String,
    /// Optional exact drilldown reference.
    pub result_ref: Option<String>,
    /// Optional mapped canvas node id.
    pub node_id: Option<String>,
}

/// Lint view of one `trace_canvases` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCanvasLintCanvas {
    /// Stable canvas id.
    pub canvas_id: String,
    /// Session id owning the canvas.
    pub session_id: String,
    /// Canvas title.
    pub title: String,
    /// Canvas goal.
    pub goal: String,
    /// Body-free canvas summary.
    pub summary: String,
    /// Optional active node id.
    pub active_node_id: Option<String>,
    /// Canvas-local render budget.
    pub max_bytes: u64,
    /// Last materialized markdown projection, if one exists.
    pub projection_markdown: Option<String>,
}

/// Lint view of one `trace_canvas_nodes` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceCanvasLintNode {
    /// Stable node id.
    pub node_id: String,
    /// Parent canvas id.
    pub canvas_id: String,
    /// Body-free node label.
    pub label: String,
    /// Body-free node summary.
    pub summary: String,
    /// Source trace step ids supporting this node.
    pub source_step_ids: Vec<String>,
}

/// Run task-trace canvas checks.
#[must_use]
pub fn run(
    snapshot: &TraceCanvasLintSnapshot,
    records: &[LintRecord],
    source_resolver: &dyn SourceResolver,
) -> Vec<Finding> {
    let node_ids = snapshot
        .nodes
        .iter()
        .map(|node| node.node_id.as_str())
        .collect::<HashSet<_>>();
    let step_ids = snapshot
        .steps
        .iter()
        .map(|step| step.step_id.as_str())
        .collect::<HashSet<_>>();
    let record_refs = records
        .iter()
        .flat_map(|record| {
            [
                record.stored.record.id.as_str(),
                record.stored.record.target_id.as_str(),
            ]
        })
        .collect::<HashSet<_>>();

    let mut findings = Vec::new();
    for step in &snapshot.steps {
        if let Some(result_ref) = step.result_ref.as_deref()
            && !record_refs.contains(result_ref)
            && !source_resolver.exists(result_ref)
        {
            findings.push(broken_result_ref_finding(step, result_ref));
        }

        match step.node_id.as_deref() {
            Some(node_id) if !node_ids.contains(node_id) => {
                findings.push(step_missing_node_finding(step, node_id));
            }
            None => findings.push(pending_step_finding(step)),
            _ => {}
        }
    }

    for node in &snapshot.nodes {
        for source_step_id in &node.source_step_ids {
            if !step_ids.contains(source_step_id.as_str()) {
                findings.push(node_missing_step_finding(node, source_step_id));
            }
        }
    }

    for canvas in &snapshot.canvases {
        if let Some(active_node_id) = canvas.active_node_id.as_deref()
            && !node_ids.contains(active_node_id)
        {
            findings.push(canvas_missing_active_node_finding(canvas, active_node_id));
        }

        let bytes = rendered_canvas_bytes(canvas, &snapshot.nodes);
        if bytes > canvas.max_bytes {
            findings.push(canvas_over_budget_finding(canvas, bytes));
        }

        let canonical = render_markdown(canvas, &snapshot.nodes);
        if let Some(finding) = projection_finding(canvas, &canonical) {
            findings.push(finding);
        }
    }

    findings
}

/// Render the canonical markdown projection for a trace canvas.
#[must_use]
pub fn render_markdown(canvas: &TraceCanvasLintCanvas, nodes: &[TraceCanvasLintNode]) -> String {
    let active_node = canvas.active_node_id.as_deref().unwrap_or("none");
    let mut out = String::new();
    out.push_str("# ");
    out.push_str(&canvas.title);
    out.push('\n');
    out.push('\n');
    out.push_str("Canvas: ");
    out.push_str(&canvas.canvas_id);
    out.push('\n');
    out.push_str("Session: ");
    out.push_str(&canvas.session_id);
    out.push('\n');
    out.push_str("Goal: ");
    out.push_str(&canvas.goal);
    out.push('\n');
    out.push_str("Active node: ");
    out.push_str(active_node);
    out.push('\n');
    out.push('\n');
    out.push_str("## Summary\n");
    out.push_str(&canvas.summary);
    out.push('\n');
    out.push('\n');
    out.push_str("## Nodes\n");

    for node in nodes
        .iter()
        .filter(|node| node.canvas_id == canvas.canvas_id)
    {
        out.push_str("- ");
        out.push_str(&node.label);
        out.push_str(" [");
        out.push_str(&node.node_id);
        out.push_str("]\n");
        out.push_str("  Summary: ");
        out.push_str(&node.summary);
        out.push('\n');
        out.push_str("  Source steps: ");
        if node.source_step_ids.is_empty() {
            out.push_str("none");
        } else {
            out.push_str(&node.source_step_ids.join(", "));
        }
        out.push('\n');
    }

    out
}

fn broken_result_ref_finding(step: &TraceCanvasLintStep, result_ref: &str) -> Finding {
    let mut f = finding(
        Kind::BrokenSourceLink,
        Severity::Error,
        format!(
            "trace_step `{}` result_ref `{result_ref}` does not resolve to an active record or source artifact",
            step.step_id,
        ),
    );
    f.target = Some(target_path(format!("trace_steps/{}", step.step_id)));
    f.suggested_fix =
        Some("repair the result_ref or forget/replay the stale trace step".to_owned());
    f
}

fn pending_step_finding(step: &TraceCanvasLintStep) -> Finding {
    let mut f = finding(
        Kind::DataGap,
        Severity::Warning,
        format!(
            "trace_step `{}` in session `{}` has no canvas node assignment",
            step.step_id, step.session_id,
        ),
    );
    f.target = Some(target_path(format!("trace_steps/{}", step.step_id)));
    f.suggested_fix = Some("run the trace_canvas materialization workflow".to_owned());
    f
}

fn step_missing_node_finding(step: &TraceCanvasLintStep, node_id: &str) -> Finding {
    let mut f = finding(
        Kind::Orphan,
        Severity::Error,
        format!(
            "trace_step `{}` references missing trace_canvas_node `{node_id}`",
            step.step_id,
        ),
    );
    f.target = Some(target_path(format!("trace_steps/{}", step.step_id)));
    f.suggested_fix = Some("rebuild the trace canvas from trace_steps".to_owned());
    f
}

fn node_missing_step_finding(node: &TraceCanvasLintNode, source_step_id: &str) -> Finding {
    let mut f = finding(
        Kind::Orphan,
        Severity::Error,
        format!(
            "trace_canvas_node `{}` cites missing source trace_step `{source_step_id}`",
            node.node_id,
        ),
    );
    f.target = Some(target_path(format!("trace_canvas_nodes/{}", node.node_id)));
    f.suggested_fix = Some("rebuild the trace canvas from trace_steps".to_owned());
    f
}

fn canvas_missing_active_node_finding(
    canvas: &TraceCanvasLintCanvas,
    active_node_id: &str,
) -> Finding {
    let mut f = finding(
        Kind::Orphan,
        Severity::Error,
        format!(
            "trace_canvas `{}` references missing active_node_id `{active_node_id}`",
            canvas.canvas_id,
        ),
    );
    f.target = Some(target_path(format!("trace_canvases/{}", canvas.canvas_id)));
    f.suggested_fix = Some("clear active_node_id or rebuild the canvas projection".to_owned());
    f
}

fn canvas_over_budget_finding(canvas: &TraceCanvasLintCanvas, bytes: u64) -> Finding {
    let mut f = finding(
        Kind::HotMemoryOverBudget,
        Severity::Error,
        format!(
            "trace_canvas `{}` renders to {bytes} bytes, exceeding its {} byte budget",
            canvas.canvas_id, canvas.max_bytes,
        ),
    );
    f.target = Some(target_path(format!("trace_canvases/{}", canvas.canvas_id)));
    f.suggested_fix =
        Some("compact the canvas summary or increase the trace canvas budget".to_owned());
    f
}

fn projection_finding(canvas: &TraceCanvasLintCanvas, canonical: &str) -> Option<Finding> {
    let path = format!("trace_canvases/{}.md", canvas.canvas_id);
    match compare_projection(canonical, canvas.projection_markdown.as_deref()) {
        ProjectionStatus::Match => None,
        ProjectionStatus::Drift {
            expected_body_hash,
            actual_body_hash,
        } => {
            let mut f = finding(
                Kind::ProjectionDrift,
                Severity::Warning,
                format!(
                    "trace canvas projection at {path} drifts from db; expected_body_hash={expected_body_hash} actual_body_hash={actual_body_hash}",
                ),
            );
            f.target = Some(target_path(path));
            f.suggested_fix = Some("rebuild the trace canvas markdown projection".to_owned());
            Some(f)
        }
        ProjectionStatus::Missing { expected_body_hash } => {
            let mut f = finding(
                Kind::ProjectionMissing,
                Severity::Warning,
                format!(
                    "trace canvas projection at {path} is missing; expected_body_hash={expected_body_hash}",
                ),
            );
            f.target = Some(target_path(path));
            f.suggested_fix = Some("rebuild the trace canvas markdown projection".to_owned());
            Some(f)
        }
    }
}

fn rendered_canvas_bytes(canvas: &TraceCanvasLintCanvas, nodes: &[TraceCanvasLintNode]) -> u64 {
    u64::try_from(render_markdown(canvas, nodes).len()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CairnConfig;
    use crate::contract::memory_store::StoredRecord;
    use crate::domain::record::tests_export::sample_record;
    use crate::generated::verbs::lint::{Kind, Severity};
    use crate::verbs::lint::{ConsentModel, LintRecord, SchemaVersion, empty_source_resolver};

    fn lint_record(record_id: &str, target_id: &str) -> LintRecord {
        let mut record = sample_record();
        record.id = crate::domain::RecordId::parse(record_id).expect("valid record id");
        record.target_id =
            crate::domain::TargetId::parse(target_id.to_owned()).expect("valid target id");
        LintRecord {
            stored: StoredRecord {
                record,
                version: 1,
                schema_version: Some(SchemaVersion::current()),
            },
            consent_model: ConsentModel::LegacyEvent,
        }
    }

    fn canvas_fixture() -> TraceCanvasLintCanvas {
        TraceCanvasLintCanvas {
            canvas_id: "canvas-1".to_owned(),
            session_id: "session-1".to_owned(),
            title: "Current task".to_owned(),
            goal: "Keep the trace canvas healthy".to_owned(),
            summary: "Canvas summary".to_owned(),
            active_node_id: Some("node-1".to_owned()),
            max_bytes: 256,
            projection_markdown: Some(render_markdown(
                &TraceCanvasLintCanvas {
                    canvas_id: "canvas-1".to_owned(),
                    session_id: "session-1".to_owned(),
                    title: "Current task".to_owned(),
                    goal: "Keep the trace canvas healthy".to_owned(),
                    summary: "Canvas summary".to_owned(),
                    active_node_id: Some("node-1".to_owned()),
                    max_bytes: 256,
                    projection_markdown: None,
                },
                &[node_fixture()],
            )),
        }
    }

    fn node_fixture() -> TraceCanvasLintNode {
        TraceCanvasLintNode {
            node_id: "node-1".to_owned(),
            canvas_id: "canvas-1".to_owned(),
            label: "Mapped node".to_owned(),
            summary: "Node summary".to_owned(),
            source_step_ids: vec!["step-1".to_owned()],
        }
    }

    #[test]
    fn healthy_canvas_snapshot_has_no_findings() {
        let records = [lint_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAA",
            "01ARZ3NDEKTSV4RRFFQ69G5FAB",
        )];
        let snapshot = TraceCanvasLintSnapshot {
            steps: vec![TraceCanvasLintStep {
                step_id: "step-1".to_owned(),
                session_id: "session-1".to_owned(),
                result_ref: Some(records[0].stored.record.id.as_str().to_owned()),
                node_id: Some("node-1".to_owned()),
            }],
            canvases: vec![canvas_fixture()],
            nodes: vec![node_fixture()],
        };

        assert!(run(&snapshot, &records, empty_source_resolver()).is_empty());
    }

    #[test]
    fn projection_markdown_is_rebuildable_and_includes_source_ids() {
        let canvas = canvas_fixture();
        let rendered = render_markdown(&canvas, &[node_fixture()]);

        assert!(rendered.contains("# Current task"));
        assert!(rendered.contains("Canvas: canvas-1"));
        assert!(rendered.contains("Active node: node-1"));
        assert!(rendered.contains("## Nodes"));
        assert!(rendered.contains("- Mapped node [node-1]"));
        assert!(rendered.contains("Source steps: step-1"));
        assert!(rendered.ends_with('\n'));
    }

    #[test]
    fn missing_or_drifted_projection_emits_projection_findings() {
        let canonical = render_markdown(&canvas_fixture(), &[node_fixture()]);
        let snapshot = TraceCanvasLintSnapshot {
            steps: vec![TraceCanvasLintStep {
                step_id: "step-1".to_owned(),
                session_id: "session-1".to_owned(),
                result_ref: None,
                node_id: Some("node-1".to_owned()),
            }],
            canvases: vec![
                TraceCanvasLintCanvas {
                    projection_markdown: None,
                    ..canvas_fixture()
                },
                TraceCanvasLintCanvas {
                    canvas_id: "canvas-2".to_owned(),
                    active_node_id: None,
                    projection_markdown: Some(format!("{canonical}\nstale edit\n")),
                    ..canvas_fixture()
                },
            ],
            nodes: vec![
                node_fixture(),
                TraceCanvasLintNode {
                    canvas_id: "canvas-2".to_owned(),
                    node_id: "node-2".to_owned(),
                    source_step_ids: Vec::new(),
                    ..node_fixture()
                },
            ],
        };

        let findings = run(&snapshot, &[], empty_source_resolver());
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::ProjectionMissing)
                    && f.message.contains("trace_canvases/canvas-1.md")),
            "missing projection should be reported: {findings:?}",
        );
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::ProjectionDrift)
                    && f.message.contains("trace_canvases/canvas-2.md")),
            "drifted projection should be reported: {findings:?}",
        );
    }

    #[test]
    fn pending_step_broken_result_ref_orphan_node_and_budget_emit_findings() {
        let _cfg = CairnConfig::default();
        let snapshot = TraceCanvasLintSnapshot {
            steps: vec![
                TraceCanvasLintStep {
                    step_id: "step-pending".to_owned(),
                    session_id: "session-1".to_owned(),
                    result_ref: Some("missing-result".to_owned()),
                    node_id: None,
                },
                TraceCanvasLintStep {
                    step_id: "step-mapped-missing-node".to_owned(),
                    session_id: "session-1".to_owned(),
                    result_ref: None,
                    node_id: Some("node-missing".to_owned()),
                },
            ],
            canvases: vec![TraceCanvasLintCanvas {
                title: "A very long title".repeat(4),
                goal: "A very long goal".repeat(4),
                summary: "A very long summary".repeat(4),
                max_bytes: 32,
                ..canvas_fixture()
            }],
            nodes: vec![TraceCanvasLintNode {
                source_step_ids: vec!["step-missing".to_owned()],
                ..node_fixture()
            }],
        };

        let findings = run(&snapshot, &[], empty_source_resolver());
        assert!(
            findings
                .iter()
                .any(|f| matches!((f.kind, f.severity), (Kind::DataGap, Severity::Warning))),
            "pending trace steps should emit a DataGap warning: {findings:?}",
        );
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::BrokenSourceLink)
                    && f.message.contains("missing-result")),
            "broken result_ref should emit BrokenSourceLink: {findings:?}",
        );
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::Orphan) && f.message.contains("node-missing")),
            "step mapped to a missing node should emit Orphan: {findings:?}",
        );
        assert!(
            findings
                .iter()
                .any(|f| matches!(f.kind, Kind::HotMemoryOverBudget)
                    && f.message.contains("canvas-1")),
            "oversized canvas text should emit HotMemoryOverBudget: {findings:?}",
        );
    }
}
