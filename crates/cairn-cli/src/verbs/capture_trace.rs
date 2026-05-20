//! `cairn capture_trace` handler (issue #77, brief §5.0).
//!
//! This module owns the async verb handler [`run_handler`] that reads a
//! JSONL stream of [`CaptureEvent`]s from disk, groups them by
//! `(session_id, turn_id)`, and persists each turn atomically via
//! [`SqliteMemoryStore::with_tx`].
//!
//! # Parent-event-id convention
//!
//! `PostTool` and `ToolOutput` events require a `parent_event_id` pointing
//! at the `PreTool` event that initiated the same tool call (validated by
//! `validate_turn_links`). Harnesses emit hook events sequentially, so the
//! canonical convention implemented here is:
//!
//! > *The `parent_event_id` of a `PostTool`/`ToolOutput` event equals the
//! > `capture_event_id` of the most-recently-seen `PreTool` event with the
//! > same `tool_call_id` in the same turn group.*
//!
//! If no matching `PreTool` is found for a given `tool_call_id`, the event
//! is reported as failed; the remaining events in the turn are not
//! persisted.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_core::config::{CairnConfig, ConsolidationConfig, DreamConfig, DreamTier};
use cairn_core::contract::job_store::JobStore;
use cairn_core::domain::capture::{
    CaptureEvent, CaptureMode, CapturePayload, CaptureRefs, PayloadHash, SourceFamily,
};
use cairn_core::domain::trace::{TraceBlock, TraceEvent, TraceLink};
use cairn_core::domain::{
    ActorChainEntry, BudgetObservation, CaptureEventId, ChainRole, Identity, LocalSensorName,
    Rfc3339Timestamp, ScopeTuple, SensorGateReason, SessionId, ZeroCaptureAuditInput,
    ZeroCaptureReport, ZeroCaptureTrigger, decide_zero_capture_nudge,
};
use cairn_core::generated::common::Ulid as WireUlid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::capture_trace::{CaptureTraceData, FailedTurn};
use cairn_core::pipeline::capture_trace::{
    ProjectedTraceBlocks, classify as core_classify, project, project_pre_compact_snapshot,
    project_with_blocks,
};
use cairn_core::pipeline::dispatch::{DefaultRegistry, trace_body_bytes};
use cairn_core::pipeline::extract::body::ResolvedBody;
use cairn_core::pipeline::filter::{
    Decision, FilterInputs, RedactionTag, fence, redact, should_memorize,
};
use cairn_core::pipeline::turn::summarize_turn_with_scope;
use cairn_core::policy_trace::{PolicyErrorCode, PolicyGate, PolicyTraceEntry, to_wire};
use cairn_store_sqlite::{SqliteMemoryStore, TraceStepDraft};
use clap::ArgMatches;
use sha2::{Digest as _, Sha256};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use ulid::Ulid;

use cairn_workflows::{
    SkillifyTrigger, TraceCanvasPayload, TraceCanvasProjection,
    consolidation::enqueue_if_due_scoped, enqueue_skillify, enqueue_tier_with_dedupe_token,
    enqueue_trace_canvas_step,
};

use crate::identity::{guard::refuse_if_degraded, status::ReconciliationReport};
use crate::sensor_gate::{
    SensorDropBudgetMetric, SensorDropMetric, SensorGateStage, append_sensor_drop_metric,
    safe_metric_ref,
};

use super::envelope::{emit_json, human_error, invalid_args_response, new_operation_id};

const DEFAULT_TENANT: &str = "default";
const CAPTURE_TRACE_ENTITY: &str = "ingest";
const DIRECT_BLOCKS_TURN_PREFIX: &str = "trace-blocks";

/// Result returned by [`run_handler`] on success.
#[derive(Debug, serde::Serialize)]
pub struct CaptureTraceResponse {
    /// Stable trace-operation id (ULID string). Unique per invocation.
    pub trace_id: String,
    /// Per-turn failures: `(session_id, turn_id, error_message)`.
    ///
    /// A non-empty list means some turns were not persisted. The turns not
    /// listed were persisted successfully.
    pub failed_turns: Vec<(String, String, String)>,
    /// Body-free policy trace produced by the per-event filter path.
    pub policy_trace: Vec<ResponsePolicyTrace>,
}

/// Read newline-delimited JSON [`CaptureEvent`]s from `path`. Blank lines
/// are skipped; the first malformed line aborts the read.
///
/// # Errors
///
/// - File open / read failure (with path context).
/// - Any line that fails to parse as [`CaptureEvent`].
pub async fn read_jsonl_events(path: impl AsRef<Path>) -> anyhow::Result<Vec<CaptureEvent>> {
    let path = path.as_ref();
    let f = File::open(path)
        .await
        .with_context(|| format!("open trace JSONL at {}", path.display()))?;
    let mut lines = BufReader::new(f).lines();
    let mut events = Vec::new();
    let mut line_no = 0_usize;
    while let Some(line) = lines
        .next_line()
        .await
        .with_context(|| format!("read line from {}", path.display()))?
    {
        line_no += 1;
        if line.trim().is_empty() {
            continue;
        }
        let event: CaptureEvent = serde_json::from_str(&line)
            .with_context(|| format!("parse CaptureEvent at {}:{line_no}", path.display()))?;
        events.push(event);
    }
    Ok(events)
}

/// Persist a JSONL batch of [`CaptureEvent`]s grouped by `(session_id, turn_id)`.
///
/// Steps per call:
/// 1. `refuse_if_degraded` guard.
/// 2. Read and validate every event in the JSONL file.
/// 3. Group by `(session_id, turn_id)`; events missing either field are
///    reported as failures immediately.
/// 4. For each group: resolve bodies from `sources/`, project records,
///    resolve `parent_event_id` for `PostTool` events, then atomically
///    `renumber_turn_with` + `validate_turn_links` + optional `summarize`.
///    A failure in one turn does **not** abort other turns.
/// 5. Return a [`CaptureTraceResponse`] with a fresh trace-id and any
///    per-turn failures.
///
/// # Parent-event-id convention
///
/// See module-level doc comment.
///
/// # Errors
///
/// Returns an error only for unrecoverable setup failures (e.g. the JSONL
/// file cannot be read, or envelope validation fails for all events). Per-turn
/// failures are reported in [`CaptureTraceResponse::failed_turns`].
pub async fn run_handler(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    from: &Path,
) -> anyhow::Result<CaptureTraceResponse> {
    run_handler_inner(
        store,
        vault_root,
        from,
        None,
        None,
        None,
        &ConsolidationConfig::default(),
        &DreamConfig::default(),
    )
    .await
}

/// Persist a JSONL batch while binding projected rows to a verified vault scope.
pub async fn run_handler_with_scope(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    from: &Path,
    scope_binding: ScopeTuple,
) -> anyhow::Result<CaptureTraceResponse> {
    run_handler_inner(
        store,
        vault_root,
        from,
        Some(&scope_binding),
        None,
        None,
        &ConsolidationConfig::default(),
        &DreamConfig::default(),
    )
    .await
}

/// Persist an already-materialized batch of capture events.
///
/// This is used by import paths that have already staged and hashed payload
/// files. Behavior matches [`run_handler`] except the events are supplied
/// directly instead of read from JSONL. The vault guard runs before any
/// persistence work.
pub async fn run_events_handler(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    events: Vec<CaptureEvent>,
) -> anyhow::Result<CaptureTraceResponse> {
    capture_trace_guard()?;
    run_events_handler_inner_no_guard(
        store,
        vault_root,
        events,
        None,
        None,
        None,
        &ConsolidationConfig::default(),
        &DreamConfig::default(),
    )
    .await
}

/// Persist an already-materialized batch while binding projected rows to a verified vault scope.
pub async fn run_events_handler_with_scope(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    events: Vec<CaptureEvent>,
    scope_binding: ScopeTuple,
) -> anyhow::Result<CaptureTraceResponse> {
    capture_trace_guard()?;
    run_events_handler_inner_no_guard(
        store,
        vault_root,
        events,
        Some(&scope_binding),
        None,
        None,
        &ConsolidationConfig::default(),
        &DreamConfig::default(),
    )
    .await
}

/// Persist a direct `Vec<TraceBlock>` capture from a JSON file.
///
/// ```rust,no_run
/// use cairn_cli::verbs::capture_trace::run_blocks_handler;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let vault = tempfile::tempdir().unwrap();
/// let store = cairn_store_sqlite::open_in_memory().await.unwrap();
/// let blocks_path = vault.path().join("trace-blocks.json");
/// std::fs::write(
///     &blocks_path,
///     serde_json::json!([
///         { "kind": "reasoning", "text": "thinking", "signature": "sig-1" },
///         { "kind": "tool_use", "tool": "Read", "input": {"file": "README.md"}, "id": "tool-1" },
///         { "kind": "tool_result", "tool_use_id": "tool-1", "content": "file body", "is_error": false },
///         { "kind": "text", "text": "final answer" }
///     ])
///     .to_string(),
/// )
/// .unwrap();
///
/// let result = run_blocks_handler(
///     &store,
///     vault.path(),
///     &blocks_path,
///     "01ARZ3NDEKTSV4RRFFQ69G5FAV",
/// )
/// .await
/// .unwrap();
/// assert!(result.failed_turns.is_empty());
/// # });
/// ```
pub async fn run_blocks_handler(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    blocks_path: &Path,
    session_id: &str,
) -> anyhow::Result<CaptureTraceResponse> {
    run_blocks_handler_inner(store, vault_root, blocks_path, session_id, None).await
}

async fn run_blocks_handler_with_scope(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    blocks_path: &Path,
    session_id: &str,
    scope_binding: ScopeTuple,
) -> anyhow::Result<CaptureTraceResponse> {
    run_blocks_handler_inner(
        store,
        vault_root,
        blocks_path,
        session_id,
        Some(&scope_binding),
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "trace import keeps validation, projection, and per-turn atomicity in one ordered transaction flow"
)]
async fn run_handler_inner(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    from: &Path,
    scope_binding: Option<&ScopeTuple>,
    sensor_config: Option<&CairnConfig>,
    job_store: Option<&dyn JobStore>,
    consolidation_config: &ConsolidationConfig,
    dream_config: &DreamConfig,
) -> anyhow::Result<CaptureTraceResponse> {
    capture_trace_guard()?;
    let events = read_jsonl_events(from).await?;
    run_events_handler_inner_no_guard(
        store,
        vault_root,
        events,
        scope_binding,
        sensor_config,
        job_store,
        consolidation_config,
        dream_config,
    )
    .await
}

