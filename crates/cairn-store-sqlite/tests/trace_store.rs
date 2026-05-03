//! Integration tests for trace-record store methods (issue #77).
//!
//! Covers `StoreTx::list_trace_events`: ordering by `trace_sequence`,
//! exclusion of `turn_summary` rows, and exclusion of tombstoned rows.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::TombstoneReason;
use cairn_core::domain::{MemoryRecord, RecordId, SessionId, TargetId};
use cairn_core::domain::record::tests_export::sample_record;
use cairn_store_sqlite::error::StoreError;
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

/// Build a trace record where `trace_event`, `tool_call_id`, and
/// `parent_event_id` are all caller-controlled.
///
/// This is the full-featured builder used by link-validation tests.
/// Pass `None` for `tool_call_id` / `parent_event_id` to omit those
/// fields from `extra_frontmatter.trace`.
#[allow(
    clippy::expect_used,
    reason = "fixture helpers: panic on invalid input is intentional"
)]
fn mk_trace_record_with_event(
    session_id: &SessionId,
    turn_id: &str,
    sequence: u64,
    capture_event_id: &str,
    trace_event: &str,
    tool_call_id: Option<&str>,
    parent_event_id: Option<&str>,
) -> MemoryRecord {
    let mut r = mk_trace_record(session_id, turn_id, sequence, capture_event_id);
    // Override trace_event.
    r.extra_frontmatter
        .insert("trace_event".into(), Json::String(trace_event.to_owned()));
    // Patch optional trace sub-fields.
    let trace = r
        .extra_frontmatter
        .get_mut("trace")
        .expect("trace key set by mk_trace_record")
        .as_object_mut()
        .expect("trace is an object");
    if let Some(tcid) = tool_call_id {
        trace.insert("tool_call_id".into(), Json::String(tcid.to_owned()));
    }
    if let Some(pid) = parent_event_id {
        trace.insert("parent_event_id".into(), Json::String(pid.to_owned()));
    }
    r
}

/// Like [`mk_trace_record`] but also sets `updated_at` on the record to
/// `captured_at`, mirroring the projector contract where `updated_at` is
/// derived from the originating `CaptureEvent`'s timestamp.
///
/// Used by renumber tests that need precise chronological ordering.
#[allow(
    clippy::expect_used,
    reason = "fixture helpers: panic on invalid input is intentional"
)]
fn mk_trace_record_at(
    session_id: &SessionId,
    turn_id: &str,
    sequence: u64,
    capture_event_id: &str,
    captured_at: &str,
) -> MemoryRecord {
    use cairn_core::domain::Rfc3339Timestamp;
    let mut r = mk_trace_record(session_id, turn_id, sequence, capture_event_id);
    r.updated_at = Rfc3339Timestamp::parse(captured_at)
        .expect("test: captured_at must be a valid RFC3339 timestamp");
    r
}

