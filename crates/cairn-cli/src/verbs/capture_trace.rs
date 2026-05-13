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
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_core::config::CairnConfig;
use cairn_core::domain::capture::{
    CaptureEvent, CaptureMode, CapturePayload, CaptureRefs, PayloadHash, SourceFamily,
};
use cairn_core::domain::trace::{TraceBlock, TraceEvent, TraceLink};
use cairn_core::domain::{
    ActorChainEntry, CaptureEventId, ChainRole, Identity, Rfc3339Timestamp, ScopeTuple, SessionId,
};
use cairn_core::generated::common::Ulid as WireUlid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::capture_trace::{CaptureTraceData, FailedTurn};
use cairn_core::pipeline::capture_trace::{
    ProjectedTraceBlocks, classify, project, project_pre_compact_snapshot, project_with_blocks,
};
use cairn_core::pipeline::dispatch::{DefaultRegistry, trace_body_bytes};
use cairn_core::pipeline::extract::body::ResolvedBody;
use cairn_core::pipeline::filter::{
    Decision, FilterInputs, RedactionTag, fence, redact, should_memorize,
};
use cairn_core::pipeline::turn::summarize_turn;
use cairn_core::policy_trace::{PolicyGate, PolicyTraceEntry, to_wire};
use cairn_store_sqlite::SqliteMemoryStore;
use clap::ArgMatches;
use sha2::{Digest as _, Sha256};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use ulid::Ulid;

use crate::identity::{guard::refuse_if_degraded, status::ReconciliationReport};

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
#[allow(
    clippy::too_many_lines,
    reason = "CLI verb dispatcher: guard → parse → group → per-turn persist. \
              Each step is linear; extracting sub-functions would hide the \
              sequential flow without reducing complexity."
)]
pub async fn run_handler(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    from: &Path,
) -> anyhow::Result<CaptureTraceResponse> {
    run_handler_inner(store, vault_root, from, None).await
}

/// Persist a JSONL batch while binding projected rows to a verified vault scope.
pub async fn run_handler_with_scope(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    from: &Path,
    scope_binding: ScopeTuple,
) -> anyhow::Result<CaptureTraceResponse> {
    run_handler_inner(store, vault_root, from, Some(&scope_binding)).await
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
    clippy::too_many_lines,
    reason = "trace import keeps validation, projection, and per-turn atomicity in one ordered transaction flow"
)]
async fn run_handler_inner(
    store: &SqliteMemoryStore,
    vault_root: &Path,
    from: &Path,
    scope_binding: Option<&ScopeTuple>,
) -> anyhow::Result<CaptureTraceResponse> {
    // §3.5 trust-boundary guard.
    refuse_if_degraded(&ReconciliationReport::default(), vec![])
        .context("capture_trace: vault degraded")?;

    let events = read_jsonl_events(from).await?;

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
        let mut had_stop = false;
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
            if !matches!(classify(event), Ok(TraceEvent::PreTool)) {
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
            let classified = match classify(event) {
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
                // exists (closed-turn re-summarize per spec §4).
                if had_stop || tx.turn_summary_exists(&session_id_tx, &turn_str_tx)? {
                    let final_rows = tx.list_trace_events(&session_id_tx, &turn_str_tx)?;
                    let summary = summarize_turn(&session_id_tx, &turn_str_tx, &final_rows)
                        .map_err(|e| cairn_store_sqlite::error::StoreError::Invariant {
                            what: format!("summarize_turn: {e}"),
                        })?;
                    tx.upsert_trace(&summary)?;
                }
                Ok::<(), cairn_store_sqlite::error::StoreError>(())
            })
            .await;

        if let Err(e) = result {
            failed_turns.push((session_str, turn_str, e.to_string()));
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

        let classified = classify(&event).map_err(anyhow::Error::msg)?;
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
    if matches!(&event.payload, CapturePayload::Voice { .. }) {
        return voice_transcript_text(body_bytes);
    }
    String::from_utf8(body_bytes.to_vec()).context("routed body is not valid UTF-8")
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

/// Compute the lowercase hex SHA-256 digest of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
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
    let result = match input {
        CaptureTraceInput::Jsonl(from) => {
            run_handler_with_scope(&ctx.store, &ctx.vault_root, &from, scope_binding).await
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