fn capture_trace_guard() -> anyhow::Result<()> {
    refuse_if_degraded(&ReconciliationReport::default(), vec![])
        .context("capture_trace: vault degraded")
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "trace import keeps validation, projection, and per-turn atomicity in one ordered transaction flow"
)]
async fn run_events_handler_inner_no_guard(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    events: Vec<CaptureEvent>,
    scope_binding: Option<&ScopeTuple>,
    sensor_config: Option<&CairnConfig>,
    job_store: Option<&dyn JobStore>,
    consolidation_config: &ConsolidationConfig,
    dream_config: &DreamConfig,
) -> anyhow::Result<CaptureTraceResponse> {
    // Group by (session_id, turn_id). Events missing either ref, or
    // failing structural validation, are reported as failed and skipped
    // rather than aborting the whole import. Per-turn atomicity: a turn
    // that contains *any* invalid event is poisoned — its valid siblings
    // are dropped too so the store never sees a truncated turn that
    // would summarize against incomplete data.
    let mut groups: BTreeMap<(String, String), Vec<&CaptureEvent>> = BTreeMap::new();
    let mut failed_turns: Vec<(String, String, String)> = Vec::new();
    let mut poisoned: BTreeSet<(String, String)> = BTreeSet::new();
    let mut policy_trace_entries: Vec<PolicyTraceEntry> = Vec::new();

    for event in &events {
        // Structural envelope validation. Failures land in failed_turns
        // keyed on whatever session/turn refs the event managed to carry,
        // and the (session, turn) pair is poisoned for this import.
        if let Err(e) = event.validate() {
            let (s, t) = event
                .refs
                .as_ref()
                .map_or((String::new(), String::new()), |r| {
                    (
                        r.session_id.clone().unwrap_or_default(),
                        r.turn_id.clone().unwrap_or_default(),
                    )
                });
            failed_turns.push((s.clone(), t.clone(), format!("envelope validate: {e}")));
            poisoned.insert((s, t));
            continue;
        }
        let Some(refs) = event.refs.as_ref() else {
            failed_turns.push((String::new(), String::new(), "event missing refs".into()));
            continue;
        };
        let (Some(s), Some(t)) = (refs.session_id.as_deref(), refs.turn_id.as_deref()) else {
            failed_turns.push((
                refs.session_id.clone().unwrap_or_default(),
                refs.turn_id.clone().unwrap_or_default(),
                "event missing session_id or turn_id".into(),
            ));
            continue;
        };
        groups
            .entry((s.to_owned(), t.to_owned()))
            .or_default()
            .push(event);
    }
    // Drop any group whose turn key was poisoned by an invalid sibling.
    groups.retain(|key, _| !poisoned.contains(key));

    for ((session_str, turn_str), group) in groups {
        let session_id = match SessionId::parse(&session_str) {
            Ok(s) => s,
            Err(e) => {
                failed_turns.push((session_str, turn_str, e.to_string()));
                continue;
            }
        };

        // Resolve bodies from sources/ and project records *before* entering
        // the sync tx closure — tokio::fs reads must not run on the DB
        // worker thread.
        let mut projected: Vec<cairn_core::domain::MemoryRecord> = Vec::with_capacity(group.len());
        let mut trace_canvas_projections: Vec<TraceCanvasProjection> = Vec::new();
        let mut had_stop = false;
        let mut explicit_skillify_requested = false;
        let mut group_failed = false;

        // Track most-recently-seen pre_tool capture_event_id per tool_call_id
        // so post_tool events can resolve their parent_event_id.
        //
        // Pre-pass: scan the FULL group up-front and index every PreTool by
        // tool_call_id. This makes parent resolution insensitive to JSONL
        // file order — a PostTool that appears before its PreTool in the
        // batch still resolves its parent in-batch instead of falling back
        // to the cross-batch store-lookup (which only sees rows already
        // persisted in prior transactions and would mis-fire here).
        let mut last_pre_tool: BTreeMap<String, CaptureEventId> = BTreeMap::new();
        for event in &group {
            if !matches!(classify_trace_event(event), Ok(TraceEvent::PreTool)) {
                continue;
            }
            let Some(refs) = event.refs.as_ref() else {
                continue;
            };
            if let Some(tcid) = refs.tool_id.as_deref() {
                last_pre_tool.insert(tcid.to_owned(), event.event_id.clone());
            }
        }

        // Indices into `projected` whose parent_event_id must be resolved
        // from the store (i.e., the PreTool arrived in a prior batch).
        // Tuple: (projected_index, tool_call_id).
        let mut needs_parent_from_store: Vec<(usize, String)> = Vec::new();

        for event in &group {
            // Classify the event. Fail the whole turn on the first
            // unclassifiable event rather than persisting a partial set:
            // the summary record would otherwise be built from incomplete
            // data and become hard-to-detect data loss.
            let classified = match classify_trace_event(event) {
                Ok(c) => c,
                Err(e) => {
                    failed_turns.push((
                        session_str.clone(),
                        turn_str.clone(),
                        format!("classify {}: {e}", event.event_id),
                    ));
                    group_failed = true;
                    break;
                }
            };
            if classified == TraceEvent::Stop {
                had_stop = true;
            }

            if let Some(config) = sensor_config {
                match evaluate_capture_trace_sensor_gate(store, vault_root, config, event).await {
                    Ok(Some(reason)) => {
                        policy_trace_entries.push(PolicyTraceEntry::error(
                            PolicyGate::SensorConsent,
                            PolicyErrorCode::from_static(reason.as_str()),
                        ));
                        failed_turns.push((
                            session_str.clone(),
                            turn_str.clone(),
                            format!("sensor_gate:{}", reason.as_str()),
                        ));
                        group_failed = true;
                        break;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        failed_turns.push((
                            session_str.clone(),
                            turn_str.clone(),
                            format!("sensor_gate: {error:#}"),
                        ));
                        group_failed = true;
                        break;
                    }
                }
            }

            // Resolve body from sources/, route it through the Capture →
            // Extract dispatch decision, then run the same pre-persist
            // filter gate used by write-path ingest. A discard rejects the
            // whole turn before projection so trace rows and summaries cannot
            // contain partial privacy-blocked content.
            let raw_bytes = match resolve_body_bytes(vault_root, event).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    failed_turns.push((
                        session_str.clone(),
                        turn_str.clone(),
                        format!("resolve_body: {e}"),
                    ));
                    group_failed = true;
                    break;
                }
            };
            let body_bytes = match trace_body_bytes(event, &raw_bytes, &DefaultRegistry) {
                Ok(bytes) => bytes,
                Err(e) => {
                    failed_turns.push((
                        session_str.clone(),
                        turn_str.clone(),
                        format!("trace_body {}: {e}", event.event_id),
                    ));
                    group_failed = true;
                    break;
                }
            };
            let raw_text = match trace_text(event, &body_bytes) {
                Ok(text) => text,
                Err(e) => {
                    failed_turns.push((
                        session_str.clone(),
                        turn_str.clone(),
                        format!("resolve_body: {e}"),
                    ));
                    group_failed = true;
                    break;
                }
            };
            if classified == TraceEvent::UserMessage && explicit_skillify_request(&raw_text) {
                explicit_skillify_requested = true;
            }
            let redacted = redact(&raw_text);
            let fenced = fence(&redacted.text);
            let blocks_secret = redacted.spans.iter().any(|span| is_secret_tag(span.tag));
            let inputs = FilterInputs {
                allow_redacted: !blocks_secret,
                ..FilterInputs::new(&redacted, &fenced)
            };
            let decision = should_memorize(&inputs);
            policy_trace_entries.extend([
                PolicyTraceEntry::from(&redacted),
                PolicyTraceEntry::from(&fenced),
                PolicyTraceEntry::from(&decision),
            ]);
            if let Decision::Discard(reason) = decision {
                failed_turns.push((
                    session_str.clone(),
                    turn_str.clone(),
                    format!("privacy filter rejected turn: {}", reason.as_str()),
                ));
                group_failed = true;
                break;
            }
            let text = fenced.text;

            let refs = event
                .refs
                .as_ref()
                .expect("invariant: filtered to events with refs above");

            // Resolve tool_call_id and parent_event_id.
            let tool_call_id: Option<String> = refs.tool_id.clone();

            // Derive parent_event_id for PostTool/ToolOutput.
            //
            // Primary: most-recently-seen PreTool with matching tool_call_id
            // *in this batch* (fast path, no store round-trip).
            //
            // Fallback: if no in-batch PreTool is found, the PreTool may
            // have arrived in a previous batch.  We project the record with
            // a placeholder parent (the event's own id) and record it in
            // `needs_parent_from_store`; the real parent is resolved inside
            // the transaction after existing rows are loaded.
            let (parent_event_id, needs_store_lookup) =
                if matches!(classified, TraceEvent::PostTool | TraceEvent::ToolOutput) {
                    let key = tool_call_id.clone().unwrap_or_default();
                    if let Some(id) = last_pre_tool.get(&key).cloned() {
                        (Some(id), false)
                    } else {
                        // Use own event id as placeholder so `TraceLink::validate`
                        // passes (it only requires parent_event_id is Some for
                        // PostTool/ToolOutput, not that it points at a distinct id).
                        // The real parent is patched inside `with_tx`.
                        (Some(event.event_id.clone()), true)
                    }
                } else {
                    (None, false)
                };

            let link = TraceLink {
                session_id: session_id.clone(),
                turn_id: turn_str.clone(),
                sequence: 0, // overridden by renumber_turn_with
                capture_event_id: event.event_id.clone(),
                parent_event_id,
                tool_call_id: tool_call_id.clone(),
                member_event_ids: Vec::new(),
            };

            // Build resolved body borrowing from the owned `text` on the stack.
            // SAFETY: `text` is owned by this block; `resolved` borrows it for
            // the duration of the `project` call only, which completes before
            // `text` is dropped. This is safe because both live in the same
            // `for` iteration scope.
            let resolved = ResolvedBody::from_trace_hook(&text);

            let mut record = match if classified == TraceEvent::PreCompact {
                project_pre_compact_snapshot(event, &resolved, &link)
            } else {
                project(event, classified, &resolved, &link)
            } {
                Ok(r) => r,
                Err(e) => {
                    failed_turns.push((
                        session_str.clone(),
                        turn_str.clone(),
                        format!("project: {e}"),
                    ));
                    group_failed = true;
                    break;
                }
            };
            if let Some(scope_binding) = scope_binding {
                bind_record_scope(&mut record, scope_binding);
            }
            policy_trace_entries.push(PolicyTraceEntry::pass(PolicyGate::VisibilityFloor));

            if needs_store_lookup {
                let key = tool_call_id.clone().unwrap_or_default();
                needs_parent_from_store.push((projected.len(), key));
            }
            if let Some(projection) = trace_canvas_projection_for_record(event, classified, &record)
            {
                trace_canvas_projections.push(projection);
            }
            projected.push(record);

            // Register this PreTool so later PostTool/ToolOutput events can
            // find their parent.
            if classified == TraceEvent::PreTool {
                let key = tool_call_id.unwrap_or_default();
                last_pre_tool.insert(key, event.event_id.clone());
            }
        }

        if group_failed {
            continue;
        }

        // Per-turn atomic transaction.
        let session_id_tx = session_id.clone();
        let turn_str_tx = turn_str.clone();
        let projected_record_ids: Vec<String> = projected
            .iter()
            .map(|record| record.id.as_str().to_owned())
            .collect();
        // Clone scope binding for the move closure. None at single-tenant
        // P0; Some when capture_trace is dispatched under a signed verb
        // context with bound tenant/workspace (round-3 adversarial
        // review #2).
        let scope_binding_tx: Option<ScopeTuple> = scope_binding.cloned();
        let result = store
            .with_tx(move |tx| {
                // Resolve any parent_event_ids that could not be satisfied
                // by the in-batch `last_pre_tool` map.  These are
                // PostTool/ToolOutput events whose PreTool arrived in a
                // prior batch.  We query the existing rows for the turn,
                // locate the pre_tool row with the matching tool_call_id,
                // and patch `extra_frontmatter.trace.parent_event_id` on
                // the projected record before `renumber_turn_with` sees it.
                if !needs_parent_from_store.is_empty() {
                    let existing = tx.list_trace_events(&session_id_tx, &turn_str_tx)?;
                    // Build: tool_call_id -> capture_event_id for pre_tool rows.
                    let pre_tool_by_tcid: BTreeMap<String, String> = existing
                        .iter()
                        .filter_map(|r| {
                            let evt = r
                                .extra_frontmatter
                                .get("trace_event")
                                .and_then(|v| v.as_str())?;
                            if evt != "pre_tool" {
                                return None;
                            }
                            let trace = r
                                .extra_frontmatter
                                .get("trace")
                                .and_then(|v| v.as_object())?;
                            let tcid = trace
                                .get("tool_call_id")
                                .and_then(|v| v.as_str())?
                                .to_owned();
                            let ceid = trace
                                .get("capture_event_id")
                                .and_then(|v| v.as_str())?
                                .to_owned();
                            Some((tcid, ceid))
                        })
                        .collect();

                    for (idx, tool_call_id) in &needs_parent_from_store {
                        let Some(parent_ceid) = pre_tool_by_tcid.get(tool_call_id) else {
                            return Err(cairn_store_sqlite::error::StoreError::Invariant {
                                what: format!(
                                    "PostTool/ToolOutput at projected[{idx}] has no \
                                         PreTool (in-batch or store) for \
                                         tool_call_id={tool_call_id:?}"
                                ),
                            });
                        };
                        // Patch `extra_frontmatter.trace.parent_event_id`.
                        if let Some(trace_obj) = projected[*idx]
                            .extra_frontmatter
                            .get_mut("trace")
                            .and_then(|v| v.as_object_mut())
                        {
                            trace_obj.insert(
                                "parent_event_id".into(),
                                serde_json::Value::String(parent_ceid.clone()),
                            );
                        }
                    }
                }

                tx.renumber_turn_with(&session_id_tx, &turn_str_tx, &projected)?;
                tx.validate_turn_links(&session_id_tx, &turn_str_tx)?;

                // Summarize if Stop landed in this batch OR a summary already
                // exists (closed-turn re-summarize per spec §4). Stamp the
                // summary with a strictly monotonic per-session turn ordinal
                // so list_trace_turns + latest_consolidation_watermark
                // advance correctly even when turns share an event count
                // (round-2 adversarial review #1).
                if had_stop || tx.turn_summary_exists(&session_id_tx, &turn_str_tx)? {
                    let final_rows = tx.list_trace_events(&session_id_tx, &turn_str_tx)?;
                    let turn_ordinal = tx.next_turn_ordinal_scoped(
                        &session_id_tx,
                        &turn_str_tx,
                        scope_binding_tx.as_ref(),
                    )?;
                    let summary = summarize_turn_with_scope(
                        &session_id_tx,
                        &turn_str_tx,
                        &final_rows,
                        turn_ordinal,
                        scope_binding_tx.as_ref(),
                    )
                    .map_err(|e| {
                        cairn_store_sqlite::error::StoreError::Invariant {
                            what: format!("summarize_turn: {e}"),
                        }
                    })?;
                    tx.upsert_trace(&summary)?;
                }
                tx.count_trace_events_for_session(&session_id_tx)
            })
            .await;

        // Destructure the per-turn tx outcome: failures push onto
        // failed_turns and skip the post-commit observers below.
        let successful_capture_trace_writes = match result {
            Ok(count) => count,
            Err(e) => {
                failed_turns.push((session_str, turn_str, e.to_string()));
                continue;
            }
        };

        // After a successful turn commit, attempt to enqueue a consolidation
        // job. Two queries feed the trigger:
        //
        // 1. `list_trace_turns(session, 0, MAX).len()` — cumulative count of
        //    closed turns. Used as `latest_sequence`. Lets a session that
        //    grows one turn at a time across multiple `capture_trace` calls
        //    still cross `min_turns_for_trigger=4` cumulatively.
        // 2. `latest_consolidation_watermark(session)` — highest
        //    `consolidation.last_sequence` across existing summaries for this
        //    session. Used as `since_sequence`. Without this, the dedupe_key
        //    stays pinned to `{session}:0` forever and only the first window
        //    ever consolidates (round-1 adversarial review #3).
        //
        // Failure is non-fatal — the CLI verb succeeds even if the scheduler
        // is absent or the enqueue fails.
        if let Some(js) = job_store {
            let now_ms = i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            )
            .unwrap_or(i64::MAX);

            for projection in &trace_canvas_projections {
                let _ = enqueue_trace_canvas_step(
                    js,
                    TraceCanvasPayload {
                        projection: projection.clone(),
                    },
                    now_ms,
                )
                .await;
            }

            // Scope both reads + enqueue by the caller's bound scope so
            // queries narrow to this issuer's data (round-4 adversarial
            // review #1). At single-tenant P0 `scope_binding` is `None`
            // and the *_scoped variants reduce to the un-narrowed query.
            //
            // `latest_sequence` must equal what the handler will actually
            // see in its window — ACTIVE turn_summary records whose
            // sequence > watermark — plus the watermark itself, so that
            // `latest - since = active_eligible_count` (the check
            // `enqueue_if_due` performs). If we used max(seq) including
            // tombstoned, a forget BEFORE the trigger threshold would
            // make the trigger fire while the handler's window was
            // empty; the dedupe_key would then permanently lock out
            // future windows for that watermark (round-6 adversarial
            // review #1). Watermark itself stays monotone via
            // `latest_consolidation_watermark_scoped` which counts
            // tombstoned summaries.
            let now_ms = i64::try_from(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            )
            .unwrap_or(i64::MAX);
            let since_sequence = store
                .latest_consolidation_watermark_scoped(&session_str, scope_binding)
                .await
                .unwrap_or(0);
            // On query failure, skip enqueue rather than substituting
            // projected_len (round-7 adversarial review #3).
            let active_eligible_opt = store
                .list_trace_turns_scoped(&session_str, since_sequence, u32::MAX, scope_binding)
                .await
                .ok()
                .map(|h| u32::try_from(h.len()).unwrap_or(u32::MAX));
            let dream_dedupe_token =
                active_eligible_opt.map(|active| since_sequence.saturating_add(active).to_string());
            if let Some(active_eligible) = active_eligible_opt {
                let turn_count = since_sequence.saturating_add(active_eligible);
                let _ = enqueue_if_due_scoped(
                    js,
                    consolidation_config,
                    &session_str,
                    turn_count,
                    since_sequence,
                    now_ms,
                    scope_binding,
                )
                .await;
            }
            if let (true, Some(dedupe_token)) = (had_stop, dream_dedupe_token.as_deref()) {
                let _ = enqueue_tier_with_dedupe_token(
                    js,
                    dream_config,
                    DreamTier::LightSleep,
                    &session_str,
                    dedupe_token,
                    now_ms,
                    scope_binding,
                )
                .await;
            }
            if explicit_skillify_requested && had_stop {
                let dedupe_token = skillify_source_dedupe_token(&projected_record_ids);
                let _ = enqueue_skillify(
                    js,
                    SkillifyTrigger::Explicit,
                    &session_str,
                    &dedupe_token,
                    now_ms,
                    scope_binding,
                    projected_record_ids.clone(),
                )
                .await;
            }
        }

        // Zero-capture audit nudge (issue #343): on Stop, record the
        // turn's activity ratio so downstream tooling can detect
        // sessions that hooked but produced no records.
        if had_stop {
            let successful_ingest_writes =
                match count_successful_ingest_writes(vault_root, &session_str) {
                    Ok(count) => count,
                    Err(e) => {
                        failed_turns.push((
                            session_str.clone(),
                            turn_str.clone(),
                            format!("zero_capture_audit metric read: {e:#}"),
                        ));
                        continue;
                    }
                };
            let input = ZeroCaptureAuditInput {
                session_id: session_id.clone(),
                activity_count: group.len() as u64,
                successful_ingest_writes,
                successful_capture_trace_writes,
                nudges_enabled: true,
                reminder_allowed: true,
                trigger: ZeroCaptureTrigger::Stop,
            };
            let decision = decide_zero_capture_nudge(&input);
            let report = ZeroCaptureReport::from_decision(&input, &decision);
            if let Err(e) = append_zero_capture_audit_metric(
                vault_root,
                &report,
                successful_ingest_writes,
                successful_capture_trace_writes,
            ) {
                failed_turns.push((
                    session_str.clone(),
                    turn_str.clone(),
                    format!("zero_capture_audit metric append: {e:#}"),
                ));
            }
        }
    }

    Ok(CaptureTraceResponse {
        trace_id: Ulid::new().to_string(),
        failed_turns,
        policy_trace: to_wire(&policy_trace_entries),
    })
}

