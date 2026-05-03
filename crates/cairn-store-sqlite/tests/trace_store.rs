//! Integration tests for trace-record store methods (issue #77).
//!
//! Covers `StoreTx::list_trace_events`: ordering by `trace_sequence`,
//! exclusion of `turn_summary` rows, and exclusion of tombstoned rows.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::TombstoneReason;
use cairn_core::domain::{MemoryRecord, RecordId, SessionId, TargetId};
use cairn_core::domain::record::tests_export::sample_record;
use cairn_store_sqlite::open_in_memory;
use serde_json::{Map as JsonMap, Value as Json};

// ── test helpers ──────────────────────────────────────────────────────────────

/// Build a minimal trace `MemoryRecord` for a given session/turn/sequence.
///
/// Uses `sample_record()` as a skeleton, then patches the fields the
/// migration-generated columns key on:
/// - `extra_frontmatter["trace_event"]`  → `"user_message"`
/// - `extra_frontmatter["trace"]`        → `{ session_id, turn_id, sequence,
///                                           capture_event_id, payload_hash,
///                                           payload_ref }`
/// - `id` / `target_id`                 → derived from `capture_event_id`
///   (unique per call site via the `capture_event_id` parameter)
#[allow(
    clippy::expect_used,
    reason = "fixture helpers: panic on invalid input is intentional"
)]
fn mk_trace_record(session_id: &SessionId, turn_id: &str, sequence: u64, capture_event_id: &str) -> MemoryRecord {
    let mut r = sample_record();
    // Use the capture_event_id as both record_id and target_id so every
    // inserted row has a distinct identity.
    r.id = RecordId::parse(capture_event_id).expect("test: valid ULID capture_event_id");
    r.target_id = TargetId::parse(capture_event_id).expect("test: valid ULID capture_event_id");

    // Build the trace linkage object.
    let mut trace_obj = JsonMap::new();
    trace_obj.insert("session_id".into(), Json::String(session_id.as_str().to_owned()));
    trace_obj.insert("turn_id".into(), Json::String(turn_id.to_owned()));
    trace_obj.insert("sequence".into(), Json::Number(sequence.into()));
    trace_obj.insert("capture_event_id".into(), Json::String(capture_event_id.to_owned()));
    trace_obj.insert(
        "payload_hash".into(),
        Json::String(
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned(),
        ),
    );
    trace_obj.insert("payload_ref".into(), Json::String(format!("sources/hook/{capture_event_id}.txt")));

    let mut extra: BTreeMap<String, Json> = BTreeMap::new();
    extra.insert("trace_event".into(), Json::String("user_message".to_owned()));
    extra.insert("trace".into(), Json::Object(trace_obj));
    r.extra_frontmatter = extra;

    r
}

/// Wrapper around [`mk_trace_record`] that overrides the `payload_hash`
/// field in the trace object.
#[allow(
    clippy::expect_used,
    reason = "fixture helpers: panic on invalid input is intentional"
)]
fn mk_trace_record_with_hash(
    session_id: &SessionId,
    turn_id: &str,
    sequence: u64,
    capture_event_id: &str,
    payload_hash: &str,
) -> MemoryRecord {
    let mut r = mk_trace_record(session_id, turn_id, sequence, capture_event_id);
    r.extra_frontmatter
        .get_mut("trace")
        .expect("trace key set by mk_trace_record")
        .as_object_mut()
        .expect("trace is an object")
        .insert("payload_hash".into(), Json::String(payload_hash.to_owned()));
    r
}

