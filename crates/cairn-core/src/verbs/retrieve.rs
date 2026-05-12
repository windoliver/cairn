//! Pure data shaping for the `retrieve` verb.

use crate::domain::{MemoryKind, MemoryRecord};
use crate::generated::common::{Cursor, ScopeFilter, Ulid};
use crate::generated::envelope::RetrieveData;
use crate::generated::verbs::retrieve::{
    DataFolder, DataProfile, DataProfileSubject, DataRecord, DataScope, DataSession, DataTurn,
    KeyFacts, ProfileHalf, ProfileLine, RecordRef, TurnItem, TurnItemRole,
};

const SNIPPET_CHARS: usize = 160;

/// Shape a full [`MemoryRecord`] as typed retrieve-record response data.
#[must_use]
pub fn record_data(record: &MemoryRecord) -> RetrieveData {
    RetrieveData::Record(DataRecord {
        body: Some(record.body.clone()),
        frontmatter: record_frontmatter(record),
        kind: record.kind.as_str().to_owned(),
        record_id: wire_ulid(record.id.as_str()),
    })
}

/// Shape an authorized record miss without returning protected data.
#[must_use]
pub fn missing_record_data(record_id: Ulid) -> RetrieveData {
    RetrieveData::Record(DataRecord {
        body: None,
        frontmatter: None,
        kind: "unknown".to_owned(),
        record_id,
    })
}

/// Shape a compact reference used by folder and scope retrieval.
#[must_use]
pub fn record_ref(record: &MemoryRecord) -> RecordRef {
    RecordRef {
        kind: record.kind.as_str().to_owned(),
        record_id: wire_ulid(record.id.as_str()),
        snippet: snippet(&record.body),
    }
}

/// Shape folder retrieval data from already-authorized records.
#[must_use]
pub fn folder_data(path: String, depth: Option<u64>, records: &[MemoryRecord]) -> RetrieveData {
    RetrieveData::Folder(DataFolder {
        depth,
        items: records.iter().map(record_ref).collect(),
        path,
    })
}

/// Shape scoped retrieval data from already-authorized records.
#[must_use]
pub fn scope_data(
    scope: ScopeFilter,
    records: &[MemoryRecord],
    next_cursor: Option<Cursor>,
) -> RetrieveData {
    RetrieveData::Scope(DataScope {
        items: records.iter().map(record_ref).collect(),
        next_cursor,
        scope,
    })
}

/// Shape session retrieval data from ordered turn items.
#[must_use]
pub fn session_data(
    session_id: String,
    records: &[MemoryRecord],
    next_cursor: Option<Cursor>,
) -> RetrieveData {
    session_data_with_options(session_id, records, next_cursor, false, false)
}

/// Shape session retrieval data, honoring optional generated include flags.
#[must_use]
pub fn session_data_with_options(
    session_id: String,
    records: &[MemoryRecord],
    next_cursor: Option<Cursor>,
    include_reasoning: bool,
    include_tool_calls: bool,
) -> RetrieveData {
    RetrieveData::Session(DataSession {
        items: records
            .iter()
            .map(|record| turn_item_with_options(record, include_reasoning, include_tool_calls))
            .collect(),
        next_cursor,
        session_id,
    })
}

/// Shape turn retrieval data from ordered trace records.
#[must_use]
pub fn turn_data(session_id: String, turn_id: String, records: &[MemoryRecord]) -> RetrieveData {
    turn_data_with_options(session_id, turn_id, records, false, false)
}

/// Shape an authorized turn miss as an empty turn payload.
#[must_use]
pub fn empty_turn_data(session_id: String, turn_id: String) -> RetrieveData {
    RetrieveData::Turn(DataTurn {
        session_id,
        turn: Vec::new(),
        turn_id,
    })
}

/// Shape turn retrieval data, honoring optional generated include flags.
#[must_use]
pub fn turn_data_with_options(
    session_id: String,
    turn_id: String,
    records: &[MemoryRecord],
    include_reasoning: bool,
    include_tool_calls: bool,
) -> RetrieveData {
    RetrieveData::Turn(DataTurn {
        session_id,
        turn: records
            .iter()
            .map(|record| turn_item_with_options(record, include_reasoning, include_tool_calls))
            .collect(),
        turn_id,
    })
}

/// Shape profile retrieval data. The caller is responsible for passing at
/// least one subject dimension, matching the generated `RetrieveArgs` gate.
#[must_use]
pub fn profile_data(
    user: Option<String>,
    agent: Option<String>,
    records: &[MemoryRecord],
) -> RetrieveData {
    RetrieveData::Profile(DataProfile {
        dynamic: profile_half(records, "dynamic"),
        r#static: profile_half(records, "static"),
        subject: DataProfileSubject { agent, user },
        updated_at: latest_profile_update(records),
    })
}

/// Shape a single trace record as a retrieve turn/session item.
#[must_use]
pub fn turn_item(record: &MemoryRecord) -> TurnItem {
    turn_item_with_options(record, false, false)
}