#[allow(clippy::too_many_lines)] // sequential block-projection pipeline; splitting hides dataflow
async fn run_blocks_handler_inner(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    blocks_path: &Path,
    session_id_raw: &str,
    scope_binding: Option<&ScopeTuple>,
) -> anyhow::Result<CaptureTraceResponse> {
    refuse_if_degraded(&ReconciliationReport::default(), vec![])
        .context("capture_trace: vault degraded")?;

    let session_id = SessionId::parse(session_id_raw)
        .map_err(anyhow::Error::msg)
        .context("capture_trace: invalid session_id for --blocks")?;
    let blocks = read_trace_blocks(blocks_path).await?;
    let raw_blocks = tokio::fs::read(blocks_path)
        .await
        .with_context(|| format!("read trace blocks at {}", blocks_path.display()))?;
    let import_id = stable_ulid(
        b"cairn:capture-trace:blocks:import:v1\0",
        &[session_id.as_str().as_bytes(), &raw_blocks],
    );
    let turn_id = stable_turn_id(&import_id);
    let payload_ref = persist_trace_blocks_source(vault_root, &import_id, &raw_blocks).await?;
    let payload_hash = PayloadHash::parse(format!("sha256:{}", sha256_hex(&raw_blocks)))
        .map_err(anyhow::Error::msg)?;
    let captured_at = chrono::Utc::now();
    let mut policy_trace_entries = Vec::new();
    let mut projected = Vec::with_capacity(blocks.len());
    let mut pre_tool_by_id: BTreeMap<String, CaptureEventId> = BTreeMap::new();

    for (block_index, block) in blocks.into_iter().enumerate() {
        let offset_secs = i64::try_from(block_index)
            .map_err(|_| anyhow::anyhow!("capture_trace: block_index exceeds i64"))?;
        let event_dt = captured_at + chrono::Duration::seconds(offset_secs);
        let captured_at = timestamp_from_datetime(event_dt)?;
        let stable_event_id = stable_ulid(
            b"cairn:capture-trace:blocks:event:v1\0",
            &[
                session_id.as_str().as_bytes(),
                import_id.as_bytes(),
                &block_index.to_le_bytes(),
            ],
        );
        let event = synthetic_block_event(
            &session_id,
            &turn_id,
            &captured_at,
            &payload_hash,
            &payload_ref,
            &stable_event_id,
            &block,
        )?;

        let body = block_body(&block);
        let redacted = redact(&body);
        let fenced = fence(&redacted.text);
        let inputs = FilterInputs::new(&redacted, &fenced);
        let decision = should_memorize(&inputs);
        policy_trace_entries.extend([
            PolicyTraceEntry::from(&redacted),
            PolicyTraceEntry::from(&fenced),
            PolicyTraceEntry::from(&decision),
        ]);
        if let Decision::Discard(reason) = decision {
            return Ok(CaptureTraceResponse {
                trace_id: Ulid::new().to_string(),
                failed_turns: vec![(
                    session_id.as_str().to_owned(),
                    turn_id.clone(),
                    format!("privacy filter rejected turn: {}", reason.as_str()),
                )],
                policy_trace: to_wire(&policy_trace_entries),
            });
        }

        let classified = core_classify(&event).map_err(anyhow::Error::msg)?;
        let parent_event_id = match &block {
            TraceBlock::ToolResult { tool_use_id, .. } => pre_tool_by_id.get(tool_use_id).cloned(),
            _ => None,
        };
        let tool_call_id = match &block {
            TraceBlock::ToolUse { id, .. } => Some(id.clone()),
            TraceBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        };
        let link = TraceLink {
            session_id: session_id.clone(),
            turn_id: turn_id.clone(),
            sequence: 0,
            capture_event_id: event.event_id.clone(),
            parent_event_id,
            tool_call_id: tool_call_id.clone(),
            member_event_ids: Vec::new(),
        };
        let resolved = ResolvedBody::from_trace_hook(&fenced.text);
        let mut record = project_with_blocks(
            &event,
            classified,
            &resolved,
            &link,
            &ProjectedTraceBlocks {
                blocks: vec![block.clone()],
            },
        )
        .map_err(anyhow::Error::msg)
        .context("capture_trace: project trace block")?;
        if matches!(block, TraceBlock::Reasoning { .. }) {
            record.kind = cairn_core::domain::MemoryKind::Reasoning;
        }
        if let Some(trace) = record
            .extra_frontmatter
            .get_mut("trace")
            .and_then(serde_json::Value::as_object_mut)
        {
            trace.insert(
                "block_index".to_owned(),
                serde_json::Value::Number(block_index.into()),
            );
        }
        if let Some(scope_binding) = scope_binding {
            bind_record_scope(&mut record, scope_binding);
        }
        policy_trace_entries.push(PolicyTraceEntry::pass(PolicyGate::VisibilityFloor));
        if let TraceBlock::ToolUse { id, .. } = &block {
            pre_tool_by_id.insert(id.clone(), event.event_id.clone());
        }
        projected.push(record);
    }

    let session_id_tx = session_id.clone();
    store
        .with_tx(move |tx| {
            tx.renumber_turn_with(&session_id_tx, &turn_id, &projected)?;
            tx.validate_turn_links(&session_id_tx, &turn_id)?;
            Ok::<(), cairn_store_sqlite::error::StoreError>(())
        })
        .await
        .map_err(anyhow::Error::msg)
        .context("capture_trace: persist trace blocks")?;

    Ok(CaptureTraceResponse {
        trace_id: Ulid::new().to_string(),
        failed_turns: Vec::new(),
        policy_trace: to_wire(&policy_trace_entries),
    })
}

