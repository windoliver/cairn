//! Property test: a single turn's trace events round-trip through the
//! `capture_trace` verb path and come back ordered by
//! `(captured_at, capture_event_id)`, with the summary's
//! `member_event_ids` covering every persisted event in canonical order
//! (issue #77, brief §5.6).
//!
//! Generation strategy
//! -------------------
//! For each case we synthesize 1..=8 events for a single
//! `(session_id, turn_id)` pair:
//!
//! - Index 0 is always a `UserPromptSubmit` (the turn opener).
//! - The last event is always a `Stop` so the verb path emits a summary.
//! - Intermediate events are additional `UserPromptSubmit` hooks with
//!   varied `captured_at` so we exercise non-trivial sort orders.
//!
//! Using only hooks that classify as parent-less trace events keeps the
//! property focused on ordering + summary membership, without dragging the
//! pre/post-tool linkage rules into the generator.

use std::io::Write as _;
use std::path::Path;

use cairn_cli::verbs::capture_trace::run_handler;
use cairn_core::contract::MemoryStore as _;
use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs,
    ChainRole, Identity, PayloadHash, Rfc3339Timestamp, SessionId, SourceFamily,
    trace::summary_record_id,
};
use proptest::prelude::*;
use sha2::{Digest as _, Sha256};

// ── helpers ───────────────────────────────────────────────────────────────────

fn sha256_hex(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

fn write_source(vault: &Path, filename: &str, content: &str) -> String {
    let dir = vault.join("sources").join("hook");
    std::fs::create_dir_all(&dir).expect("create sources/hook dir");
    let abs = dir.join(filename);
    std::fs::write(&abs, content).expect("write source file");
    format!("sources/hook/{filename}")
}

/// Encode `index` (`0..16_777_216`) as a 26-char Crockford base32 ULID
/// string. The high 80 bits are zero-padded; the low bits encode `index`.
/// Crockford base32 alphabet (no `I`, `L`, `O`, `U`).
fn ulid_for(index: u32) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut out = [b'0'; 26];
    let mut n = u128::from(index);
    let mut pos = 25_usize;
    while n > 0 && pos < 26 {
        let d = (n & 0x1F) as usize;
        out[pos] = ALPHABET[d];
        n >>= 5;
        if pos == 0 {
            break;
        }
        pos -= 1;
    }
    String::from_utf8(out.to_vec()).expect("invariant: ascii alphabet")
}

fn rfc3339_at(base_offset_secs: u32) -> String {
    // 2026-05-02T00:00:00Z + offset seconds, capped well within a single day.
    let total = u64::from(base_offset_secs);
    let h = (total / 3600) % 24;
    let m = (total / 60) % 60;
    let s = total % 60;
    format!("2026-05-02T{h:02}:{m:02}:{s:02}Z")
}

#[allow(clippy::too_many_arguments)]
fn make_event(
    event_id: &str,
    hook_name: &str,
    session_id: &str,
    turn_id: &str,
    timestamp: &str,
    payload_ref: &str,
    payload_hash_hex: &str,
) -> CaptureEvent {
    let sensor =
        Identity::parse("snr:local:hook:cc-session:v1").expect("invariant: valid sensor id");
    let hash_str = format!("sha256:{payload_hash_hex}");
    CaptureEvent {
        event_id: CaptureEventId::parse(event_id).expect("invariant: valid ULID"),
        sensor_id: sensor.clone(),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: sensor,
            at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        }],
        refs: Some(CaptureRefs {
            session_id: Some(session_id.to_owned()),
            turn_id: Some(turn_id.to_owned()),
            tool_id: None,
        }),
        payload_hash: PayloadHash::parse(&hash_str).expect("invariant: valid sha256 hash"),
        payload_ref: payload_ref.to_owned(),
        captured_at: Rfc3339Timestamp::parse(timestamp).expect("invariant: valid RFC-3339"),
        payload: CapturePayload::Hook {
            hook_name: hook_name.to_owned(),
            tool_name: None,
        },
        source_family: SourceFamily::Hook,
    }
}

/// Build a vector of `(event_id, captured_at)` pairs for a turn of `n`
/// events. Index 0 is a `UserPromptSubmit`, last is a `Stop`, intermediate
/// rows are additional `UserPromptSubmit`s.
struct GeneratedEvent {
    event_id: String,
    hook_name: &'static str,
    captured_at: String,
}

fn build_events(n: usize, offsets: &[u32], seed: u32) -> Vec<GeneratedEvent> {
    debug_assert_eq!(n, offsets.len(), "offsets length must match n");
    (0..n)
        .map(|i| {
            // ULIDs use a high-byte derived from seed so different cases get
            // distinct ids; low-byte tracks index. `i` is bounded by 8.
            let id_index = (u32::try_from(i).unwrap_or(0)).wrapping_add(seed.wrapping_mul(1024));
            GeneratedEvent {
                event_id: ulid_for(id_index),
                hook_name: if i + 1 == n {
                    "Stop"
                } else {
                    "UserPromptSubmit"
                },
                captured_at: rfc3339_at(offsets[i]),
            }
        })
        .collect()
}

