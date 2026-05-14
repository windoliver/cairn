//! Turn-level pure helpers for trace persistence (issue #77, spec §4 Ordering).
//!
//! `cairn-core` is store-agnostic. These helpers operate on lightweight
//! references that the CLI verb (or tests) supplies; the actual `SQLite`
//! reads happen elsewhere.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde_json::{Map as JsonMap, Value as Json};

use crate::domain::capture::CaptureEvent;
use crate::domain::record::{Ed25519Signature, MemoryRecord};
use crate::domain::taxonomy::{MemoryClass, MemoryKind, MemoryVisibility};
use crate::domain::trace::{TraceEvent, summary_record_id};
use crate::domain::{EvidenceVector, Provenance, ScopeTuple, SessionId, TargetId};
use crate::pipeline::capture_trace::TRACE_BODY_CAP;

/// One entry in a turn's ordered event sequence.
#[derive(Debug, Clone, Copy)]
pub struct OrderedEntry<'a> {
    /// The originating capture envelope.
    pub event: &'a CaptureEvent,
    /// The trace-event classification produced by `classify`.
    pub classified: TraceEvent,
}

/// Sort the union of `persisted` and `incoming` events by
/// `(captured_at, capture_event_id)`. Stable across replays — identical
/// inputs always produce the same ordering.
///
/// `persisted` represents events that already live in the store for this
/// turn (decoded back into their original `CaptureEvent`s by the caller).
/// `incoming` represents the new events being imported.
///
/// Chronological ordering uses [`crate::domain::Rfc3339Timestamp::cmp_chronological`]
/// which accounts for timezone offsets, rather than lexical string comparison.
/// Ties in timestamp are broken by `capture_event_id` ascending (lexical on
/// the ULID string, which also sorts chronologically for ULIDs generated in
/// the same millisecond).
#[must_use]
pub fn order_by_captured_at<'a>(
    persisted: &'a [OrderedEntry<'a>],
    incoming: &'a [OrderedEntry<'a>],
) -> Vec<OrderedEntry<'a>> {
    let mut all: Vec<OrderedEntry<'a>> = Vec::with_capacity(persisted.len() + incoming.len());
    all.extend_from_slice(persisted);
    all.extend_from_slice(incoming);
    all.sort_by(|x, y| {
        x.event
            .captured_at
            .cmp_chronological(&y.event.captured_at)
            .then_with(|| x.event.event_id.as_str().cmp(y.event.event_id.as_str()))
    });
    all
}

/// Assign sequences `0..N` over an already-ordered slice. Returned
/// `(sequence, entry)` pairs preserve the input order.
#[must_use]
pub fn assign_sequences<'a>(ordered: &'a [OrderedEntry<'a>]) -> Vec<(u64, OrderedEntry<'a>)> {
    ordered
        .iter()
        .enumerate()
        // `i` is a `usize`; on all supported targets `usize` fits in `u64`
        // (both 32-bit and 64-bit platforms), so the fallback is unreachable
        // in practice. `unwrap_or` avoids a bare `unwrap`.
        .map(|(i, entry)| (u64::try_from(i).unwrap_or(u64::MAX), *entry))
        .collect()
}

/// Failure modes for [`summarize_turn`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum TurnSummaryError {
    /// At least one event is required to summarize.
    #[error("turn must contain at least one event")]
    EmptyTurn,
    /// Events span more than one `(session_id, turn_id)`.
    #[error("events span more than one (session_id, turn_id)")]
    CrossTurn,
    /// Sequence numbers must be strictly increasing.
    #[error("sequences must be strictly increasing")]
    OutOfOrderSequences,
    /// A record lacked the required `extra_frontmatter.trace.*` field.
    #[error("record is missing extra_frontmatter.trace.{0}")]
    MissingTrace(&'static str),
}