/// Build a `turn_summary` record for the given session/turn.
///
/// The `trace_event` column is gated to `'turn_summary'`; this helper
/// sets that and gives the record a unique id derived from
/// `cairn_core::domain::trace::summary_record_id`.
#[allow(
    clippy::expect_used,
    reason = "fixture helpers: panic on invalid input is intentional"
)]
fn mk_summary_record(session_id: &SessionId, turn_id: &str, member_event_ids: &[&str]) -> MemoryRecord {
    // Derive the deterministic summary id exactly as the real pipeline does.
    let summary_id = cairn_core::domain::trace::summary_record_id(session_id, turn_id);
    let summary_id_str = summary_id.as_str().to_owned();

    let mut r = sample_record();
    r.id = summary_id;
    r.target_id = TargetId::parse(summary_id_str.clone()).expect("test: summary_id is valid ULID");

    let mut trace_obj = JsonMap::new();
    trace_obj.insert("session_id".into(), Json::String(session_id.as_str().to_owned()));
    trace_obj.insert("turn_id".into(), Json::String(turn_id.to_owned()));
    // Summary rows have no sequence; sequence column will be NULL.
    trace_obj.insert("capture_event_id".into(), Json::String(summary_id_str.clone()));
    let members: Vec<Json> = member_event_ids
        .iter()
        .map(|s| Json::String((*s).to_owned()))
        .collect();
    trace_obj.insert("member_event_ids".into(), Json::Array(members));
    trace_obj.insert(
        "payload_hash".into(),
        Json::String(
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_owned(),
        ),
    );
    trace_obj.insert("payload_ref".into(), Json::String(format!("sources/hook/{summary_id_str}.txt")));

    let mut extra: BTreeMap<String, Json> = BTreeMap::new();
    extra.insert("trace_event".into(), Json::String("turn_summary".to_owned()));
    extra.insert("trace".into(), Json::Object(trace_obj));
    r.extra_frontmatter = extra;

    r
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn list_trace_events_orders_by_sequence() {
    let store = open_in_memory().await.expect("open store");
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid session_id");

    // Insert three trace records with sequences 2, 0, 1 (out of order).
    store
        .with_tx(move |tx| {
            for (capture_event_id, seq) in [
                ("01ARZ3NDEKTSV4RRFFQ69G5FAA", 2_u64),
                ("01ARZ3NDEKTSV4RRFFQ69G5FAB", 0),
                ("01ARZ3NDEKTSV4RRFFQ69G5FAC", 1),
            ] {
                tx.upsert(&mk_trace_record(&session_id, "turn-1", seq, capture_event_id))?;
            }
            let rows = tx.list_trace_events(&session_id, "turn-1")?;
            let seqs: Vec<u64> = rows
                .iter()
                .map(|r| {
                    r.extra_frontmatter["trace"]["sequence"]
                        .as_u64()
                        .expect("sequence present")
                })
                .collect();
            assert_eq!(seqs, vec![0, 1, 2], "records must be ordered by trace_sequence ASC");
            Ok(())
        })
        .await
        .expect("with_tx");
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn list_trace_events_excludes_summary() {
    let store = open_in_memory().await.expect("open store");
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid session_id");

    store
        .with_tx(move |tx| {
            tx.upsert(&mk_trace_record(
                &session_id,
                "turn-1",
                0,
                "01ARZ3NDEKTSV4RRFFQ69G5FAA",
            ))?;
            tx.upsert(&mk_summary_record(&session_id, "turn-1", &[]))?;

            let rows = tx.list_trace_events(&session_id, "turn-1")?;
            assert_eq!(rows.len(), 1, "summary row must be excluded");
            assert_ne!(
                rows[0]
                    .extra_frontmatter
                    .get("trace_event")
                    .and_then(Json::as_str),
                Some("turn_summary"),
                "returned row must not be a summary"
            );
            Ok(())
        })
        .await
        .expect("with_tx");
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn list_trace_events_excludes_tombstoned() {
    let store = open_in_memory().await.expect("open store");
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid session_id");

    store
        .with_tx(move |tx| {
            let r = mk_trace_record(&session_id, "turn-1", 0, "01ARZ3NDEKTSV4RRFFQ69G5FAA");
            tx.upsert(&r)?;
            tx.tombstone(&r.id, TombstoneReason::Forget)?;

            let rows = tx.list_trace_events(&session_id, "turn-1")?;
            assert!(rows.is_empty(), "tombstoned row must be excluded");
            Ok(())
        })
        .await
        .expect("with_tx");
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn turn_summary_exists_after_write() {
    let store = open_in_memory().await.expect("open store");
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
    store
        .with_tx(move |tx| {
            assert!(!tx.turn_summary_exists(&session_id, "turn-1")?);
            let s = mk_summary_record(&session_id, "turn-1", &["01ARZ3NDEKTSV4RRFFQ69G5FAA"]);
            tx.upsert(&s)?;
            assert!(tx.turn_summary_exists(&session_id, "turn-1")?);
            Ok(())
        })
        .await
        .expect("with_tx");
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn turn_summary_exists_false_for_other_turn() {
    let store = open_in_memory().await.expect("open store");
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
    store
        .with_tx(move |tx| {
            let s = mk_summary_record(&session_id, "turn-1", &["01ARZ3NDEKTSV4RRFFQ69G5FAA"]);
            tx.upsert(&s)?;
            assert!(!tx.turn_summary_exists(&session_id, "turn-2")?);
            Ok(())
        })
        .await
        .expect("with_tx");
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn payload_hash_count_in_scope_basic() {
    let store = open_in_memory().await.expect("open store");
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
    let hash = "sha256:deadbeef";
    store
        .with_tx(move |tx| {
            // Two records with the same hash under the same scope (scope.user = "hmn:tafeng").
            let r1 = mk_trace_record_with_hash(
                &session_id,
                "turn-1",
                0,
                "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                hash,
            );
            let r2 = mk_trace_record_with_hash(
                &session_id,
                "turn-1",
                1,
                "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                hash,
            );
            tx.upsert(&r1)?;
            tx.upsert(&r2)?;

            // No exclusion → 2.
            let n = tx.payload_hash_count_in_scope(hash, None, Some("hmn:tafeng"), None, &[])?;
            assert_eq!(n, 2, "expected 2 matching records");

            // Exclude r1 → 1.
            let n = tx.payload_hash_count_in_scope(
                hash,
                None,
                Some("hmn:tafeng"),
                None,
                &[r1.id.as_str()],
            )?;
            assert_eq!(n, 1, "expected 1 after excluding r1");

            // Wrong scope → 0.
            let n =
                tx.payload_hash_count_in_scope(hash, Some("other-tenant"), None, None, &[])?;
            assert_eq!(n, 0, "wrong tenant scope should match 0");

            // Different hash → 0.
            let n =
                tx.payload_hash_count_in_scope("sha256:other", None, Some("hmn:tafeng"), None, &[])?;
            assert_eq!(n, 0, "different hash should match 0");
            Ok(())
        })
        .await
        .expect("with_tx");
}
