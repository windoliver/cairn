//! Trace-record fixtures for issue #77 / #90 test suites.

use std::collections::BTreeMap;

use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::trace::summary_record_id;
use cairn_core::domain::{MemoryRecord, SessionId, TargetId};
use serde_json::{Map as JsonMap, Value as Json};

/// Construct a sample `turn_summary` trace record for a given session and
/// sequence number.
///
/// Fields are filled with deterministic test data. The turn id is derived
/// as `"turn-{sequence}"` so each call with a distinct sequence produces a
/// distinct `(trace_session_id, trace_turn_id)` pair, satisfying the
/// `records_trace_summary` unique index.
///
/// The `trace.sequence` field is set to `sequence` so that
/// `SqliteMemoryStore::list_trace_turns`'s `trace_sequence > since_sequence`
/// predicate behaves predictably in tests.
///
/// # Panics
///
/// Panics if `session` is not a valid `SessionId` (26-char Crockford base32).
/// Test callers should always pass well-formed ULID strings.
#[must_use]
#[allow(
    clippy::expect_used,
    reason = "fixture factory: invalid inputs indicate a test data bug; panic is correct"
)]
pub fn sample_turn_summary(session: &str, sequence: u32) -> MemoryRecord {
    let session_id =
        SessionId::parse(session).expect("sample_turn_summary: session must be a valid SessionId");
    let turn_id = format!("turn-{sequence}");

    // Deterministic summary record id — same algorithm as the real pipeline.
    let summary_id = summary_record_id(&session_id, &turn_id);
    let summary_id_str = summary_id.as_str().to_owned();

    let mut r = sample_record();
    r.id = summary_id;
    r.target_id =
        TargetId::parse(&summary_id_str).expect("summary_record_id always yields valid ULID");

    // Build trace linkage object.
    let mut trace_obj = JsonMap::new();
    trace_obj.insert(
        "session_id".into(),
        Json::String(session_id.as_str().to_owned()),
    );
    trace_obj.insert("turn_id".into(), Json::String(turn_id.clone()));
    // Use the caller-supplied sequence so list_trace_turns paging works.
    trace_obj.insert("sequence".into(), Json::Number(u64::from(sequence).into()));
    // capture_event_id for a summary equals its deterministic record id.
    trace_obj.insert(
        "capture_event_id".into(),
        Json::String(summary_id_str.clone()),
    );
    trace_obj.insert(
        "member_event_ids".into(),
        Json::Array(vec![Json::String(format!("member-{turn_id}"))]),
    );

    let mut extra: BTreeMap<String, Json> = BTreeMap::new();
    extra.insert(
        "trace_event".into(),
        Json::String("turn_summary".to_owned()),
    );
    extra.insert("trace".into(), Json::Object(trace_obj));
    r.extra_frontmatter = extra;

    r
}
