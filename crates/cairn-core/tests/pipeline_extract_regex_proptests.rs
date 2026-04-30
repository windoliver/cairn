//! Property tests for `RegexExtractor`. Spec §10.3.

#![allow(missing_docs)]

use proptest::prelude::*;

use cairn_core::domain::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload, ChainRole,
    Identity, PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use cairn_core::pipeline::extract::{
    BodyResolution, ExtractInput, ExtractOutput, ExtractorWorker, RegexExtractor, ResolvedBody,
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

proptest! {
    #[test]
    fn random_text_no_panic(s in "[\\PC]{0,4096}") {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let event = fixture_event();
            let body = ResolvedBody::from_user_ingest(&s, &event.payload, UserIngestPayloadKind::Cli)
                .expect("matching variant");
            let input = ExtractInput {
                event: &event,
                body: BodyResolution::Resolved(body),
            };
            let extractor = RegexExtractor::builtin();
            let res = extractor.extract(&input).await;
            prop_assert!(res.is_ok());
            // `max_drafts` bounds USER-rule outputs only — built-in
            // triggers are intentionally uncapped (see
            // dispatch::tests::builtin_outputs_are_uncapped). The
            // builtin RuleSet has no user rules, so user-rule output
            // count must be 0 here.
            if let Ok(r) = res {
                let user_outputs = r
                    .outputs
                    .iter()
                    .filter(|o| match o {
                        ExtractOutput::Draft(d) => d
                            .trigger_id
                            .as_deref()
                            .is_some_and(|id| id.starts_with("user.")),
                        ExtractOutput::Forget(i) => i.trigger_id.starts_with("user."),
                    })
                    .count();
                prop_assert!(user_outputs <= extractor.budget().max_drafts as usize);
            }
            Ok::<(), TestCaseError>(())
        }).unwrap();
    }

    #[test]
    fn output_serde_round_trip(
        body in "remember that .{0,80}",
    ) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let event = fixture_event();
            let rb = ResolvedBody::from_user_ingest(&body, &event.payload, UserIngestPayloadKind::Cli)
                .expect("matching variant");
            let input = ExtractInput {
                event: &event,
                body: BodyResolution::Resolved(rb),
            };
            let extractor = RegexExtractor::builtin();
            let res = extractor.extract(&input).await.unwrap();
            for out in &res.outputs {
                let json = serde_json::to_string(out).unwrap();
                let back: ExtractOutput = serde_json::from_str(&json).unwrap();
                prop_assert_eq!(&back, out);
            }
            Ok::<(), TestCaseError>(())
        }).unwrap();
    }
}