/// Convenience alias used by tests that need an in-memory store.
async fn test_store_in_memory() -> cairn_store_sqlite::SqliteMemoryStore {
    open_in_memory().await.expect("open in-memory store")
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

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn upsert_trace_is_idempotent_on_capture_event_id() {
    let store = open_in_memory().await.expect("open store");
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
    let event_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let record = mk_trace_record(&session_id, "turn-1", 0, event_id);

    store
        .with_tx({
            let r = record.clone();
            move |tx| {
                tx.upsert_trace(&r)?;
                Ok(())
            }
        })
        .await
        .unwrap();
    store
        .with_tx({
            let r = record.clone();
            move |tx| {
                tx.upsert_trace(&r)?;
                Ok(())
            }
        })
        .await
        .unwrap();

    store
        .with_tx(move |tx| {
            let rows = tx.list_trace_events(&session_id, "turn-1")?;
            assert_eq!(
                rows.len(),
                1,
                "duplicate capture_event_id must not produce two rows"
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn upsert_trace_rejects_duplicate_sequence() {
    let store = open_in_memory().await.expect("open store");
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
    let r1 = mk_trace_record(&session_id, "turn-1", 0, "01ARZ3NDEKTSV4RRFFQ69G5FAA");
    let r2 = mk_trace_record(&session_id, "turn-1", 0, "01ARZ3NDEKTSV4RRFFQ69G5FAB");

    let result = store
        .with_tx({
            let r1 = r1.clone();
            let r2 = r2.clone();
            move |tx| {
                tx.upsert_trace(&r1)?;
                tx.upsert_trace(&r2)?;
                Ok(())
            }
        })
        .await;
    let err = result.expect_err("duplicate sequence should error");
    assert!(
        matches!(err, StoreError::TraceSequenceConflict { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn upsert_trace_allows_same_sequence_after_first_record_replayed() {
    // Replaying r1 then upserting it again should still succeed (idempotent).
    let store = open_in_memory().await.expect("open store");
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
    let r1 = mk_trace_record(&session_id, "turn-1", 0, "01ARZ3NDEKTSV4RRFFQ69G5FAA");
    store
        .with_tx({
            let r = r1.clone();
            move |tx| {
                tx.upsert_trace(&r)?;
                tx.upsert_trace(&r)?;
                Ok(())
            }
        })
        .await
        .unwrap();
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn out_of_order_backfill_renumbers() {
    let store = test_store_in_memory().await;
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");

    // Construct three records: a (t=2), b (t=3), c (t=1).
    let a = mk_trace_record_at(
        &session_id,
        "turn-1",
        999,
        "01ARZ3NDEKTSV4RRFFQ69G5FAA",
        "2026-05-02T00:00:02Z",
    );
    let b = mk_trace_record_at(
        &session_id,
        "turn-1",
        999,
        "01ARZ3NDEKTSV4RRFFQ69G5FAB",
        "2026-05-02T00:00:03Z",
    );
    let c = mk_trace_record_at(
        &session_id,
        "turn-1",
        999,
        "01ARZ3NDEKTSV4RRFFQ69G5FAC",
        "2026-05-02T00:00:01Z",
    );

    store
        .with_tx({
            let a = a.clone();
            let b = b.clone();
            let c = c.clone();
            let session_id = session_id.clone();
            move |tx| {
                // First two events arrive in order and are renumbered.
                tx.renumber_turn_with(&session_id, "turn-1", &[a, b])?;
                // Late arrival of c (earliest captured_at) triggers a full renumber.
                tx.renumber_turn_with(&session_id, "turn-1", &[c])?;

                let rows = tx.list_trace_events(&session_id, "turn-1")?;

                // Final order must be c (t=1), a (t=2), b (t=3).
                let order: Vec<&str> = rows
                    .iter()
                    .map(|r| {
                        r.extra_frontmatter["trace"]["capture_event_id"]
                            .as_str()
                            .expect("capture_event_id present")
                    })
                    .collect();
                assert_eq!(
                    order,
                    vec![
                        "01ARZ3NDEKTSV4RRFFQ69G5FAC", // captured_at = t1
                        "01ARZ3NDEKTSV4RRFFQ69G5FAA", // captured_at = t2
                        "01ARZ3NDEKTSV4RRFFQ69G5FAB", // captured_at = t3
                    ],
                    "rows must be ordered by captured_at after renumber"
                );

                let seqs: Vec<u64> = rows
                    .iter()
                    .map(|r| {
                        r.extra_frontmatter["trace"]["sequence"]
                            .as_u64()
                            .expect("sequence present")
                    })
                    .collect();
                assert_eq!(seqs, vec![0, 1, 2], "sequences must be 0..N after renumber");

                Ok(())
            }
        })
        .await
        .expect("with_tx");
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn link_validation_passes_when_parent_in_turn() {
    let store = test_store_in_memory().await;
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
    store
        .with_tx({
            let session_id = session_id.clone();
            move |tx| {
                // pre_tool with tool_call_id="call_abc"
                let pre = mk_trace_record_with_event(
                    &session_id,
                    "turn-1",
                    0,
                    "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                    "pre_tool",
                    Some("call_abc"),
                    None,
                );
                // post_tool referencing the pre by capture_event_id
                let post = mk_trace_record_with_event(
                    &session_id,
                    "turn-1",
                    1,
                    "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                    "post_tool",
                    Some("call_abc"),
                    Some("01ARZ3NDEKTSV4RRFFQ69G5FAA"),
                );
                tx.upsert_trace(&pre)?;
                tx.upsert_trace(&post)?;
                tx.validate_turn_links(&session_id, "turn-1")?;
                Ok(())
            }
        })
        .await
        .unwrap();
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn link_validation_rejects_missing_parent() {
    let store = test_store_in_memory().await;
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
    let result = store
        .with_tx({
            let session_id = session_id.clone();
            move |tx| {
                // post_tool whose parent_event_id never landed.
                let post = mk_trace_record_with_event(
                    &session_id,
                    "turn-1",
                    0,
                    "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                    "post_tool",
                    Some("call_abc"),
                    Some("01ARZ3NDEKTSV4RRFFQ69G5FFF"), // does not exist
                );
                tx.upsert_trace(&post)?;
                tx.validate_turn_links(&session_id, "turn-1")?;
                Ok(())
            }
        })
        .await;
    let err = result.expect_err("missing parent should fail");
    assert!(
        matches!(err, StoreError::TraceLinkOrphan { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
#[allow(
    clippy::expect_used,
    reason = "test: panics surface broken invariants immediately"
)]
async fn link_validation_rejects_tool_call_id_mismatch() {
    let store = test_store_in_memory().await;
    let session_id = SessionId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("valid");
    let result = store
        .with_tx({
            let session_id = session_id.clone();
            move |tx| {
                let pre = mk_trace_record_with_event(
                    &session_id,
                    "turn-1",
                    0,
                    "01ARZ3NDEKTSV4RRFFQ69G5FAA",
                    "pre_tool",
                    Some("call_abc"),
                    None,
                );
                let post = mk_trace_record_with_event(
                    &session_id,
                    "turn-1",
                    1,
                    "01ARZ3NDEKTSV4RRFFQ69G5FAB",
                    "post_tool",
                    Some("call_other"), // mismatch
                    Some("01ARZ3NDEKTSV4RRFFQ69G5FAA"),
                );
                tx.upsert_trace(&pre)?;
                tx.upsert_trace(&post)?;
                tx.validate_turn_links(&session_id, "turn-1")?;
                Ok(())
            }
        })
        .await;
    let err = result.expect_err("tool_call_id mismatch should fail");
    assert!(
        matches!(err, StoreError::TraceLinkOrphan { ref reason, .. } if reason.contains("tool_call_id")),
        "got {err:?}"
    );
}