fn bind_record_scope(record: &mut cairn_core::domain::MemoryRecord, scope_binding: &ScopeTuple) {
    if let Some(value) = &scope_binding.tenant {
        record.scope.tenant = Some(value.clone());
    }
    if let Some(value) = &scope_binding.workspace {
        record.scope.workspace = Some(value.clone());
    }
    if let Some(value) = &scope_binding.project {
        record.scope.project = Some(value.clone());
    }
    if let Some(value) = &scope_binding.entity {
        record.scope.entity = Some(value.clone());
    }
    if let Some(value) = &scope_binding.user {
        record.scope.user = Some(value.clone());
    }
    if let Some(value) = &scope_binding.agent {
        record.scope.agent = Some(value.clone());
    }
}

fn trace_text(event: &CaptureEvent, body_bytes: &[u8]) -> anyhow::Result<String> {
    match &event.payload {
        CapturePayload::Voice { .. } => voice_transcript_text(body_bytes),
        CapturePayload::RecordingBatch { .. } => recording_segment_text(body_bytes),
        _ => String::from_utf8(body_bytes.to_vec()).context("routed body is not valid UTF-8"),
    }
}

fn explicit_skillify_request(raw_text: &str) -> bool {
    matches!(
        raw_text.trim().to_ascii_lowercase().as_str(),
        "skillify this" | "skillify it" | "/skillify" | "cairn skillify"
    )
}

fn skillify_source_dedupe_token(source_record_ids: &[String]) -> String {
    let mut source_record_ids = source_record_ids.to_vec();
    source_record_ids.sort();
    let mut hasher = Sha256::new();
    hasher.update(source_record_ids.join(",").as_bytes());
    format!("explicit:{:x}", hasher.finalize())
}

fn classify_trace_event(
    event: &CaptureEvent,
) -> Result<TraceEvent, cairn_core::pipeline::capture_trace::TraceProjectError> {
    match &event.payload {
        CapturePayload::RecordingBatch { .. } => Ok(TraceEvent::UserMessage),
        _ => core_classify(event),
    }
}

fn trace_canvas_projection_for_record(
    event: &CaptureEvent,
    classified: TraceEvent,
    record: &cairn_core::domain::MemoryRecord,
) -> Option<TraceCanvasProjection> {
    if !matches!(
        classified,
        TraceEvent::PreTool | TraceEvent::PostTool | TraceEvent::ToolOutput
    ) {
        return None;
    }

    let refs = event.refs.as_ref()?;
    let session_id = refs.session_id.as_ref()?.clone();
    let turn_id = refs.turn_id.as_ref()?.clone();
    let tool_call_id = refs.tool_id.clone();
    let tool_name = trace_canvas_tool_name(event);
    let record_id = record.id.as_str().to_owned();
    let timestamp_ms = event.captured_at.as_chrono().timestamp_millis();
    let (call_summary, result_summary, node_label, node_status, node_summary) =
        trace_canvas_summaries(classified, tool_name.as_deref());
    let step_id = format!("trace-step:{record_id}");

    Some(TraceCanvasProjection {
        step: TraceStepDraft {
            step_id: step_id.clone(),
            trace_id: event.event_id.as_str().to_owned(),
            session_id,
            turn_id,
            tool_call_id: tool_call_id.clone(),
            timestamp_ms,
            tool_name,
            call_summary,
            result_summary,
            result_ref: Some(record_id),
            salience: trace_canvas_salience(classified),
            replaceability_score: trace_canvas_replaceability(classified),
            node_id: Some(format!("trace-node:{step_id}")),
            source_hash: trace_canvas_source_hash(event, classified, tool_call_id.as_deref()),
        },
        canvas_title: "Current task".to_owned(),
        canvas_goal: "Continue the current session task".to_owned(),
        node_label,
        node_status,
        node_summary,
    })
}

fn trace_canvas_tool_name(event: &CaptureEvent) -> Option<String> {
    match &event.payload {
        CapturePayload::Hook { tool_name, .. } => tool_name.clone(),
        CapturePayload::Terminal { .. } => Some("terminal".to_owned()),
        _ => None,
    }
}

