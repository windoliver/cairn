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
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_core::domain::capture::CaptureEvent;
use cairn_core::domain::trace::{TraceEvent, TraceLink};
use cairn_core::domain::{
    CaptureEventId, SessionId, ZeroCaptureAuditInput, ZeroCaptureReport, ZeroCaptureTrigger,
    decide_zero_capture_nudge,
};
use cairn_core::generated::common::Ulid as GeneratedUlid;
use cairn_core::generated::envelope::{
    Response, ResponseData, ResponsePolicyTrace, ResponseStatus, ResponseVerb,
};
use cairn_core::generated::verbs::capture_trace::{CaptureTraceData, CaptureTraceDataFailedTurns};
use cairn_core::pipeline::capture_trace::{classify, project};
use cairn_core::pipeline::dispatch::{BypassReason, DefaultRegistry, DispatchDecision, dispatch};
use cairn_core::pipeline::extract::body::ResolvedBody;
use cairn_core::pipeline::turn::summarize_turn;
use cairn_store_sqlite::SqliteMemoryStore;
use clap::ArgMatches;
use sha2::{Digest as _, Sha256};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt as _, BufReader};
use ulid::Ulid;

use crate::identity::{guard::refuse_if_degraded, status::ReconciliationReport};

use super::envelope::{emit_json, human_error, internal_error_response, new_operation_id};

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
            // Classify the event. P0 only recognizes the four hook payloads
            // (UserPromptSubmit / PreToolUse / PostToolUse / Stop). Other
            // shapes (`AgentMessage`, `ToolOutput`) await sensor adapters
            // (#84). Fail the whole turn on the first unclassifiable event
            // rather than persisting a partial set: the summary record
            // would otherwise be built from incomplete data and become
            // hard-to-detect data loss.
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

            // Capture → Extract dispatch (issue #217). Every event reaching
            // this driver passes through the routing decision before we
            // resolve the body bytes from sources/. P0 trace replay only
            // accepts hook payloads (classify() rejects everything else),
            // so the decision is always `Bypass(NonTerminalFamily)` —
            // raw bytes flow to Extract unchanged. Calling dispatch here
            // anyway makes the routing path live in production: a future
            // PR that admits a Terminal payload here cannot bypass the
            // squash gate, and a malformed envelope is rejected with
            // `MalformedEnvelope` before we open `sources/`.
            match dispatch(event, &DefaultRegistry) {
                DispatchDecision::Bypass(BypassReason::NonTerminalFamily) => {}
                DispatchDecision::Bypass(BypassReason::MalformedEnvelope) => {
                    failed_turns.push((
                        session_str.clone(),
                        turn_str.clone(),
                        format!("dispatch {}: malformed envelope", event.event_id),
                    ));
                    group_failed = true;
                    break;
                }
                other => {
                    // classify() already rejected non-hook payloads; reaching
                    // any other dispatch decision means the classifier and
                    // dispatch driver disagree about routing. Surface as a
                    // turn-level failure rather than silently proceeding.
                    failed_turns.push((
                        session_str.clone(),
                        turn_str.clone(),
                        format!(
                            "dispatch {}: unexpected decision {other:?} for hook payload",
                            event.event_id
                        ),
                    ));
                    group_failed = true;
                    break;
                }
            }

            // Resolve body from sources/, then run the standard pre-persist
            // PII / secret redactor (CLAUDE.md §4.9 "privacy by construction":
            // raw record bodies must be redacted before they reach storage).
            // Trace records flow into both per-event rows and turn summaries
            // built from those rows, so redacting once at the boundary covers
            // both surfaces.
            let raw_text = match resolve_body_text(vault_root, event).await {
                Ok(t) => t,
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
            let text = cairn_core::pipeline::filter::redact(&raw_text).text;

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

            let record = match project(event, classified, &resolved, &link) {
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
                tx.count_trace_events_for_session(&session_id_tx)
            })
            .await;

        let successful_capture_trace_writes = match result {
            Ok(count) => count,
            Err(e) => {
                failed_turns.push((session_str, turn_str, e.to_string()));
                continue;
            }
        };

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
                continue;
            }
        }
    }

    Ok(CaptureTraceResponse {
        trace_id: Ulid::new().to_string(),
        failed_turns,
    })
}

/// Read the payload bytes referenced by `event.payload_ref` from disk
/// (relative to `vault_root`), verify the SHA-256 hash matches
/// `event.payload_hash`, and return the body as a UTF-8 `String`.
///
/// The `payload_hash` field may be prefixed with `"sha256:"` — this prefix
/// is stripped before comparison.
///
/// # Errors
///
/// - I/O error reading the file.
/// - SHA-256 mismatch between the stored hash and the computed hash.
/// - Bytes not valid UTF-8.
async fn resolve_body_text(vault_root: &Path, event: &CaptureEvent) -> anyhow::Result<String> {
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

    String::from_utf8(bytes)
        .with_context(|| format!("payload at {} is not valid UTF-8", path.display()))
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

/// Run `cairn capture_trace`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: &Path) -> ExitCode {
    let json = sub.get_flag("json");
    let from = sub
        .get_one::<std::path::PathBuf>("from")
        .cloned()
        .expect("invariant: clap requires --from");

    let db_path = vault_root.join(".cairn").join("cairn.db");
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return emit_capture_trace_internal(json, &format!("build tokio runtime: {e}")),
    };

    let result = rt.block_on(async {
        let store = cairn_store_sqlite::open(&db_path).await?;
        run_handler(&store, vault_root, &from).await
    });

    match result {
        Ok(response) => {
            if json {
                emit_json(&committed_response(&response));
            } else {
                println!(
                    "cairn capture_trace: trace_id={} failed_turns={}",
                    response.trace_id,
                    response.failed_turns.len()
                );
                for (session, turn, reason) in &response.failed_turns {
                    println!("  - session={} turn={} reason={}", session, turn, reason);
                }
            }
            ExitCode::SUCCESS
        }
        Err(e) => emit_capture_trace_internal(json, &format!("{e:#}")),
    }
}

fn committed_response(response: &CaptureTraceResponse) -> Response {
    Response {
        contract: "cairn.mcp.v1".to_owned(),
        data: Some(ResponseData::CaptureTrace(CaptureTraceData {
            failed_turns: response
                .failed_turns
                .iter()
                .map(
                    |(session_id, turn_id, reason)| CaptureTraceDataFailedTurns {
                        reason: reason.clone(),
                        session_id: session_id.clone(),
                        turn_id: turn_id.clone(),
                    },
                )
                .collect(),
            trace_id: GeneratedUlid(response.trace_id.clone()),
        })),
        error: None,
        operation_id: new_operation_id(),
        policy_trace: Vec::<ResponsePolicyTrace>::new(),
        status: ResponseStatus::Committed,
        target: None,
        verb: ResponseVerb::CaptureTrace,
    }
}

fn emit_capture_trace_internal(json: bool, message: &str) -> ExitCode {
    let response = internal_error_response(ResponseVerb::CaptureTrace, message);
    if json {
        emit_json(&response);
    } else {
        human_error("capture_trace", "Internal", message, &response.operation_id);
    }
    ExitCode::FAILURE
}