fn write_jsonl(
    vault: &Path,
    jsonl_path: &Path,
    session: &str,
    turn: &str,
    events: &[GeneratedEvent],
) {
    let mut f = std::fs::File::create(jsonl_path).expect("create JSONL file");
    for ev in events {
        let body = format!("body:{}:{}", ev.event_id, ev.captured_at);
        let payload_ref = write_source(vault, &format!("{}.txt", ev.event_id), &body);
        let hash = sha256_hex(&body);
        let event = make_event(
            &ev.event_id,
            ev.hook_name,
            session,
            turn,
            &ev.captured_at,
            &payload_ref,
            &hash,
        );
        let line = serde_json::to_string(&event).expect("serialize event");
        writeln!(f, "{line}").expect("write JSONL line");
    }
}

/// Canonical ordering rule: `(captured_at, capture_event_id)`, both lexical
/// on RFC-3339 strings (matches `Rfc3339Timestamp::cmp_chronological` for
/// our generator since every timestamp uses the same `Z` offset and same
/// fixed-width `HH:MM:SS` form).
fn canonical_order(events: &[GeneratedEvent]) -> Vec<&GeneratedEvent> {
    let mut v: Vec<&GeneratedEvent> = events.iter().collect();
    v.sort_by(|a, b| {
        a.captured_at
            .cmp(&b.captured_at)
            .then_with(|| a.event_id.cmp(&b.event_id))
    });
    v
}

// ── property ──────────────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]
    #[test]
    fn turn_round_trip_reconstructs_canonical_order(
        n in 1_usize..=8,
        seed in 0_u32..1024,
        // We over-generate offsets and truncate to `n` inside the body —
        // proptest can't easily make a vector whose length is a previously
        // generated value. Range chosen wide enough to allow ties.
        offsets_raw in proptest::collection::vec(0_u32..3600, 8),
    ) {
        let mut offsets = offsets_raw.clone();
        offsets.truncate(n);
        // n==1: lone event must be a Stop (no UserPromptSubmit before it,
        // but the turn-summary path requires the closer). Skip n==1 cases
        // that would have only a Stop with no opener — actually a single
        // Stop is acceptable to the verb (it summarizes the lone event).
        let events = build_events(n, &offsets, seed);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime");

        rt.block_on(async {
            let vault = tempfile::tempdir().expect("tempdir");
            let store = cairn_store_sqlite::open_in_memory()
                .await
                .expect("open in-memory store");
            let jsonl_path = vault.path().join("trace.jsonl");

            let session = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
            let turn = "turn-prop";

            write_jsonl(vault.path(), &jsonl_path, session, turn, &events);

            let resp = run_handler(&store, vault.path(), &jsonl_path)
                .await
                .expect("run_handler should succeed");
            prop_assert!(
                resp.failed_turns.is_empty(),
                "expected no failures, got: {:?}",
                resp.failed_turns
            );

            // Read back all non-summary events and verify the order matches
            // the canonical sort of the generated events.
            let session_id = SessionId::parse(session).expect("valid session_id");
            let expected_order: Vec<String> = canonical_order(&events)
                .iter()
                .map(|e| e.event_id.clone())
                .collect();

            let observed_order: Vec<String> = store
                .with_tx({
                    let session_id = session_id.clone();
                    let turn = turn.to_owned();
                    move |tx| {
                        let rows = tx.list_trace_events(&session_id, &turn)?;
                        let ids: Vec<String> = rows
                            .iter()
                            .map(|r| {
                                r.extra_frontmatter["trace"]["capture_event_id"]
                                    .as_str()
                                    .expect("trace.capture_event_id is a string")
                                    .to_owned()
                            })
                            .collect();
                        Ok(ids)
                    }
                })
                .await
                .expect("with_tx read should succeed");

            prop_assert_eq!(
                observed_order.clone(),
                expected_order.clone(),
                "trace events must be persisted in canonical (captured_at, capture_event_id) order"
            );

            // The turn always ends with Stop, so a summary must exist with
            // member_event_ids equal to the canonical order of all event ids.
            let summary_id = summary_record_id(&session_id, turn);
            let summary = store
                .get(&summary_id)
                .await
                .expect("get summary record")
                .expect("summary record must exist after Stop");
            let members = summary
                .extra_frontmatter
                .get("trace")
                .and_then(|t| t.get("member_event_ids"))
                .and_then(|m| m.as_array())
                .expect("summary must carry trace.member_event_ids array")
                .iter()
                .map(|v| {
                    v.as_str()
                        .expect("member_event_ids entries are strings")
                        .to_owned()
                })
                .collect::<Vec<String>>();

            prop_assert_eq!(
                members,
                expected_order,
                "summary.member_event_ids must equal the canonical order of all turn events"
            );

            Ok(())
        })?;
    }
}
