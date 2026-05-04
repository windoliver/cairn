//! p99 < 2 ms latency assertion. Marked `#[ignore]` by default; runs
//! locally with `cargo nextest run -- --include-ignored`. Replace with
//! a `criterion` benchmark in a follow-up issue.

#![allow(missing_docs)]

use std::time::Instant;

use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, ChainRole,
    Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use cairn_core::pipeline::extract::{
    BodyResolution, ExtractInput, ExtractorWorker, RegexExtractor, ResolvedBody,
    UserIngestPayloadKind,
};

fn ts() -> Rfc3339Timestamp {
    Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid")
}

fn fixture_event() -> CaptureEvent {
    CaptureEvent {
        event_id: CaptureEventId::parse("01ARZ3NDEKTSV4RRFFQ69G5FB0").expect("valid ULID"),
        sensor_id: Identity::parse("snr:local:cli:default:v1").expect("valid"),
        capture_mode: CaptureMode::Explicit,
        actor_chain: vec![
            ActorChainEntry {
                role: ChainRole::Delegator,
                identity: Identity::parse("agt:claude-code:opus-4-7:main:v1").expect("valid"),
                at: ts(),
            },
            ActorChainEntry {
                role: ChainRole::Author,
                identity: Identity::parse("hmn:tafeng").expect("valid"),
                at: ts(),
            },
        ],
        refs: None,
        payload_hash: PayloadHash::parse(format!("sha256:{}", "ab".repeat(32))).expect("valid"),
        payload_ref: "sources/cli/01ARZ3NDEKTSV4RRFFQ69G5FB0.txt".into(),
        captured_at: ts(),
        payload: CapturePayload::Cli {
            kind_hint: "user".into(),
        },
        source_family: SourceFamily::Cli,
    }
}

#[ignore = "perf-sensitive; run locally with --include-ignored"]
#[tokio::test]
async fn p99_under_2ms_on_mixed_fixture() {
    let extractor = RegexExtractor::builtin();
    let event = fixture_event();
    let bodies: Vec<String> = (0..10_000)
        .map(|i| match i % 3 {
            0 => format!("remember that I like option {}", i % 10),
            1 => format!("hello world batch {i}"),
            _ => format!("forget what I said about thing {i}"),
        })
        .collect();

    for body in bodies.iter().take(100) {
        let rb = ResolvedBody::from_user_ingest(body, &event.payload, UserIngestPayloadKind::Cli)
            .expect("matching variant");
        let _ = extractor
            .extract(&ExtractInput {
                event: &event,
                body: BodyResolution::Resolved(rb),
                eligible_spans: None,
            })
            .await;
    }

    let mut samples = Vec::with_capacity(bodies.len());
    for body in &bodies {
        let rb = ResolvedBody::from_user_ingest(body, &event.payload, UserIngestPayloadKind::Cli)
            .expect("matching variant");
        let start = Instant::now();
        let _ = extractor
            .extract(&ExtractInput {
                event: &event,
                body: BodyResolution::Resolved(rb),
                eligible_spans: None,
            })
            .await
            .expect("ok");
        samples.push(start.elapsed());
    }
    samples.sort();
    let p99 = samples[(samples.len() * 99) / 100];
    assert!(
        p99.as_millis() < 2,
        "p99 was {} ms, expected < 2 ms",
        p99.as_millis()
    );
}