/// Shape a trace record as a turn/session item with optional fields.
#[must_use]
pub fn turn_item_with_options(
    record: &MemoryRecord,
    include_reasoning: bool,
    include_tool_calls: bool,
) -> TurnItem {
    let event = trace_event(record);
    let is_tool_event = matches!(
        event.as_deref(),
        Some("pre_tool" | "post_tool" | "tool_output")
    );
    let is_reasoning = record.kind == MemoryKind::Reasoning;
    TurnItem {
        content: if (is_tool_event && !include_tool_calls) || (is_reasoning && !include_reasoning) {
            None
        } else {
            non_empty(record.body.clone())
        },
        reasoning: include_reasoning
            .then(|| reasoning_content(record))
            .flatten(),
        linkage: None,
        role: turn_item_role(record),
        tool_calls: include_tool_calls.then(|| tool_calls(record)).flatten(),
        turn_id: trace_turn_id(record).unwrap_or_else(|| record.id.as_str().to_owned()),
    }
}

/// Build a compact, whitespace-normalized body preview.
#[must_use]
pub fn snippet(body: &str) -> Option<String> {
    let normalized = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    let mut chars = normalized.chars();
    let head: String = chars.by_ref().take(SNIPPET_CHARS).collect();
    if chars.next().is_some() {
        let truncated: String = head.chars().take(SNIPPET_CHARS.saturating_sub(3)).collect();
        Some(format!("{truncated}..."))
    } else {
        Some(head)
    }
}

fn record_frontmatter(record: &MemoryRecord) -> Option<serde_json::Value> {
    if record.extra_frontmatter.is_empty() {
        return None;
    }
    Some(serde_json::Value::Object(
        record
            .extra_frontmatter
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    ))
}

fn wire_ulid(value: &str) -> Ulid {
    Ulid(value.to_owned())
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

fn reasoning_content(record: &MemoryRecord) -> Option<String> {
    if record.kind == MemoryKind::Reasoning {
        non_empty(record.body.clone())
    } else {
        None
    }
}

fn turn_item_role(record: &MemoryRecord) -> TurnItemRole {
    match trace_event(record).as_deref() {
        Some("user_message") => TurnItemRole::User,
        Some("pre_tool" | "post_tool" | "tool_output") => TurnItemRole::Tool,
        Some("stop" | "turn_summary") => TurnItemRole::System,
        _ => TurnItemRole::Assistant,
    }
}

fn tool_calls(record: &MemoryRecord) -> Option<Vec<serde_json::Value>> {
    if !matches!(
        trace_event(record).as_deref(),
        Some("pre_tool" | "post_tool")
    ) {
        return None;
    }
    let trace = record
        .extra_frontmatter
        .get("trace")
        .and_then(serde_json::Value::as_object)?;
    let tool_call_id = trace
        .get("tool_call_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if tool_call_id.is_empty() {
        return None;
    }
    Some(vec![serde_json::json!({ "tool_call_id": tool_call_id })])
}

fn trace_event(record: &MemoryRecord) -> Option<String> {
    record
        .extra_frontmatter
        .get("trace_event")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn trace_turn_id(record: &MemoryRecord) -> Option<String> {
    record
        .extra_frontmatter
        .get("trace")
        .and_then(|trace| trace.get("turn_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn profile_half(records: &[MemoryRecord], section: &str) -> ProfileHalf {
    let lines = profile_lines(records, section);
    let mut key_facts = empty_key_facts();
    if section == "static" {
        key_facts.preferences = lines;
    } else {
        key_facts.current_issues = lines;
    }
    ProfileHalf {
        historical_summary: String::new(),
        key_facts,
        summary: String::new(),
    }
}

fn profile_lines(records: &[MemoryRecord], section: &str) -> Vec<ProfileLine> {
    let mut lines = std::collections::BTreeMap::<String, ProfileLine>::new();
    for record in records {
        let Some(section_obj) = profile_section(record, section) else {
            continue;
        };
        for (key, value) in section_obj {
            lines.entry(key.clone()).or_insert_with(|| ProfileLine {
                confidence: f64::from(record.confidence),
                evidence: vec![wire_ulid(record.id.as_str())],
                value: format!("{key}: {}", profile_value(value)),
            });
        }
    }
    lines.into_values().collect()
}

fn profile_section<'a>(
    record: &'a MemoryRecord,
    section: &str,
) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
    let flat_key = format!("profile_{section}");
    record
        .extra_frontmatter
        .get("profile")
        .and_then(|profile| profile.get(section))
        .or_else(|| record.extra_frontmatter.get(&flat_key))
        .and_then(serde_json::Value::as_object)
}

fn profile_value(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

fn latest_profile_update(records: &[MemoryRecord]) -> String {
    records
        .iter()
        .map(|record| &record.updated_at)
        .max_by(|a, b| a.cmp_chronological(b))
        .map_or_else(crate::time::now_rfc3339_seconds, |updated_at| {
            updated_at.as_str().to_owned()
        })
}

fn empty_key_facts() -> KeyFacts {
    KeyFacts {
        addressed_issues: Vec::new(),
        current_issues: Vec::new(),
        devices: Vec::new(),
        known_entities: Vec::new(),
        preferences: Vec::new(),
        recurring_issues: Vec::new(),
        software: Vec::new(),
    }
}
