//! Trace-event projector (issue #77, brief §5.0).
//!
//! Pure helpers that turn a `CaptureEvent` plus a hash-verified
//! `ResolvedBody` into a `MemoryRecord` (Task 6) for any of the seven
//! [`TraceEvent`] variants. This module owns body shaping and the cap that
//! keeps trace records bounded; full bytes stay in `sources/` referenced
//! by `payload_hash`.

use std::collections::BTreeMap;

use serde_json::{Map as JsonMap, Value as Json};

use crate::domain::{
    ChainRole, DomainError, EvidenceVector, Provenance, ScopeTuple, SourceId, TargetId,
    capture::{CaptureEvent, CapturePayload},
    record::{Ed25519Signature, MemoryRecord, RecordId},
    taxonomy::{MemoryClass, MemoryKind, MemoryVisibility},
    trace::{TraceBlock, TraceEvent, TraceLink, TraceLinkError, summary_record_id},
};
use crate::pipeline::extract::body::ResolvedBody;

/// Maximum size of a stored trace-record body, in bytes. Anything larger
/// is truncated at a UTF-8 char boundary; the full bytes remain in
/// `sources/` referenced by `payload_hash`.
pub const TRACE_BODY_CAP: usize = 4 * 1024;

/// Failure modes for the trace projector.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TraceProjectError {
    /// The provided [`crate::domain::trace::TraceLink`] failed field-level
    /// validation.
    #[error("trace link: {0}")]
    Link(#[from] TraceLinkError),
    /// The capture event does not map to any known trace-event type.
    #[error("cannot classify capture event into a trace event type")]
    Unclassifiable,
    /// `RecordId::parse` rejected the derived id.
    #[error("record id derivation: {0}")]
    RecordId(#[from] DomainError),
    /// JSON serialization failed (e.g., serializing `TraceEvent`).
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Optional structured blocks to attach to a projected trace record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectedTraceBlocks {
    /// Ordered transcript blocks for this trace record.
    pub blocks: Vec<TraceBlock>,
}

impl ProjectedTraceBlocks {
    #[must_use]
    fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

/// Map a [`CaptureEvent`] to a [`TraceEvent`]. Static rules; no LLM.
///
/// Hook payloads route by hook name (brief §9.3), including the
/// `PreCompact` snapshot hook. Terminal payloads with a tool reference
/// are raw tool output, voice payloads are user utterances, and proactive
/// payloads whose kind explicitly names an agent/assistant message become
/// assistant trace messages. `TurnSummary` is generated post-hoc by
/// [`crate::pipeline::turn::summarize_turn`] and never reaches
/// `classify`.
///
/// # Errors
///
/// Returns [`TraceProjectError::Unclassifiable`] when the event does not
/// match any known mapping.
pub fn classify(event: &CaptureEvent) -> Result<TraceEvent, TraceProjectError> {
    match &event.payload {
        CapturePayload::Hook { hook_name, .. } => match hook_name.as_str() {
            "UserPromptSubmit" => Ok(TraceEvent::UserMessage),
            "PreToolUse" => Ok(TraceEvent::PreTool),
            "PostToolUse" => Ok(TraceEvent::PostTool),
            "ToolOutput" => Ok(TraceEvent::ToolOutput),
            "PreCompact" => Ok(TraceEvent::PreCompact),
            "Stop" => Ok(TraceEvent::Stop),
            _ => Err(TraceProjectError::Unclassifiable),
        },
        CapturePayload::Terminal { .. }
            if event
                .refs
                .as_ref()
                .and_then(|refs| refs.tool_id.as_ref())
                .is_some() =>
        {
            Ok(TraceEvent::ToolOutput)
        }
        CapturePayload::Voice { .. } => Ok(TraceEvent::UserMessage),
        CapturePayload::Proactive { kind, .. }
            if matches!(kind.as_str(), "agent_message" | "assistant_message") =>
        {
            Ok(TraceEvent::AgentMessage)
        }
        _ => Err(TraceProjectError::Unclassifiable),
    }
}

/// Project a [`CaptureEvent`] + verified [`ResolvedBody`] + [`TraceLink`]
/// into a [`MemoryRecord`] of [`MemoryKind::Trace`].
///
/// Pure: no I/O, no signing key required. The returned record carries a
/// zeroed placeholder signature (`ed25519:000…`); a later signing stage
/// must finalize it before the store accepts it. This mirrors the pattern
/// used by the sample-record factory in
/// `crate::domain::record::tests_export::sample_record`, which also
/// constructs a syntactically-valid-but-content-free signature.
///
/// # Errors
///
/// - [`TraceProjectError::Link`] if [`TraceLink::validate`] fails.
/// - [`TraceProjectError::RecordId`] if the derived record-id ULID is
///   somehow malformed (should not happen in practice — ULIDs from
///   `CaptureEventId` are already validated).
/// - [`TraceProjectError::Serde`] if serializing `classified` to JSON fails.
pub fn project(
    event: &CaptureEvent,
    classified: TraceEvent,
    resolved_body: &ResolvedBody<'_>,
    link: &TraceLink,
) -> Result<MemoryRecord, TraceProjectError> {
    project_with_blocks(
        event,
        classified,
        resolved_body,
        link,
        &ProjectedTraceBlocks::default(),
    )
}

/// Project a trace record with optional structured transcript blocks.
///
/// # Errors
///
/// Returns the same errors as [`project`], plus any `serde_json`
/// serialization failure while storing `trace_blocks`.
pub fn project_with_blocks(
    event: &CaptureEvent,
    classified: TraceEvent,
    resolved_body: &ResolvedBody<'_>,
    link: &TraceLink,
    trace_blocks: &ProjectedTraceBlocks,
) -> Result<MemoryRecord, TraceProjectError> {
    link.validate(classified)?;

    // Build extra_frontmatter["trace"]: the canonical linkage blob (spec §6.1, §8.1).
    let mut trace_obj = JsonMap::new();
    trace_obj.insert(
        "session_id".into(),
        Json::String(link.session_id.as_str().to_owned()),
    );
    trace_obj.insert("turn_id".into(), Json::String(link.turn_id.clone()));
    trace_obj.insert("sequence".into(), Json::Number(link.sequence.into()));
    trace_obj.insert(
        "capture_event_id".into(),
        Json::String(link.capture_event_id.to_string()),
    );
    trace_obj.insert(
        "payload_hash".into(),
        Json::String(event.payload_hash.as_str().to_owned()),
    );
    trace_obj.insert(
        "payload_ref".into(),
        Json::String(event.payload_ref.clone()),
    );
    if let Some(parent) = &link.parent_event_id {
        trace_obj.insert("parent_event_id".into(), Json::String(parent.to_string()));
    }
    if let Some(call) = &link.tool_call_id {
        trace_obj.insert("tool_call_id".into(), Json::String(call.clone()));
    }
    if !link.member_event_ids.is_empty() {
        let arr: Vec<Json> = link
            .member_event_ids
            .iter()
            .map(|id| Json::String(id.to_string()))
            .collect();
        trace_obj.insert("member_event_ids".into(), Json::Array(arr));
    }

    let mut extra: BTreeMap<String, Json> = BTreeMap::new();
    extra.insert("trace_event".into(), serde_json::to_value(classified)?);
    extra.insert("trace".into(), Json::Object(trace_obj));
    if !trace_blocks.is_empty() {
        extra.insert(
            "trace_blocks".into(),
            serde_json::to_value(&trace_blocks.blocks)?,
        );
    }

    let body = shape_body(classified, resolved_body.text());

    let scope = ScopeTuple {
        session_id: Some(link.session_id.as_str().to_owned()),
        ..ScopeTuple::default()
    };

    // Record id: deterministic for TurnSummary (idempotent across replays),
    // derived from capture_event_id otherwise (ULID by construction).
    let id: RecordId = if classified == TraceEvent::TurnSummary {
        summary_record_id(&link.session_id, &link.turn_id)
    } else {
        RecordId::parse(link.capture_event_id.as_str())?
    };

    let target_id = TargetId::parse(id.as_str())?;

    Ok(MemoryRecord {
        id,
        target_id,
        kind: MemoryKind::Trace,
        class: MemoryClass::Episodic,
        visibility: MemoryVisibility::Private,
        scope,
        body,
        source_ids: vec![source_id_from_payload_ref(&event.payload_ref)?],
        provenance: provenance_from_event(event),
        updated_at: event.captured_at.clone(),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 1.0,
        actor_chain: event.actor_chain.clone(),
        signature: placeholder_signature(),
        tags: Vec::new(),
        extra_frontmatter: extra,
        consent_model: None,
    })
}

/// Persist a `PreCompact` hook snapshot through the existing trace-record
/// path while preserving its distinct lifecycle boundary in the stored
/// `trace_event` field.
pub fn project_pre_compact_snapshot(
    event: &CaptureEvent,
    resolved_body: &ResolvedBody<'_>,
    link: &TraceLink,
) -> Result<MemoryRecord, TraceProjectError> {
    match &event.payload {
        CapturePayload::Hook { hook_name, .. } if hook_name == "PreCompact" => {}
        _ => return Err(TraceProjectError::Unclassifiable),
    }

    project(event, TraceEvent::PreCompact, resolved_body, link)
}

fn source_id_from_payload_ref(payload_ref: &str) -> Result<SourceId, TraceProjectError> {
    let stem = payload_ref
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.').map(|(stem, _)| stem).or(Some(name)))
        .ok_or(DomainError::EmptyField {
            field: "source_ids",
        })?;
    SourceId::parse(stem.to_owned()).map_err(TraceProjectError::from)
}

/// Build a [`Provenance`] from a [`CaptureEvent`].
///
/// - `source_sensor` = `event.sensor_id` (always a `snr:` identity on a
///   valid capture event).
/// - `created_at` / `updated_at` = `event.captured_at`.
/// - `originating_agent_id` = the `Author` chain entry's identity; falls
///   back to `sensor_id` if the chain is somehow empty (should not occur
///   on a validated event).
/// - `source_hash` = `event.payload_hash.as_str()` — the cryptographic
///   hash of the raw payload bytes stored in `sources/`.
/// - `consent_ref` = `"consent:pending"` — a P0 placeholder; the
///   consent journal integration lands in a later task.
/// - `llm_id_if_any` = `None` — trace records are captured verbatim,
///   not produced by an LLM.
fn provenance_from_event(event: &CaptureEvent) -> Provenance {
    let originating_agent_id = event
        .actor_chain
        .iter()
        .find(|e| e.role == ChainRole::Author)
        .map_or_else(|| event.sensor_id.clone(), |e| e.identity.clone());

    Provenance {
        source_sensor: event.sensor_id.clone(),
        created_at: event.captured_at.clone(),
        originating_agent_id,
        source_hash: event.payload_hash.as_str().to_owned(),
        consent_ref: "consent:pending".to_owned(),
        llm_id_if_any: None,
        source_refs: Vec::new(),
    }
}

/// Return a syntactically valid but content-free Ed25519 signature
/// placeholder. The returned value passes `Ed25519Signature::parse`
/// (correct prefix + 128 hex chars) but carries no key material.
/// A later signing stage replaces this before the record is accepted
/// by the store.
///
/// Pattern: all-zeros hex, matching the "unsigned" sentinel used in
/// `crate::domain::record::tests_export::sample_record` (which uses
/// all-`a`s for a *test record*; we use all-`0`s here to distinguish
/// a *pending-signature* record from a *test fixture* record).
fn placeholder_signature() -> Ed25519Signature {
    Ed25519Signature::parse(format!("ed25519:{}", "0".repeat(128))).unwrap_or_else(|_| {
        unreachable!("'0'.repeat(128) always satisfies Ed25519Signature::parse")
    })
}

/// Render the textual body for a given event type. Pure; no I/O.
///
/// Caller is responsible for passing already-privacy-filtered text. The
/// body is truncated at [`TRACE_BODY_CAP`] on a UTF-8 char boundary.
#[must_use]
pub(crate) fn shape_body(_event: TraceEvent, filtered_text: &str) -> String {
    truncate(filtered_text, TRACE_BODY_CAP)
}

fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        ActorChainEntry, ChainRole, Identity, Rfc3339Timestamp, SessionId,
        capture::{
            CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, PayloadHash,
            SourceFamily, TerminalContext,
        },
    };
    use crate::pipeline::extract::body;

    fn ts() -> Rfc3339Timestamp {
        Rfc3339Timestamp::parse("2026-04-27T00:00:00Z").expect("invariant: valid timestamp")
    }

    /// Build a minimal hook `CaptureEvent` for classification tests.
    ///
    /// Copied from `pipeline::squash::tests::hook_event` (squash.rs ~line 503),
    /// parameterised on `hook_name` instead of a fixed `"PostToolUse"`.
    fn mk_hook_event(hook_name: &str) -> CaptureEvent {
        CaptureEvent {
            event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .expect("invariant: fixed ULID is valid"),
            sensor_id: Identity::parse("snr:local:hook:cc-session:v1")
                .expect("invariant: fixed sensor identity is valid"),
            capture_mode: CaptureMode::Auto,
            actor_chain: vec![ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("snr:local:hook:cc-session:v1")
                    .expect("invariant: fixed sensor identity is valid"),
                at: ts(),
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
            payload_ref: "sources/hook/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into(),
            captured_at: ts(),
            payload: CapturePayload::Hook {
                hook_name: hook_name.into(),
                tool_name: None,
            },
            source_family: SourceFamily::Hook,
        }
    }

    fn mk_terminal_event(tool_id: Option<&str>) -> CaptureEvent {
        let mut event = mk_hook_event("UserPromptSubmit");
        event.sensor_id =
            Identity::parse("snr:local:terminal:default:v1").expect("valid terminal sensor");
        event.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: event.sensor_id.clone(),
            at: ts(),
        }];
        event.refs = Some(CaptureRefs {
            session_id: Some("sess".into()),
            turn_id: Some("turn".into()),
            tool_id: tool_id.map(ToOwned::to_owned),
        });
        event.payload_ref = "sources/terminal/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into();
        event.payload = CapturePayload::Terminal {
            command: "cargo test".into(),
            exit_code: Some(0),
            context: Some(TerminalContext::NonInteractiveOrStructured),
        };
        event.source_family = SourceFamily::Terminal;
        event
    }

    fn mk_voice_event() -> CaptureEvent {
        let mut event = mk_hook_event("UserPromptSubmit");
        event.sensor_id =
            Identity::parse("snr:local:voice:default:v1").expect("valid voice sensor");
        event.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: event.sensor_id.clone(),
            at: ts(),
        }];
        event.payload_ref = "sources/voice/01ARZ3NDEKTSV4RRFFQ69G5FAV.json".into();
        event.payload = CapturePayload::Voice {
            speaker_id: "unknown_speaker_01".into(),
            duration_ms: 2_000,
            confidence: 0.94,
        };
        event.source_family = SourceFamily::Voice;
        event
    }

    fn mk_proactive_event(kind: &str) -> CaptureEvent {
        let mut event = mk_hook_event("UserPromptSubmit");
        event.sensor_id =
            Identity::parse("snr:local:proactive:codex:v1").expect("valid proactive sensor");
        event.capture_mode = CaptureMode::Proactive;
        event.actor_chain = vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("agt:codex:gpt-5:main:v1").expect("valid agent"),
            at: ts(),
        }];
        event.payload_ref = "sources/proactive/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into();
        event.payload = CapturePayload::Proactive {
            kind: kind.into(),
            rationale: "captured final agent response".into(),
        };
        event.source_family = SourceFamily::Proactive;
        event
    }

    #[test]
    fn body_truncates_at_cap() {
        let big = "x".repeat(TRACE_BODY_CAP + 100);
        let body = shape_body(TraceEvent::ToolOutput, &big);
        assert!(body.len() <= TRACE_BODY_CAP);
    }

    #[test]
    fn body_below_cap_unchanged() {
        let body = shape_body(TraceEvent::UserMessage, "hello world");
        assert_eq!(body, "hello world");
    }

    #[test]
    fn truncate_respects_char_boundary() {
        // Build a string whose byte length crosses the cap mid multi-byte char.
        let mut s = "x".repeat(TRACE_BODY_CAP - 2);
        s.push('𝄞'); // 4 bytes (U+1D11E)
        let body = shape_body(TraceEvent::UserMessage, &s);
        assert!(body.is_char_boundary(body.len()));
    }

    #[test]
    fn classifies_user_prompt_submit() {
        assert_eq!(
            classify(&mk_hook_event("UserPromptSubmit")).unwrap(),
            TraceEvent::UserMessage
        );
    }

    #[test]
    fn classifies_pre_tool_use() {
        assert_eq!(
            classify(&mk_hook_event("PreToolUse")).unwrap(),
            TraceEvent::PreTool
        );
    }

    #[test]
    fn classifies_post_tool_use() {
        assert_eq!(
            classify(&mk_hook_event("PostToolUse")).unwrap(),
            TraceEvent::PostTool
        );
    }

    #[test]
    fn classifies_hook_tool_output() {
        let mut event = mk_hook_event("ToolOutput");
        event.refs.as_mut().expect("refs").tool_id = Some("toolu_1".into());
        assert_eq!(classify(&event).unwrap(), TraceEvent::ToolOutput);
    }

    #[test]
    fn classifies_terminal_tool_output_when_tool_ref_present() {
        assert_eq!(
            classify(&mk_terminal_event(Some("toolu_1"))).unwrap(),
            TraceEvent::ToolOutput
        );
    }

    #[test]
    fn classifies_voice_as_user_message() {
        assert_eq!(
            classify(&mk_voice_event()).unwrap(),
            TraceEvent::UserMessage
        );
    }

    #[test]
    fn rejects_terminal_without_tool_ref() {
        assert!(matches!(
            classify(&mk_terminal_event(None)).unwrap_err(),
            TraceProjectError::Unclassifiable
        ));
    }

    #[test]
    fn classifies_proactive_agent_message_kinds() {
        assert_eq!(
            classify(&mk_proactive_event("agent_message")).unwrap(),
            TraceEvent::AgentMessage
        );
        assert_eq!(
            classify(&mk_proactive_event("assistant_message")).unwrap(),
            TraceEvent::AgentMessage
        );
    }

    #[test]
    fn rejects_other_proactive_kinds() {
        assert!(matches!(
            classify(&mk_proactive_event("knowledge_gap")).unwrap_err(),
            TraceProjectError::Unclassifiable
        ));
    }

    #[test]
    fn classifies_pre_compact() {
        assert_eq!(
            classify(&mk_hook_event("PreCompact")).unwrap(),
            TraceEvent::PreCompact
        );
    }

    #[test]
    fn classifies_stop() {
        assert_eq!(classify(&mk_hook_event("Stop")).unwrap(), TraceEvent::Stop);
    }

    #[test]
    fn projects_pre_compact_snapshot_through_trace_path() {
        let event = mk_hook_event("PreCompact");
        let resolved = body::for_test("before compact");
        let link = TraceLink {
            session_id: SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .expect("invariant: valid ULID"),
            turn_id: "turn-1".into(),
            sequence: 7,
            capture_event_id: event.event_id.clone(),
            parent_event_id: None,
            tool_call_id: None,
            member_event_ids: Vec::new(),
        };

        let record = project_pre_compact_snapshot(&event, &resolved, &link).unwrap();
        assert_eq!(record.kind, MemoryKind::Trace);
        assert_eq!(record.body, "before compact");
        assert_eq!(
            record.extra_frontmatter.get("trace_event").unwrap(),
            "pre_compact"
        );
    }

    #[test]
    fn unknown_hook_rejected() {
        assert!(matches!(
            classify(&mk_hook_event("UnknownHook")).unwrap_err(),
            TraceProjectError::Unclassifiable
        ));
    }

    #[test]
    fn session_start_rejected() {
        assert!(matches!(
            classify(&mk_hook_event("SessionStart")).unwrap_err(),
            TraceProjectError::Unclassifiable
        ));
    }

    #[test]
    fn projects_user_message_record() {
        let event = mk_hook_event("UserPromptSubmit");
        let resolved = body::for_test("hello world");
        let link = TraceLink {
            session_id: SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .expect("invariant: valid ULID"),
            turn_id: "turn-1".into(),
            sequence: 0,
            capture_event_id: event.event_id.clone(),
            parent_event_id: None,
            tool_call_id: None,
            member_event_ids: Vec::new(),
        };
        let record = project(&event, TraceEvent::UserMessage, &resolved, &link).unwrap();
        assert_eq!(record.kind, MemoryKind::Trace);
        assert_eq!(record.class, MemoryClass::Episodic);
        assert_eq!(record.visibility, MemoryVisibility::Private);
        assert_eq!(
            record.scope.session_id.as_deref(),
            Some("01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );
        assert!(record.body.contains("hello world"));
        let trace_meta = record.extra_frontmatter.get("trace").unwrap();
        assert_eq!(trace_meta["turn_id"], "turn-1");
        assert_eq!(trace_meta["sequence"], 0);
        assert_eq!(
            record.extra_frontmatter.get("trace_event").unwrap(),
            "user_message"
        );
    }

    #[test]
    fn project_validates_link() {
        // PreTool without tool_call_id → Link error.
        let event = mk_hook_event("PreToolUse");
        let resolved = body::for_test("some tool input");
        let link = TraceLink {
            session_id: SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .expect("invariant: valid ULID"),
            turn_id: "t".into(),
            sequence: 0,
            capture_event_id: event.event_id.clone(),
            parent_event_id: None,
            tool_call_id: None, // missing — required for PreTool
            member_event_ids: Vec::new(),
        };
        let err = project(&event, TraceEvent::PreTool, &resolved, &link).unwrap_err();
        assert!(
            matches!(err, TraceProjectError::Link(_)),
            "expected Link error, got {err:?}"
        );
    }
}