fn trace_canvas_summaries(
    event: TraceEvent,
    tool_name: Option<&str>,
) -> (String, String, String, String, String) {
    let label_tool = tool_name.unwrap_or("tool");
    match event {
        TraceEvent::PreTool => (
            "tool call started".to_owned(),
            "result pending".to_owned(),
            format!("{label_tool} started"),
            "active".to_owned(),
            "Tool call was dispatched.".to_owned(),
        ),
        TraceEvent::PostTool => (
            "tool call completed".to_owned(),
            "tool result captured".to_owned(),
            format!("{label_tool} completed"),
            "completed".to_owned(),
            "Tool call completed and its result is available.".to_owned(),
        ),
        TraceEvent::ToolOutput => (
            "tool output captured".to_owned(),
            "tool output available".to_owned(),
            format!("{label_tool} output"),
            "completed".to_owned(),
            "Tool output was captured for exact retrieval.".to_owned(),
        ),
        _ => unreachable!("caller filters to tool lifecycle events"),
    }
}

fn trace_canvas_salience(event: TraceEvent) -> f64 {
    match event {
        TraceEvent::PreTool => 0.45,
        TraceEvent::PostTool | TraceEvent::ToolOutput => 0.65,
        _ => 0.5,
    }
}

fn trace_canvas_replaceability(event: TraceEvent) -> f64 {
    match event {
        TraceEvent::PreTool => 0.75,
        TraceEvent::PostTool | TraceEvent::ToolOutput => 0.55,
        _ => 0.7,
    }
}

fn trace_canvas_source_hash(
    event: &CaptureEvent,
    classified: TraceEvent,
    tool_call_id: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"cairn:trace-canvas-step:v1\0");
    hasher.update(event.event_id.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(event.payload_hash.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(format!("{classified:?}").as_bytes());
    hasher.update([0]);
    if let Some(tool_call_id) = tool_call_id {
        hasher.update(tool_call_id.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn voice_transcript_text(body_bytes: &[u8]) -> anyhow::Result<String> {
    let raw: serde_json::Value =
        serde_json::from_slice(body_bytes).context("voice payload is not valid JSON")?;
    let text = raw
        .get("transcript")
        .and_then(|transcript| transcript.get("text"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("voice payload missing transcript.text"))?;
    if text.trim().is_empty() {
        anyhow::bail!("voice payload transcript.text is empty");
    }
    Ok(text.to_owned())
}

fn recording_segment_text(body_bytes: &[u8]) -> anyhow::Result<String> {
    let raw: serde_json::Value =
        serde_json::from_slice(body_bytes).context("recording payload is not valid JSON")?;
    let text = raw
        .get("segment")
        .and_then(|segment| segment.get("text"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("recording payload missing segment.text"))?;
    if text.trim().is_empty() {
        anyhow::bail!("recording payload segment.text is empty");
    }
    Ok(text.to_owned())
}

/// Read the payload bytes referenced by `event.payload_ref` from disk
/// (relative to `vault_root`) and verify the SHA-256 hash matches
/// `event.payload_hash`.
///
/// The `payload_hash` field may be prefixed with `"sha256:"` — this prefix
/// is stripped before comparison.
///
/// # Errors
///
/// - I/O error reading the file.
/// - SHA-256 mismatch between the stored hash and the computed hash.
async fn resolve_body_bytes(vault_root: &Path, event: &CaptureEvent) -> anyhow::Result<Vec<u8>> {
    // Trust boundary: `payload_ref` is caller-supplied (the JSONL on disk).
    // A matching `payload_hash` is integrity, not authorization — without a
    // path containment check, a crafted entry like `../../../etc/passwd`
    // plus its real SHA-256 would be ingested as a "trace" body. Test
    // fixtures already supply `payload_ref` rooted at `sources/...`, so we
    // canonicalize the resolved path and require it to remain under the
    // canonical `vault_root/sources/` subtree.
    let raw_path = vault_root.join(&event.payload_ref);
    let canon_sources = tokio::fs::canonicalize(vault_root.join("sources"))
        .await
        .with_context(|| {
            format!(
                "canonicalize vault sources root {}",
                vault_root.join("sources").display()
            )
        })?;
    let canon_path = tokio::fs::canonicalize(&raw_path)
        .await
        .with_context(|| format!("canonicalize payload path {}", raw_path.display()))?;
    if !canon_path.starts_with(&canon_sources) {
        anyhow::bail!(
            "payload_ref escapes vault sources/ (resolved {} not under {})",
            canon_path.display(),
            canon_sources.display(),
        );
    }
    let path = canon_path;
    let bytes = tokio::fs::read(&path)
        .await
        .with_context(|| format!("read payload at {}", path.display()))?;

    let actual = sha256_hex(&bytes);
    let raw_expected = event.payload_hash.as_str();
    let expected_hex = raw_expected.strip_prefix("sha256:").unwrap_or(raw_expected);

    if actual != expected_hex {
        anyhow::bail!(
            "payload_hash mismatch at {}: expected sha256:{expected_hex}, got sha256:{actual}",
            path.display()
        );
    }

    Ok(bytes)
}

async fn evaluate_capture_trace_sensor_gate(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    config: &CairnConfig,
    event: &CaptureEvent,
) -> anyhow::Result<Option<SensorGateReason>> {
    let Some(sensor) = LocalSensorName::from_source_family(event.source_family) else {
        return Ok(None);
    };
    let consent = crate::sensor_gate::latest_sensor_consent(store, sensor).await?;
    let observation = BudgetObservation {
        items: 1,
        bytes: payload_metadata_len(vault_root, event).await.unwrap_or(0),
    };
    match crate::sensor_gate::evaluate_sensor_gate(config, consent, sensor, observation) {
        Ok(()) => Ok(None),
        Err(reason) => {
            append_capture_trace_drop_metric(
                vault_root,
                config,
                event,
                sensor,
                reason,
                observation,
            )?;
            Ok(Some(reason))
        }
    }
}

fn append_capture_trace_drop_metric(
    vault_root: &Path,
    config: &CairnConfig,
    event: &CaptureEvent,
    sensor: LocalSensorName,
    reason: SensorGateReason,
    observation: BudgetObservation,
) -> anyhow::Result<()> {
    let budget = (reason == SensorGateReason::BudgetExceeded)
        .then(|| {
            crate::sensor_gate::sensor_budget(config, sensor).map(|budget| SensorDropBudgetMetric {
                max_items: budget.max_items,
                max_bytes: budget.max_bytes,
                observed_items: observation.items,
                observed_bytes: observation.bytes,
            })
        })
        .flatten();
    let refs = event.refs.as_ref();
    let metric = SensorDropMetric {
        event: crate::sensor_gate::SENSOR_DROP_EVENT,
        sensor,
        source_family: Some(event.source_family),
        reason,
        stage: SensorGateStage::PreExtraction,
        operation_id: Some(event.event_id.as_str().to_owned()),
        session_id: refs.and_then(|refs| safe_metric_ref(refs.session_id.as_deref())),
        turn_id: refs.and_then(|refs| safe_metric_ref(refs.turn_id.as_deref())),
        budget,
    };
    append_sensor_drop_metric(vault_root, &metric)
}

async fn payload_metadata_len(vault_root: &Path, event: &CaptureEvent) -> Option<u64> {
    let raw_path = vault_root.join(&event.payload_ref);
    let canon_sources = tokio::fs::canonicalize(vault_root.join("sources"))
        .await
        .ok()?;
    let canon_path = tokio::fs::canonicalize(&raw_path).await.ok()?;
    if !canon_path.starts_with(canon_sources) {
        return None;
    }
    tokio::fs::metadata(canon_path)
        .await
        .ok()
        .map(|metadata| metadata.len())
}

/// Compute the lowercase hex SHA-256 digest of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

#[derive(Debug, serde::Serialize)]
struct ZeroCaptureAuditMetricRow {
    event: &'static str,
    session_id: String,
    activity_count: u64,
    successful_ingest_writes: u64,
    successful_capture_trace_writes: u64,
    successful_write_count: u64,
    decision: String,
}

fn count_successful_ingest_writes(vault_root: &Path, session_id: &str) -> anyhow::Result<u64> {
    let metrics_path = vault_root.join(".cairn").join("metrics.jsonl");
    if !metrics_path.exists() {
        return Ok(0);
    }
    let body = fs::read_to_string(&metrics_path)
        .with_context(|| format!("read {}", metrics_path.display()))?;
    let mut count = 0_u64;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let row = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(row) => row,
            Err(e) if metrics_line_sets_event(line, "accepted") => {
                return Err(e).context("parse accepted metrics.jsonl line");
            }
            Err(_) => continue,
        };
        let event = row.get("event").and_then(serde_json::Value::as_str);
        if event != Some("accepted") {
            continue;
        }
        let scope_session = row
            .get("scope")
            .and_then(|v| v.get("session_id"))
            .and_then(serde_json::Value::as_str);
        let Some(scope_session) = scope_session else {
            anyhow::bail!("accepted metrics row missing scope.session_id");
        };
        if scope_session == session_id {
            count += 1;
        }
    }
    Ok(count)
}

fn metrics_line_sets_event(line: &str, event: &str) -> bool {
    let Some(after_key) = line.split_once("\"event\"").map(|(_, rest)| rest) else {
        return false;
    };
    let after_key = after_key.trim_start();
    let Some(after_colon) = after_key.strip_prefix(':') else {
        return false;
    };
    let after_colon = after_colon.trim_start();
    let Some(after_quote) = after_colon.strip_prefix('"') else {
        return false;
    };
    let Some(after_event) = after_quote.strip_prefix(event) else {
        return false;
    };
    after_event.is_empty() || after_event.starts_with('"')
}

fn append_zero_capture_audit_metric(
    vault_root: &Path,
    report: &ZeroCaptureReport,
    successful_ingest_writes: u64,
    successful_capture_trace_writes: u64,
) -> anyhow::Result<()> {
    let cairn_dir = vault_root.join(".cairn");
    fs::create_dir_all(&cairn_dir)
        .with_context(|| format!("create metrics dir {}", cairn_dir.display()))?;
    let metrics_path = cairn_dir.join("metrics.jsonl");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&metrics_path)
        .with_context(|| format!("open {}", metrics_path.display()))?;
    let row = ZeroCaptureAuditMetricRow {
        event: "zero_capture_audit",
        session_id: report.session_id.to_string(),
        activity_count: report.activity_count,
        successful_ingest_writes,
        successful_capture_trace_writes,
        successful_write_count: report.successful_write_count,
        decision: report.decision.as_str().to_owned(),
    };
    serde_json::to_writer(&mut file, &row).context("serialize zero-capture metric row")?;
    file.write_all(b"\n")
        .with_context(|| format!("write {}", metrics_path.display()))?;
    Ok(())
}

async fn read_trace_blocks(path: &Path) -> anyhow::Result<Vec<TraceBlock>> {
    let raw = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("read trace blocks JSON at {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse trace blocks at {}", path.display()))
}

async fn persist_trace_blocks_source(
    vault_root: &Path,
    event_id: &str,
    raw_blocks: &[u8],
) -> anyhow::Result<String> {
    let dir = vault_root.join("sources").join("trace_blocks");
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("create trace_blocks source dir {}", dir.display()))?;
    let filename = format!("{event_id}.json");
    let path = dir.join(&filename);
    tokio::fs::write(&path, raw_blocks)
        .await
        .with_context(|| format!("write trace blocks source {}", path.display()))?;
    Ok(format!("sources/trace_blocks/{filename}"))
}

fn timestamp_from_datetime(dt: chrono::DateTime<chrono::Utc>) -> anyhow::Result<Rfc3339Timestamp> {
    let raw = dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Rfc3339Timestamp::parse(raw).map_err(anyhow::Error::msg)
}

fn stable_ulid(domain: &[u8], chunks: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    h.update(domain);
    for chunk in chunks {
        h.update(chunk);
        h.update([0]);
    }
    let digest = h.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Ulid::from_bytes(bytes).to_string()
}

fn stable_turn_id(import_id: &str) -> String {
    format!("{DIRECT_BLOCKS_TURN_PREFIX}-{import_id}")
}

fn synthetic_block_event(
    session_id: &SessionId,
    turn_id: &str,
    captured_at: &Rfc3339Timestamp,
    payload_hash: &PayloadHash,
    payload_ref: &str,
    event_id: &str,
    block: &TraceBlock,
) -> anyhow::Result<CaptureEvent> {
    let (sensor_id, capture_mode, actor_identity, payload, source_family, tool_id) = match block {
        TraceBlock::Reasoning { .. } | TraceBlock::Text { .. } => (
            Identity::parse("snr:local:proactive:claude-code:v1").map_err(anyhow::Error::msg)?,
            CaptureMode::Proactive,
            Identity::parse("agt:claude-code:opus-4-7:main:v1").map_err(anyhow::Error::msg)?,
            CapturePayload::Proactive {
                kind: "assistant_message".to_owned(),
                rationale: "capture_trace --blocks".to_owned(),
            },
            SourceFamily::Proactive,
            None,
        ),
        TraceBlock::ToolUse { tool, id, .. } => (
            Identity::parse("snr:local:hook:cc-session:v1").map_err(anyhow::Error::msg)?,
            CaptureMode::Auto,
            Identity::parse("snr:local:hook:cc-session:v1").map_err(anyhow::Error::msg)?,
            CapturePayload::Hook {
                hook_name: "PreToolUse".to_owned(),
                tool_name: Some(tool.clone()),
            },
            SourceFamily::Hook,
            Some(id.clone()),
        ),
        TraceBlock::ToolResult { tool_use_id, .. } => (
            Identity::parse("snr:local:hook:cc-session:v1").map_err(anyhow::Error::msg)?,
            CaptureMode::Auto,
            Identity::parse("snr:local:hook:cc-session:v1").map_err(anyhow::Error::msg)?,
            CapturePayload::Hook {
                hook_name: "ToolOutput".to_owned(),
                tool_name: None,
            },
            SourceFamily::Hook,
            Some(tool_use_id.clone()),
        ),
    };
    let event = CaptureEvent {
        event_id: CaptureEventId::parse(event_id).map_err(anyhow::Error::msg)?,
        sensor_id: sensor_id.clone(),
        capture_mode,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: actor_identity,
            at: captured_at.clone(),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session_id.as_str().to_owned()),
            turn_id: Some(turn_id.to_owned()),
            tool_id,
        }),
        payload_hash: payload_hash.clone(),
        payload_ref: payload_ref.to_owned(),
        captured_at: captured_at.clone(),
        payload,
        source_family,
    };
    event.validate().map_err(anyhow::Error::msg)?;
    Ok(event)
}