/// Build a deterministic `turn_summary` [`MemoryRecord`] from an ordered
/// slice of trace records belonging to one `(session_id, turn_id)`.
///
/// Pure: no LLM, no I/O. Same input always produces the same output.
///
/// The returned record carries a zeroed placeholder signature
/// (`ed25519:000…`); a later signing stage must finalize it before the
/// store accepts it. This mirrors the pattern used by
/// [`crate::pipeline::capture_trace::project`].
///
/// # Errors
///
/// See [`TurnSummaryError`].
pub fn summarize_turn(
    session_id: &SessionId,
    turn_id: &str,
    events: &[MemoryRecord],
) -> Result<MemoryRecord, TurnSummaryError> {
    if events.is_empty() {
        return Err(TurnSummaryError::EmptyTurn);
    }
    let member_ids = validate_and_collect_members(session_id, turn_id, events)?;
    let body = build_summary_body(session_id, turn_id, events)?;
    let id = summary_record_id(session_id, turn_id);
    let extra = build_summary_frontmatter(session_id, turn_id, &id, member_ids);
    let first = &events[0];
    // Safety: non-empty checked above; last index cannot fail.
    let last = &events[events.len() - 1];
    let provenance = Provenance {
        source_sensor: first.provenance.source_sensor.clone(),
        created_at: first.provenance.created_at.clone(),
        originating_agent_id: first.provenance.originating_agent_id.clone(),
        source_ids: first.provenance.source_ids.clone(),
        // Summary has no raw source bytes — zeroed SHA256 sentinel mirrors
        // the all-zeros pattern used for the placeholder signature below.
        source_hash: format!("sha256:{}", "0".repeat(64)),
        consent_ref: "consent:pending".to_owned(),
        llm_id_if_any: None,
        source_refs: Vec::new(),
    };
    let scope = ScopeTuple {
        session_id: Some(session_id.as_str().to_owned()),
        ..ScopeTuple::default()
    };
    let target_id = TargetId::parse(id.as_str())
        .unwrap_or_else(|_| unreachable!("summary_record_id always yields a valid ULID"));
    // Placeholder signature: all-zeros hex, matching
    // `capture_trace::placeholder_signature`. A later signing stage
    // replaces this before the store accepts the record.
    let signature = Ed25519Signature::parse(format!("ed25519:{}", "0".repeat(128)))
        .unwrap_or_else(|_| unreachable!("128 '0' chars always satisfies Ed25519Signature::parse"));
    Ok(MemoryRecord {
        id,
        target_id,
        kind: MemoryKind::Trace,
        class: MemoryClass::Episodic,
        visibility: MemoryVisibility::Private,
        scope,
        body,
        source_ids: first.source_ids.clone(),
        provenance,
        updated_at: last.updated_at.clone(),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 1.0,
        actor_chain: first.actor_chain.clone(),
        signature,
        tags: Vec::new(),
        extra_frontmatter: extra,
        consent_model: None,
    })
}

/// Validate that all events belong to `(session_id, turn_id)` with
/// strictly increasing sequences, and collect the `capture_event_id` of
/// each in order.
fn validate_and_collect_members(
    session_id: &SessionId,
    turn_id: &str,
    events: &[MemoryRecord],
) -> Result<Vec<String>, TurnSummaryError> {
    let mut last_seq: Option<u64> = None;
    let mut member_ids: Vec<String> = Vec::with_capacity(events.len());
    for r in events {
        let trace = r
            .extra_frontmatter
            .get("trace")
            .and_then(Json::as_object)
            .ok_or(TurnSummaryError::MissingTrace("(root)"))?;
        let s = trace
            .get("session_id")
            .and_then(Json::as_str)
            .ok_or(TurnSummaryError::MissingTrace("session_id"))?;
        let t = trace
            .get("turn_id")
            .and_then(Json::as_str)
            .ok_or(TurnSummaryError::MissingTrace("turn_id"))?;
        if s != session_id.as_str() || t != turn_id {
            return Err(TurnSummaryError::CrossTurn);
        }
        let seq = trace
            .get("sequence")
            .and_then(Json::as_u64)
            .ok_or(TurnSummaryError::MissingTrace("sequence"))?;
        if last_seq.is_some_and(|prev| seq <= prev) {
            return Err(TurnSummaryError::OutOfOrderSequences);
        }
        last_seq = Some(seq);
        let cap_id = trace
            .get("capture_event_id")
            .and_then(Json::as_str)
            .ok_or(TurnSummaryError::MissingTrace("capture_event_id"))?;
        member_ids.push(cap_id.to_owned());
    }
    Ok(member_ids)
}

