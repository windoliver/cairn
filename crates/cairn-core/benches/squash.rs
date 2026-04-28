//! Criterion micro-bench for `cairn_core::pipeline::squash`.
//!
//! Measures the cost of the full squash pipeline on a synthetic 50 KB payload
//! of repeating ASCII bytes — a representative lower bound for real terminal
//! output.

// criterion_group!/criterion_main! expand to public functions with no doc
// comments; suppress the workspace `missing_docs` lint for this file only.
#![allow(missing_docs)]

use cairn_core::{
    domain::{
        actor_chain::{ActorChainEntry, ChainRole},
        capture::{
            CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, CaptureRefs, PayloadHash,
            SourceFamily,
        },
        identity::Identity,
        timestamp::Rfc3339Timestamp,
    },
    pipeline::squash::{SquashConfig, TerminalContext, UnstructuredTextBytes, squash},
};
use criterion::{Criterion, criterion_group, criterion_main};
use sha2::{Digest, Sha256};

// ── helper (mirrors squash_fixtures.rs; criterion benches cannot import from tests/) ──

fn payload_hash_of(bytes: &[u8]) -> PayloadHash {
    let digest = Sha256::digest(bytes);
    PayloadHash::parse(format!("sha256:{digest:x}"))
        .expect("invariant: sha256 hex is always a valid PayloadHash")
}

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-04-27T00:00:00Z").expect("invariant: valid RFC-3339 literal")
}

fn terminal_event(payload_bytes: &[u8]) -> CaptureEvent {
    CaptureEvent {
        event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .expect("invariant: valid ULID literal"),
        sensor_id: Identity::parse("snr:local:terminal:cli:v1")
            .expect("invariant: valid sensor identity"),
        capture_mode: CaptureMode::Auto,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: Identity::parse("snr:local:terminal:cli:v1")
                .expect("invariant: valid sensor identity"),
            at: ts(),
        }],
        refs: Some(CaptureRefs {
            session_id: Some("sess".into()),
            turn_id: Some("turn".into()),
            tool_id: None,
        }),
        payload_hash: payload_hash_of(payload_bytes),
        payload_ref: "sources/terminal/01ARZ3NDEKTSV4RRFFQ69G5FAV.txt".into(),
        captured_at: ts(),
        payload: CapturePayload::Terminal {
            command: "echo hi".into(),
            exit_code: Some(0),
        },
        source_family: SourceFamily::Terminal,
    }
}

// ── benchmark ────────────────────────────────────────────────────────────────

fn bench_squash(c: &mut Criterion) {
    let raw: Vec<u8> = (0..50_000_u32)
        .map(|i| b'a' + u8::try_from(i % 26).unwrap())
        .collect();
    let cfg = SquashConfig::default();
    let evt = terminal_event(&raw);

    c.bench_function("squash 50KB", |b| {
        b.iter(|| {
            let w = UnstructuredTextBytes::try_from_terminal_event_unstable(
                &evt,
                &raw,
                TerminalContext::InteractiveTty,
            )
            .unwrap();
            squash(w, &cfg)
        });
    });
}

criterion_group!(benches, bench_squash);
criterion_main!(benches);