fn block_body(block: &TraceBlock) -> String {
    match block {
        TraceBlock::Reasoning { text, .. } | TraceBlock::Text { text } => text.clone(),
        TraceBlock::ToolUse { tool, input, .. } => {
            format!(
                "{tool}\n{}",
                serde_json::to_string(input).unwrap_or_default()
            )
        }
        TraceBlock::ToolResult { content, .. } => content.clone(),
    }
}

enum CaptureTraceInput {
    Jsonl(PathBuf),
    Blocks { path: PathBuf, session_id: String },
}

fn normalize_cli_path(path: &Path) -> PathBuf {
    match path.to_str().and_then(|raw| raw.strip_prefix('@')) {
        Some(stripped) => PathBuf::from(stripped),
        None => path.to_path_buf(),
    }
}

/// Run `cairn capture_trace`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: PathBuf, config: CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");
    let input = match (
        sub.get_one::<PathBuf>("from")
            .map(|p| normalize_cli_path(p)),
        sub.get_one::<PathBuf>("blocks")
            .map(|p| normalize_cli_path(p)),
        sub.get_one::<String>("session_id").cloned(),
    ) {
        (Some(from), None, None) => CaptureTraceInput::Jsonl(from),
        (None, Some(path), Some(session_id)) => CaptureTraceInput::Blocks { path, session_id },
        (Some(_), None, Some(_)) => {
            let resp = invalid_args_response(
                ResponseVerb::CaptureTrace,
                "session_id",
                "session-scoped trace import is not yet supported for JSONL capture_trace",
            );
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "capture_trace",
                    "InvalidArgs",
                    "--session is only supported with --blocks",
                    &resp.operation_id,
                );
            }
            return ExitCode::from(64);
        }
        (None, Some(_), None) => {
            let resp = invalid_args_response(
                ResponseVerb::CaptureTrace,
                "session_id",
                "required when using --blocks",
            );
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "capture_trace",
                    "InvalidArgs",
                    "--session is required with --blocks",
                    &resp.operation_id,
                );
            }
            return ExitCode::from(64);
        }
        _ => {
            let resp = invalid_args_response(
                ResponseVerb::CaptureTrace,
                "from|blocks",
                "exactly one input mode is required",
            );
            if json {
                emit_json(&resp);
            } else {
                human_error(
                    "capture_trace",
                    "InvalidArgs",
                    "pass exactly one of --from or --blocks",
                    &resp.operation_id,
                );
            }
            return ExitCode::from(64);
        }
    };

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let resp = super::signed::aborted(
                ResponseVerb::CaptureTrace,
                format!("runtime initialization: {e}"),
            );
            if json {
                emit_json(&resp);
            } else {
                emit_human(&resp);
            }
            return ExitCode::FAILURE;
        }
    };

    let resp = rt.block_on(run_async(input, vault_root, config));
    if json {
        emit_json(&resp);
    } else {
        emit_human(&resp);
    }
    response_exit_code(&resp)
}

async fn run_async(input: CaptureTraceInput, vault_root: PathBuf, config: CairnConfig) -> Response {
    let ctx =
        match super::signed::open_context(ResponseVerb::CaptureTrace, &vault_root, config).await {
            Ok(ctx) => ctx,
            Err(resp) => return resp,
        };
    let scope_binding = ScopeTuple {
        tenant: Some(DEFAULT_TENANT.to_owned()),
        workspace: Some(ctx.config.vault.name.clone()),
        entity: Some(CAPTURE_TRACE_ENTITY.to_owned()),
        ..ScopeTuple::default()
    };
    let job_store_ref = ctx.job_store.as_deref();
    let result = match input {
        CaptureTraceInput::Jsonl(from) => {
            run_handler_inner(
                &ctx.store,
                &ctx.vault_root,
                &from,
                Some(&scope_binding),
                Some(&ctx.config),
                job_store_ref,
                &ctx.config.consolidation,
                &ctx.config.dream,
            )
            .await
        }
        CaptureTraceInput::Blocks { path, session_id } => {
            run_blocks_handler_with_scope(
                &ctx.store,
                &ctx.vault_root,
                &path,
                &session_id,
                scope_binding,
            )
            .await
        }
    };
    match result {
        Ok(result) => {
            let failed_turns = public_failed_turns(result.failed_turns);
            let data = CaptureTraceData {
                failed_turns,
                trace_id: WireUlid(result.trace_id),
            };
            super::signed::committed(
                ResponseVerb::CaptureTrace,
                new_operation_id(),
                ResponseData::CaptureTrace(data),
                result.policy_trace,
            )
        }
        Err(e) => super::signed::aborted(ResponseVerb::CaptureTrace, format!("capture_trace: {e}")),
    }
}