/// Build the deterministic body: header + one line per event, capped at
/// [`TRACE_BODY_CAP`]. Same truncation logic as `capture_trace::truncate`.
fn build_summary_body(
    session_id: &SessionId,
    turn_id: &str,
    events: &[MemoryRecord],
) -> Result<String, TurnSummaryError> {
    let mut body = format!("## Turn {} (session {})\n", turn_id, session_id.as_str());
    for r in events {
        let trace = r
            .extra_frontmatter
            .get("trace")
            .and_then(Json::as_object)
            .ok_or(TurnSummaryError::MissingTrace("(root)"))?;
        let seq = trace.get("sequence").and_then(Json::as_u64).unwrap_or(0);
        let evt = r
            .extra_frontmatter
            .get("trace_event")
            .and_then(Json::as_str)
            .unwrap_or("");
        let excerpt = summary_excerpt(r);
        // writeln! on String is infallible (String's fmt::Write never errors).
        let _ = writeln!(body, "- [seq {seq}] {evt}: {excerpt}");
        if body.len() >= TRACE_BODY_CAP {
            let mut end = TRACE_BODY_CAP;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            body.truncate(end);
            break;
        }
    }
    Ok(body)
}

fn summary_excerpt(record: &MemoryRecord) -> String {
    if !record.body.is_empty() {
        return record.body.chars().take(80).collect();
    }

    record
        .extra_frontmatter
        .get("trace_blocks")
        .and_then(trace_blocks_excerpt)
        .unwrap_or_default()
}