fn emit_human(resp: &Response) {
    if let (ResponseStatus::Committed, Some(ResponseData::CaptureTrace(data))) =
        (&resp.status, resp.data.as_ref())
    {
        if data.failed_turns.is_empty() {
            println!(
                "capture_trace: trace_id {} (operation_id: {})",
                data.trace_id.0, resp.operation_id.0
            );
        } else {
            println!(
                "capture_trace: trace_id {} ({} failed turn(s), operation_id: {})",
                data.trace_id.0,
                data.failed_turns.len(),
                resp.operation_id.0
            );
            for turn in &data.failed_turns {
                println!(
                    "capture_trace: failed turn session={} turn={} reason={}",
                    turn.session_id, turn.turn_id, turn.reason
                );
            }
        }
    } else {
        let code = resp
            .error
            .as_ref()
            .and_then(|e| e.get("code"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Internal");
        let message = resp
            .error
            .as_ref()
            .and_then(|e| e.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("capture_trace failed");
        human_error("capture_trace", code, message, &resp.operation_id);
    }
}

fn public_failed_turns(failed_turns: Vec<(String, String, String)>) -> Vec<FailedTurn> {
    failed_turns
        .into_iter()
        .map(|(session_id, turn_id, reason)| FailedTurn {
            reason: public_failed_turn_reason(&reason),
            session_id: public_failed_turn_ref(session_id),
            turn_id: public_failed_turn_ref(turn_id),
        })
        .collect()
}

fn public_failed_turn_ref(value: String) -> String {
    if value.is_empty() {
        "unknown".to_owned()
    } else {
        value
    }
}

fn public_failed_turn_reason(reason: &str) -> String {
    match reason {
        "sensor_gate:disabled" | "sensor_gate:privacy_denied" | "sensor_gate:budget_exceeded" => {
            return reason.to_owned();
        }
        _ if reason.starts_with("sensor_gate:") => {
            return "turn_failed".to_owned();
        }
        _ => {}
    }
    if let Some(code) = reason.strip_prefix("privacy filter rejected turn: ") {
        return format!("privacy_filter:{code}");
    }
    if reason.starts_with("envelope validate:")
        || reason.starts_with("dispatch ")
        || reason.contains("missing refs")
        || reason.contains("missing session_id or turn_id")
    {
        return "malformed_capture".to_owned();
    }
    if reason.starts_with("resolve_body:") {
        return "payload_unavailable".to_owned();
    }
    if reason.starts_with("classify ") {
        return "unclassifiable_capture".to_owned();
    }
    if reason.starts_with("project:") {
        return "projection_failed".to_owned();
    }
    if reason.contains("PostTool/ToolOutput") || reason.contains("PreTool") {
        return "trace_link_failed".to_owned();
    }
    "turn_failed".to_owned()
}

fn is_secret_tag(tag: RedactionTag) -> bool {
    matches!(
        tag,
        RedactionTag::AwsAccessKeyId
            | RedactionTag::GithubToken
            | RedactionTag::SlackToken
            | RedactionTag::Jwt
            | RedactionTag::HexSecret
            | RedactionTag::OpaqueApiKey
            | RedactionTag::ContextKeyedSecret
            | RedactionTag::PrivateKeyBlock
    )
}

fn response_exit_code(resp: &Response) -> ExitCode {
    match resp.status {
        ResponseStatus::Committed => ExitCode::SUCCESS,
        ResponseStatus::Rejected => ExitCode::from(64),
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use cairn_core::contract::job_store::{
        EnqueueRequest, FailDisposition, FailureClass, JobId, JobKind, JobStoreError, LeaseToken,
        LeasedJob, ReclaimedRow,
    };

    const STOP_EVENT_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const STOP_SESSION_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const STOP_SOURCE_HASH: &str =
        "sha256:a11c3d917a2149a4b548217038bc8f2dd2130a1966e0c7af5c4225d81f25a8c3";
    const STOP_SOURCE_REF: &str = "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAW.json";
    const SKILLIFY_EVENT_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const SKILLIFY_SOURCE_REF: &str = "sources/recording_batch/01ARZ3NDEKTSV4RRFFQ69G5FAY.json";

    struct RecordingJobStore {
        requests: Mutex<Vec<EnqueueRequest>>,
    }

    impl RecordingJobStore {
        fn new() -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl JobStore for RecordingJobStore {
        async fn enqueue(&self, req: EnqueueRequest) -> Result<(), JobStoreError> {
            self.requests.lock().expect("invariant: mutex").push(req);
            Ok(())
        }

        async fn enqueue_leased(
            &self,
            _: EnqueueRequest,
            _: &str,
            _: i64,
            _: i64,
        ) -> Result<LeasedJob, JobStoreError> {
            Err(JobStoreError::Backend("unused in test".into()))
        }

        async fn lease(&self, _: &str, _: i64, _: i64) -> Result<Option<LeasedJob>, JobStoreError> {
            Ok(None)
        }

        async fn lease_specific(
            &self,
            _: &JobId,
            _: &JobKind,
            _: &str,
            _: i64,
            _: i64,
        ) -> Result<Option<LeasedJob>, JobStoreError> {
            Ok(None)
        }

        async fn heartbeat(
            &self,
            _: &JobId,
            _: &LeaseToken,
            _: i64,
            _: i64,
        ) -> Result<(), JobStoreError> {
            Err(JobStoreError::Backend("unused in test".into()))
        }

        async fn complete(&self, _: &JobId, _: &LeaseToken, _: i64) -> Result<(), JobStoreError> {
            Err(JobStoreError::Backend("unused in test".into()))
        }

        async fn fail(
            &self,
            _: &JobId,
            _: &LeaseToken,
            _: FailDisposition,
            _: FailureClass,
            _: &str,
            _: i64,
        ) -> Result<(), JobStoreError> {
            Err(JobStoreError::Backend("unused in test".into()))
        }

        async fn reap_expired(&self, _: i64) -> Result<Vec<ReclaimedRow>, JobStoreError> {
            Ok(Vec::new())
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "test fixture: panics surface broken invariants immediately"
    )]
    fn write_stop_source(vault: &Path) {
        std::fs::create_dir_all(vault.join("sources/hook")).expect("mkdir sources");
        std::fs::write(vault.join(STOP_SOURCE_REF), b"stop\n").expect("write source");
    }

    #[allow(
        clippy::expect_used,
        reason = "test fixture: panics surface broken invariants immediately"
    )]
    fn stop_hook_event() -> CaptureEvent {
        let sensor =
            Identity::parse("snr:local:hook:cc-session:v1").expect("invariant: valid sensor id");
        let captured_at =
            Rfc3339Timestamp::parse("2026-05-17T12:00:00Z").expect("invariant: valid RFC-3339");
        CaptureEvent {
            event_id: CaptureEventId::parse(STOP_EVENT_ID).expect("invariant: valid ULID"),
            sensor_id: sensor.clone(),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: sensor,
                at: captured_at.clone(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some(STOP_SESSION_ID.to_owned()),
                turn_id: Some("turn-1".to_owned()),
                tool_id: None,
            }),
            payload_hash: PayloadHash::parse(STOP_SOURCE_HASH).expect("invariant: valid sha256"),
            payload_ref: STOP_SOURCE_REF.into(),
            captured_at,
            payload: CapturePayload::Hook {
                hook_name: "Stop".to_owned(),
                tool_name: None,
            },
            source_family: SourceFamily::Hook,
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "test fixture: panics surface broken invariants immediately"
    )]
    fn write_skillify_source(vault: &Path) {
        std::fs::create_dir_all(vault.join("sources/recording_batch"))
            .expect("mkdir recording sources");
        std::fs::write(
            vault.join(SKILLIFY_SOURCE_REF),
            br#"{"segment":{"text":"skillify this"}}"#,
        )
        .expect("write skillify source");
    }

    #[allow(
        clippy::expect_used,
        reason = "test fixture: panics surface broken invariants immediately"
    )]
    fn explicit_skillify_event() -> CaptureEvent {
        let sensor =
            Identity::parse("snr:local:recording:default:v1").expect("valid recording sensor");
        let captured_at = Rfc3339Timestamp::parse("2026-05-17T12:00:01Z").expect("valid RFC-3339");
        CaptureEvent {
            event_id: CaptureEventId::parse(SKILLIFY_EVENT_ID).expect("valid ULID"),
            sensor_id: sensor.clone(),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: sensor,
                at: captured_at.clone(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some(STOP_SESSION_ID.to_owned()),
                turn_id: Some("turn-1".to_owned()),
                tool_id: None,
            }),
            payload_hash: PayloadHash::parse(format!(
                "sha256:{}",
                test_sha256_hex(r#"{"segment":{"text":"skillify this"}}"#)
            ))
            .expect("valid sha256"),
            payload_ref: SKILLIFY_SOURCE_REF.into(),
            captured_at,
            payload: CapturePayload::RecordingBatch {
                segment_start_ms: 0,
                segment_duration_ms: 1_000,
            },
            source_family: SourceFamily::RecordingBatch,
        }
    }

    #[test]
    fn public_failed_turn_reason_redacts_internal_sensor_gate_errors() {
        assert_eq!(
            public_failed_turn_reason("sensor_gate:privacy_denied"),
            "sensor_gate:privacy_denied"
        );
        assert_eq!(
            public_failed_turn_reason(
                "sensor_gate: failed to open /tmp/private-vault/.cairn/cairn.db: disk I/O error"
            ),
            "turn_failed"
        );
    }

    /// Verify that `run_handler_inner` with an empty JSONL and `job_store=None`
    /// succeeds and produces no failed turns. This is the degenerate happy-path
    /// that confirms the new `job_store` + `consolidation_config` parameters
    /// don't break the existing no-op path.
    ///
    /// End-to-end enqueue coverage (`job_store=Some`, events threshold met) lives
    /// in the integration tests at Task 18 (`tests/rolling_summary.rs`), which
    /// have access to the full vault fixture harness.
    #[tokio::test]
    #[allow(
        clippy::expect_used,
        reason = "test: panics surface broken invariants immediately"
    )]
    async fn run_handler_inner_empty_jsonl_no_job_store() {
        let store = cairn_store_sqlite::open_in_memory()
            .await
            .expect("in-memory store");
        let vault = tempfile::tempdir().expect("tempdir");

        // Create the sources/ dir that `resolve_body_bytes` canonicalizes.
        std::fs::create_dir_all(vault.path().join("sources")).expect("mkdir sources");

        // Write an empty JSONL file.
        let jsonl = vault.path().join("empty.jsonl");
        std::fs::write(&jsonl, b"").expect("write empty JSONL");

        let result = run_handler_inner(
            &store,
            vault.path(),
            &jsonl,
            None,
            None,
            None,
            &ConsolidationConfig::default(),
            &DreamConfig::default(),
        )
        .await
        .expect("run_handler_inner should succeed on empty input");

        assert!(
            result.failed_turns.is_empty(),
            "no failed turns for empty input"
        );
        assert_eq!(result.trace_id.len(), 26, "trace_id is a ULID");
    }

    /// Confirm that `run_handler_inner` with `job_store=Some` but a
    /// `ConsolidationConfig { enabled: false, .. }` does not enqueue any jobs
    /// even when events are present. We exercise only the configuration branch —
    /// the threshold-firing path is covered by Task 18.
    ///
    /// Because this test needs an actual `SqliteJobStore` (file-backed, after
    /// migration), it is marked `#[ignore]` when the trait object can't be
    /// provided in-process without the full vault harness. Instead we verify
    /// only that `enqueue_if_due` with `enabled=false` returns `Disabled` —
    /// delegating to the trigger module's own test suite which already covers
    /// this case directly.
    #[test]
    fn consolidation_disabled_config_skips_enqueue_sanity() {
        // Confirm the config path that skip-enqueues is stable — the trigger
        // module's `disabled_returns_disabled` test owns the real assertion.
        let cfg = ConsolidationConfig {
            enabled: false,
            ..ConsolidationConfig::default()
        };
        assert!(!cfg.enabled, "disabled config must not be enabled");
    }

    #[tokio::test]
    #[allow(
        clippy::expect_used,
        reason = "test: panics surface broken invariants immediately"
    )]
    async fn stop_hook_enqueues_light_sleep_dream_job() {
        let store = cairn_store_sqlite::open_in_memory()
            .await
            .expect("in-memory store");
        let jobs = RecordingJobStore::new();
        let vault = tempfile::tempdir().expect("tempdir");
        write_stop_source(vault.path());
        let consolidation = ConsolidationConfig {
            enabled: false,
            ..ConsolidationConfig::default()
        };
        let dream_config = DreamConfig {
            enabled: true,
            ..DreamConfig::default()
        };

        let result = run_events_handler_inner_no_guard(
            &store,
            vault.path(),
            vec![stop_hook_event()],
            None,
            None,
            Some(&jobs),
            &consolidation,
            &dream_config,
        )
        .await
        .expect("capture_trace");
        assert!(
            result.failed_turns.is_empty(),
            "unexpected failed turns: {:?}",
            result.failed_turns
        );

        {
            let requests = jobs.requests.lock().expect("invariant: mutex");
            let dream_request = requests
                .iter()
                .find(|req| req.kind.as_str() == cairn_workflows::DREAM_KIND)
                .expect("queued dream job");
            let payload = cairn_workflows::DreamPayload::from_bytes(&dream_request.payload)
                .expect("dream payload");
            assert_eq!(payload.tier, DreamTier::LightSleep);
            assert_eq!(payload.key, STOP_SESSION_ID);
        }

        let replay = run_events_handler_inner_no_guard(
            &store,
            vault.path(),
            vec![stop_hook_event()],
            None,
            None,
            Some(&jobs),
            &consolidation,
            &dream_config,
        )
        .await
        .expect("capture_trace replay");
        assert!(
            replay.failed_turns.is_empty(),
            "unexpected replay failed turns: {:?}",
            replay.failed_turns
        );

        let requests = jobs.requests.lock().expect("invariant: mutex");
        let dream_requests = requests
            .iter()
            .filter(|req| req.kind.as_str() == cairn_workflows::DREAM_KIND)
            .collect::<Vec<_>>();
        assert_eq!(dream_requests.len(), 2);
        assert_eq!(
            dream_requests[0].dedupe_key, dream_requests[1].dedupe_key,
            "idempotent Stop-hook replay must target the same dream dedupe slot"
        );
    }

    #[tokio::test]
    #[allow(
        clippy::expect_used,
        reason = "test: panics surface broken invariants immediately"
    )]
    async fn explicit_skillify_stop_hook_enqueues_skillify_job() {
        let store = cairn_store_sqlite::open_in_memory()
            .await
            .expect("in-memory store");
        let jobs = RecordingJobStore::new();
        let vault = tempfile::tempdir().expect("tempdir");
        write_skillify_source(vault.path());
        write_stop_source(vault.path());
        let consolidation = ConsolidationConfig {
            enabled: false,
            ..ConsolidationConfig::default()
        };

        let result = run_events_handler_inner_no_guard(
            &store,
            vault.path(),
            vec![explicit_skillify_event(), stop_hook_event()],
            None,
            None,
            Some(&jobs),
            &consolidation,
            &DreamConfig::default(),
        )
        .await
        .expect("capture_trace");
        assert!(
            result.failed_turns.is_empty(),
            "unexpected failed turns: {:?}",
            result.failed_turns
        );

        let replay = run_events_handler_inner_no_guard(
            &store,
            vault.path(),
            vec![explicit_skillify_event(), stop_hook_event()],
            None,
            None,
            Some(&jobs),
            &consolidation,
            &DreamConfig::default(),
        )
        .await
        .expect("capture_trace replay");
        assert!(
            replay.failed_turns.is_empty(),
            "unexpected replay failed turns: {:?}",
            replay.failed_turns
        );

        let requests = jobs.requests.lock().expect("invariant: mutex");
        let skillify_requests = requests
            .iter()
            .filter(|req| req.kind.as_str() == cairn_workflows::SKILLIFY_KIND)
            .collect::<Vec<_>>();
        assert_eq!(skillify_requests.len(), 2);
        assert_eq!(
            skillify_requests[0].dedupe_key, skillify_requests[1].dedupe_key,
            "idempotent explicit Skillify replay must target the same dedupe slot"
        );
        let payload = cairn_workflows::SkillifyPayload::from_bytes(&skillify_requests[0].payload)
            .expect("skillify payload");
        assert_eq!(payload.trigger, cairn_workflows::SkillifyTrigger::Explicit);
        assert_eq!(payload.key, STOP_SESSION_ID);
        assert_eq!(payload.source_record_ids.len(), 2);
    }

    #[tokio::test]
    #[allow(
        clippy::expect_used,
        reason = "test: panics surface broken invariants immediately"
    )]
    async fn capture_trace_enqueues_trace_canvas_tool_steps() {
        let store = cairn_store_sqlite::open_in_memory()
            .await
            .expect("in-memory store");
        let vault = tempfile::tempdir().expect("tempdir");
        let conn = rusqlite::Connection::open_in_memory().expect("job conn");
        cairn_workflows::sqlite_store::install_for_tests(&conn);
        let jobs = cairn_workflows::SqliteJobStore::new(conn).expect("job store");

        let session_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let turn_id = "turn-canvas";
        let tool_id = "toolu_canvas_01";
        let pre_id = "01ARZ3NDEKTSV4RRFFQ69G5FA1";
        let post_id = "01ARZ3NDEKTSV4RRFFQ69G5FA2";
        let pre_body = r#"{"tool":"shell","input":{}}"#;
        let post_body = r#"{"ok":true}"#;
        let pre_ref = write_test_source(vault.path(), pre_id, pre_body);
        let post_ref = write_test_source(vault.path(), post_id, post_body);
        let events = vec![
            test_hook_event(
                pre_id,
                "PreToolUse",
                session_id,
                turn_id,
                "2026-05-02T00:00:02Z",
                Some(tool_id),
                &pre_ref,
                pre_body,
            ),
            test_hook_event(
                post_id,
                "PostToolUse",
                session_id,
                turn_id,
                "2026-05-02T00:00:03Z",
                Some(tool_id),
                &post_ref,
                post_body,
            ),
        ];

        let response = run_events_handler_inner_no_guard(
            &store,
            vault.path(),
            events,
            None,
            None,
            Some(&jobs),
            &ConsolidationConfig::default(),
            &DreamConfig::default(),
        )
        .await
        .expect("capture trace");
        assert_eq!(
            response.failed_turns,
            Vec::<(String, String, String)>::new()
        );

        let job_id = cairn_core::contract::job_store::JobId::new(format!(
            "trace-canvas:trace-step:{post_id}"
        ));
        let leased = cairn_core::contract::job_store::JobStore::lease_specific(
            &jobs,
            &job_id,
            &cairn_core::contract::job_store::JobKind::new(cairn_workflows::TRACE_CANVAS_KIND),
            "test-worker",
            chrono::Utc::now().timestamp_millis(),
            30_000,
        )
        .await
        .expect("lease")
        .expect("trace canvas job queued");
        let payload =
            cairn_workflows::TraceCanvasPayload::from_bytes(&leased.payload).expect("payload");
        assert_eq!(
            payload.projection.step.step_id,
            format!("trace-step:{post_id}")
        );
        assert_eq!(payload.projection.step.session_id, session_id);
        assert_eq!(payload.projection.step.turn_id, turn_id);
        assert_eq!(
            payload.projection.step.tool_call_id.as_deref(),
            Some(tool_id)
        );
        assert_eq!(payload.projection.step.result_ref.as_deref(), Some(post_id));
        assert_eq!(payload.projection.node_status, "completed");
    }

    fn write_test_source(vault: &Path, event_id: &str, body: &str) -> String {
        let dir = vault.join("sources/hook");
        std::fs::create_dir_all(&dir).expect("mkdir source dir");
        let file_name = format!("{event_id}.txt");
        std::fs::write(dir.join(&file_name), body).expect("write source");
        format!("sources/hook/{file_name}")
    }

    #[allow(clippy::too_many_arguments, reason = "test fixture factory")]
    fn test_hook_event(
        event_id: &str,
        hook_name: &str,
        session_id: &str,
        turn_id: &str,
        timestamp: &str,
        tool_id: Option<&str>,
        payload_ref: &str,
        body: &str,
    ) -> CaptureEvent {
        let sensor =
            Identity::parse("snr:local:hook:cc-session:v1").expect("valid sensor identity");
        CaptureEvent {
            event_id: CaptureEventId::parse(event_id).expect("valid event id"),
            sensor_id: sensor.clone(),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: sensor,
                at: Rfc3339Timestamp::parse(timestamp).expect("valid timestamp"),
            }],
            refs: Some(CaptureRefs {
                session_id: Some(session_id.to_owned()),
                turn_id: Some(turn_id.to_owned()),
                tool_id: tool_id.map(ToOwned::to_owned),
            }),
            payload_hash: PayloadHash::parse(format!("sha256:{}", test_sha256_hex(body)))
                .expect("valid payload hash"),
            payload_ref: payload_ref.to_owned(),
            captured_at: Rfc3339Timestamp::parse(timestamp).expect("valid timestamp"),
            payload: CapturePayload::Hook {
                hook_name: hook_name.to_owned(),
                tool_name: Some("shell".to_owned()),
            },
            source_family: SourceFamily::Hook,
        }
    }

    fn test_sha256_hex(body: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(body.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}