fn trace_blocks_excerpt(value: &Json) -> Option<String> {
    let blocks = value.as_array()?;
    let mut out = String::new();
    for block in blocks {
        let obj = block.as_object()?;
        // Skip reasoning blocks — they must not leak into searchable summary
        // bodies. Reasoning visibility is controlled by the search filter
        // against `extra_frontmatter.trace_blocks`; the summary excerpt is
        // already past that gate.
        let kind = obj.get("kind").and_then(Json::as_str).unwrap_or_default();
        if kind.eq_ignore_ascii_case("reasoning") {
            continue;
        }
        let Some(text) = obj
            .get("text")
            .and_then(Json::as_str)
            .or_else(|| obj.get("content").and_then(Json::as_str))
        else {
            continue;
        };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(text);
        if out.len() >= 80 {
            break;
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out.chars().take(80).collect())
}

/// Build the `extra_frontmatter` map for a turn-summary record.
fn build_summary_frontmatter(
    session_id: &SessionId,
    turn_id: &str,
    id: &crate::domain::record::RecordId,
    member_ids: Vec<String>,
) -> BTreeMap<String, Json> {
    let mut trace_obj = JsonMap::new();
    trace_obj.insert(
        "session_id".into(),
        Json::String(session_id.as_str().to_owned()),
    );
    trace_obj.insert("turn_id".into(), Json::String(turn_id.to_owned()));
    // sequence = 0: summaries are out-of-band from the monotonic event
    // sequence. The unique-seq SQLite index excludes summary records, so
    // 0 is the designated sentinel and easy to filter on.
    trace_obj.insert("sequence".into(), Json::Number(0_u64.into()));
    // capture_event_id for the summary equals its own deterministic record
    // id — synthetic, no underlying CaptureEvent.
    trace_obj.insert(
        "capture_event_id".into(),
        Json::String(id.as_str().to_owned()),
    );
    trace_obj.insert(
        "member_event_ids".into(),
        Json::Array(member_ids.into_iter().map(Json::String).collect()),
    );
    let mut extra: BTreeMap<String, Json> = BTreeMap::new();
    extra.insert(
        "trace_event".into(),
        Json::String("turn_summary".to_owned()),
    );
    extra.insert("trace".into(), Json::Object(trace_obj));
    extra
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ActorChainEntry, ChainRole, Identity, Rfc3339Timestamp,
        capture::{
            CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, PayloadHash,
            SourceFamily,
        },
    };

    /// Build a minimal hook `CaptureEvent` with the given `event_id` and
    /// `captured_at`. Mirrors `pipeline::capture_trace::tests::mk_hook_event`
    /// but parameterised on both fields so ordering tests can vary them
    /// independently.
    fn mk_at(event_id: &str, captured_at: &str) -> CaptureEvent {
        let ts = Rfc3339Timestamp::parse(captured_at)
            .expect("invariant: test timestamp must be valid RFC3339");
        CaptureEvent {
            event_id: CaptureEventId::parse(event_id)
                .expect("invariant: test event_id must be a valid ULID"),
            sensor_id: Identity::parse("snr:local:hook:cc-session:v1")
                .expect("invariant: fixed sensor identity is valid"),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("snr:local:hook:cc-session:v1")
                    .expect("invariant: fixed sensor identity is valid"),
                at: ts.clone(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some("sess".into()),
                turn_id: Some("turn".into()),
                tool_id: None,
            }),
            payload_hash: PayloadHash::parse(
                "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            )
            .expect("invariant: well-formed sha256 hash"),
            payload_ref: format!("sources/hook/{event_id}.txt"),
            captured_at: ts,
            payload: CapturePayload::Hook {
                hook_name: "UserPromptSubmit".into(),
                tool_name: None,
            },
            source_family: SourceFamily::Hook,
        }
    }

    #[test]
    fn orders_by_captured_at_then_event_id() {
        let a = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAA", "2026-05-02T00:00:01Z");
        let b = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAB", "2026-05-02T00:00:00Z");
        let c = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAC", "2026-05-02T00:00:00Z");
        let entries: Vec<OrderedEntry<'_>> = [&a, &b, &c]
            .into_iter()
            .map(|e| OrderedEntry {
                event: e,
                classified: TraceEvent::UserMessage,
            })
            .collect();
        let ordered = order_by_captured_at(&[], &entries);
        // Earlier timestamp first; ties broken by capture_event_id ascending.
        assert_eq!(
            ordered[0].event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAB"
        );
        assert_eq!(
            ordered[1].event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAC"
        );
        assert_eq!(
            ordered[2].event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAA"
        );
    }

    #[test]
    fn assigns_sequences_zero_indexed() {
        let a = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAA", "2026-05-02T00:00:00Z");
        let b = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAB", "2026-05-02T00:00:01Z");
        let entries: Vec<OrderedEntry<'_>> = [&a, &b]
            .into_iter()
            .map(|e| OrderedEntry {
                event: e,
                classified: TraceEvent::UserMessage,
            })
            .collect();
        let ordered = order_by_captured_at(&[], &entries);
        let with_seq = assign_sequences(&ordered);
        assert_eq!(with_seq[0].0, 0);
        assert_eq!(with_seq[1].0, 1);
        assert_eq!(
            with_seq[0].1.event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAA"
        );
    }

    #[test]
    fn merges_persisted_and_incoming() {
        let p = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAA", "2026-05-02T00:00:01Z");
        let i = mk_at("01ARZ3NDEKTSV4RRFFQ69G5FAB", "2026-05-02T00:00:00Z");
        let persisted = [OrderedEntry {
            event: &p,
            classified: TraceEvent::UserMessage,
        }];
        let incoming = [OrderedEntry {
            event: &i,
            classified: TraceEvent::PreTool,
        }];
        let ordered = order_by_captured_at(&persisted, &incoming);
        // Incoming has earlier timestamp; should sort first.
        assert_eq!(
            ordered[0].event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAB"
        );
        assert_eq!(
            ordered[1].event.event_id.as_str(),
            "01ARZ3NDEKTSV4RRFFQ69G5FAA"
        );
    }

    // ── summarize_turn tests ──────────────────────────────────────────────

    /// Build a minimal trace `MemoryRecord` by calling
    /// `capture_trace::project` with synthesized inputs. Uses
    /// `UserMessage` for all events; adjust `hook_name` / `classified`
    /// if variant-specific tests are needed.
    #[allow(
        clippy::expect_used,
        reason = "test helper: bad inputs mean the test data itself is broken"
    )]
    fn mk_trace_record(
        session_id_str: &str,
        turn_id: &str,
        sequence: u64,
        capture_event_id_str: &str,
        trace_event: &str,
        body_text: &str,
    ) -> MemoryRecord {
        use crate::domain::trace::{TraceEvent as TE, TraceLink};
        use crate::pipeline::capture_trace::project;
        use crate::pipeline::extract::body;

        // Map trace_event string to a hook name and classified variant.
        let hook_name = match trace_event {
            "stop" => "Stop",
            "pre_tool" => "PreToolUse",
            "post_tool" => "PostToolUse",
            // user_message, agent_message, and unknown — use UserPromptSubmit
            _ => "UserPromptSubmit",
        };
        let classified = match trace_event {
            "stop" => TE::Stop,
            _ => TE::UserMessage,
        };

        let ts =
            Rfc3339Timestamp::parse("2026-05-02T00:00:00Z").expect("invariant: fixed timestamp");
        let event = CaptureEvent {
            event_id: CaptureEventId::parse(capture_event_id_str)
                .expect("invariant: test capture_event_id must be valid ULID"),
            sensor_id: Identity::parse("snr:local:hook:cc-session:v1")
                .expect("invariant: fixed sensor identity"),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("snr:local:hook:cc-session:v1")
                    .expect("invariant: fixed sensor identity"),
                at: ts.clone(),
            }],
            refs: Some(CaptureRefs {
                session_id: Some(session_id_str.into()),
                turn_id: Some(turn_id.into()),
                tool_id: None,
            }),
            payload_hash: PayloadHash::parse(
                "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            )
            .expect("invariant: well-formed sha256 hash"),
            payload_ref: format!("sources/hook/{capture_event_id_str}.txt"),
            captured_at: ts,
            payload: CapturePayload::Hook {
                hook_name: hook_name.into(),
                tool_name: None,
            },
            source_family: SourceFamily::Hook,
        };

        let sess = SessionId::parse(session_id_str).expect("invariant: valid session_id ULID");
        let cap_id = CaptureEventId::parse(capture_event_id_str)
            .expect("invariant: valid capture_event_id ULID");
        let link = TraceLink {
            session_id: sess,
            turn_id: turn_id.to_owned(),
            sequence,
            capture_event_id: cap_id,
            parent_event_id: None,
            tool_call_id: None,
            member_event_ids: Vec::new(),
        };

        let resolved = body::for_test(body_text);
        project(&event, classified, &resolved, &link)
            .expect("invariant: test inputs must produce a valid trace record")
    }

    #[test]
    fn summarize_orders_and_collects_member_ids() {
        let s = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
        let events = vec![
            mk_trace_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "turn-1",
                0,
                "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                "user_message",
                "hi",
            ),
            mk_trace_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "turn-1",
                1,
                "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                "agent_message",
                "hello",
            ),
        ];
        let summary = summarize_turn(&s, "turn-1", &events).unwrap();
        assert_eq!(summary.kind, MemoryKind::Trace);
        assert_eq!(
            summary.extra_frontmatter.get("trace_event").unwrap(),
            "turn_summary"
        );
        let trace = summary.extra_frontmatter.get("trace").unwrap();
        let members = trace["member_event_ids"].as_array().unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0], "01ARZ3NDEKTSV4RRFFQ69G5FAA");
        assert_eq!(members[1], "01ARZ3NDEKTSV4RRFFQ69G5FAB");
        assert_eq!(summary.id, summary_record_id(&s, "turn-1"));
    }

    #[test]
    fn summarize_blocks_excerpt_omits_reasoning_signatures() {
        let s = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
        let mut record = mk_trace_record(
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "turn-1",
            0,
            "01ARZ3NDEKTSV4RRFFQ69G5FAA",
            "agent_message",
            "",
        );
        record.extra_frontmatter.insert(
            "trace_blocks".into(),
            serde_json::json!([
                {
                    "kind": "reasoning",
                    "text": "compare the tool outputs carefully",
                    "signature": "sig-secret-keep-out"
                },
                {
                    "kind": "tool_result",
                    "tool_use_id": "toolu_1",
                    "content": "selected ripgrep",
                    "is_error": false
                }
            ]),
        );

        let summary = summarize_turn(&s, "turn-1", &[record]).expect("summary");
        assert!(
            !summary.body.contains("compare the tool outputs carefully"),
            "summary must not leak reasoning text: {}",
            summary.body
        );
        assert!(
            summary.body.contains("selected ripgrep"),
            "summary should include tool result text: {}",
            summary.body
        );
        assert!(
            !summary.body.contains("sig-secret-keep-out"),
            "summary must not leak reasoning signatures: {}",
            summary.body
        );
    }

    #[test]
    fn summarize_rejects_empty() {
        let s = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
        assert_eq!(
            summarize_turn(&s, "turn-1", &[]).unwrap_err(),
            TurnSummaryError::EmptyTurn
        );
    }

    #[test]
    fn summarize_rejects_cross_turn() {
        let s = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
        let events = vec![
            mk_trace_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "turn-1",
                0,
                "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                "user_message",
                "a",
            ),
            mk_trace_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "turn-2",
                0,
                "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                "user_message",
                "b",
            ),
        ];
        assert!(matches!(
            summarize_turn(&s, "turn-1", &events).unwrap_err(),
            TurnSummaryError::CrossTurn
        ));
    }

    #[test]
    fn summarize_rejects_out_of_order_sequence() {
        let s = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
        let events = vec![
            mk_trace_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "turn-1",
                1,
                "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                "user_message",
                "a",
            ),
            mk_trace_record(
                "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "turn-1",
                0,
                "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                "agent_message",
                "b",
            ),
        ];
        assert!(matches!(
            summarize_turn(&s, "turn-1", &events).unwrap_err(),
            TurnSummaryError::OutOfOrderSequences
        ));
    }
}
