//! End-to-end federation propagation tests (brief §12.a).
//!
//! Two in-memory nodes (issuer + receiver) connected by
//! [`LoopbackTransport`]. Exercises:
//!
//! * **Happy path:** `propose_share` mints + enqueues; `PropagationHandler`
//!   drains the queue and forwards via the transport; `accept_share`
//!   applies the enriched envelope. The receiver projection carries the
//!   share-link provenance and the tier cap.
//! * **Transient retry:** the transport returns `Transient` twice, then
//!   `Ack`. The handler surfaces `Retry`, `Retry`, `Done`. The receiver
//!   applies the first envelope only — subsequent envelopes hit the
//!   dedup gate and return `Duplicate` (single-apply guarantee).
//! * **Permanent failure:** the transport returns `Permanent`. The
//!   handler surfaces `HandlerOutcome::Permanent` (which the scheduler
//!   maps to a dead-letter row).
//!
//! ## Why a manifest is attached between transport and receiver
//!
//! `PropagationHandler::build_envelope` currently emits a propose
//! envelope with `manifest: None` — its docs flag this as a T14 follow-up.
//! The receiver's `accept_share` only applies records when the envelope
//! carries a manifest, so each test enriches the captured envelope with
//! a one-record manifest before calling `accept_share`. Once the handler
//! grows a manifest-builder these helpers can drop the enrichment step;
//! the rest of the e2e shape stays.

use std::sync::Arc;

use cairn_core::contract::federation_transport::FederationTransport;
use cairn_core::domain::MemoryVisibility;
use cairn_core::verbs::accept_share::{AcceptOutcome, AcceptShareRequest, accept_share};
use cairn_core::verbs::propose_share::{ProposeShareRequest, propose_share};
use cairn_test_fixtures::federation::{LoopbackTransport, ProgrammedOutcome};
use cairn_workflows::propagation::handler::PropagationHandler;
use cairn_workflows::propagation::payload::OUTBOUND_SHARE_KIND;
use cairn_workflows::scheduler::handler::{HandlerOutcome, JobHandler};

mod e2e_helpers;
use e2e_helpers::{
    PeerEndpointAlias, attach_manifest_stub, build_issuer, build_receiver, loopback_peer,
};

#[tokio::test]
async fn propose_drain_accept_makes_record_visible_on_receiver() {
    let issuer = build_issuer();
    let receiver = build_receiver();
    let transport = Arc::new(LoopbackTransport::new());
    transport.program([ProgrammedOutcome::Ack]);

    // 1. Issuer mints a share.
    let record_id = issuer.insert_project_record("hello world").await;
    let resp = propose_share(
        ProposeShareRequest {
            record_ids: vec![record_id.clone()],
            grantee: Some(receiver.identity()),
            scope: issuer.scope(),
            grant_tier: MemoryVisibility::Team,
            expires_at: issuer.in_one_hour(),
            peer: Some(PeerEndpointAlias("loopback-node-b".into())),
        },
        &issuer.propose_deps(),
    )
    .await
    .expect("propose ok");

    // 2. Drain the outbound queue via PropagationHandler.
    let payload = issuer.next_pending_payload(OUTBOUND_SHARE_KIND).await;
    let handler = PropagationHandler::outbound_share(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        loopback_peer(),
    );
    let outcome = handler.handle(&payload).await;
    assert_eq!(outcome, HandlerOutcome::Done);

    // 3. Capture the envelope the transport saw, enrich it with the
    //    issuer's record stub (the handler ships `manifest: None` for
    //    now — T14 follow-up), and feed it into the receiver.
    let (envelope, _peer) = transport
        .sent()
        .into_iter()
        .next()
        .expect("transport recorded one envelope");
    assert_eq!(
        envelope.link.as_ref().expect("propose has link").link_id,
        resp.link.link_id,
        "transport envelope link must match the minted link",
    );
    let envelope = attach_manifest_stub(
        envelope,
        &record_id,
        &issuer.scope(),
        MemoryVisibility::Team,
        "hello world",
    );

    let accepted = accept_share(
        AcceptShareRequest {
            envelope: envelope.clone(),
        },
        &receiver.accept_deps(),
    )
    .await
    .expect("accept ok");

    assert_eq!(accepted.outcome, AcceptOutcome::Accepted);
    assert_eq!(accepted.applied_records.len(), 1);

    // 4. Receiver's projection has share-link provenance + tier cap.
    let stored = receiver.fetch_applied(&accepted.applied_records[0]).await;
    assert_eq!(stored.visibility, MemoryVisibility::Team);
    assert!(receiver.has_share_link_provenance(&stored));
}

#[tokio::test]
async fn transient_retries_eventually_succeed_with_single_apply() {
    let issuer = build_issuer();
    let receiver = build_receiver();
    let transport = Arc::new(LoopbackTransport::new());
    transport.program([
        ProgrammedOutcome::Transient("net".into()),
        ProgrammedOutcome::Transient("net".into()),
        ProgrammedOutcome::Ack,
    ]);

    let record_id = issuer.insert_project_record("idempotent").await;
    let _ = propose_share(
        ProposeShareRequest {
            record_ids: vec![record_id.clone()],
            grantee: Some(receiver.identity()),
            scope: issuer.scope(),
            grant_tier: MemoryVisibility::Team,
            expires_at: issuer.in_one_hour(),
            peer: Some(PeerEndpointAlias("loopback-node-b".into())),
        },
        &issuer.propose_deps(),
    )
    .await
    .expect("propose ok");

    let payload = issuer.next_pending_payload(OUTBOUND_SHARE_KIND).await;
    let handler = PropagationHandler::outbound_share(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        loopback_peer(),
    );

    // Three handler invocations mirror the scheduler's retry loop: the
    // payload is replayed verbatim each time, so the handler's outputs
    // depend only on the transport's programmed outcomes.
    let r1 = handler.handle(&payload).await;
    let r2 = handler.handle(&payload).await;
    let r3 = handler.handle(&payload).await;
    assert!(
        matches!(r1, HandlerOutcome::Retry { .. }),
        "first call must be Retry, got {r1:?}",
    );
    assert!(
        matches!(r2, HandlerOutcome::Retry { .. }),
        "second call must be Retry, got {r2:?}",
    );
    assert_eq!(r3, HandlerOutcome::Done);

    // The transport recorded three sends. Receiver applies each — only
    // the first becomes `Accepted`; the rest hit dedup and return
    // `Duplicate`. The receiver's projection still shows ONE record.
    let sent = transport.sent();
    assert_eq!(sent.len(), 3, "transport observed three sends");

    let mut outcomes = Vec::with_capacity(sent.len());
    for (env, _peer) in sent {
        let env = attach_manifest_stub(
            env,
            &record_id,
            &issuer.scope(),
            MemoryVisibility::Team,
            "idempotent",
        );
        let resp = accept_share(
            AcceptShareRequest {
                envelope: env.clone(),
            },
            &receiver.accept_deps(),
        )
        .await
        .expect("accept ok");
        // Mirror the apply into the consent-lookup fake so the next
        // call's dedup probe finds it. In production T12's adapter
        // commits this projection inside the same outbox transaction.
        if resp.outcome == AcceptOutcome::Accepted {
            receiver.record_accept_for_idempotency(&env, resp.applied_records.clone());
        }
        outcomes.push(resp.outcome);
    }

    let accepted = outcomes
        .iter()
        .filter(|o| **o == AcceptOutcome::Accepted)
        .count();
    let duplicates = outcomes
        .iter()
        .filter(|o| **o == AcceptOutcome::Duplicate)
        .count();
    assert_eq!(
        accepted, 1,
        "exactly one envelope applied, got {outcomes:?}"
    );
    assert_eq!(
        duplicates, 2,
        "remaining envelopes hit dedup, got {outcomes:?}"
    );
    assert_eq!(
        receiver.records_with_share_link_provenance(),
        1,
        "single-apply guarantee: receiver projection has one inbound record",
    );
}

#[tokio::test]
async fn permanent_failure_yields_handler_permanent() {
    let issuer = build_issuer();
    let receiver = build_receiver();
    let transport = Arc::new(LoopbackTransport::new());
    transport.program([ProgrammedOutcome::Permanent("rebac denied".into())]);

    let record_id = issuer.insert_project_record("body").await;
    let _ = propose_share(
        ProposeShareRequest {
            record_ids: vec![record_id],
            grantee: Some(receiver.identity()),
            scope: issuer.scope(),
            grant_tier: MemoryVisibility::Team,
            expires_at: issuer.in_one_hour(),
            peer: Some(PeerEndpointAlias("loopback-node-b".into())),
        },
        &issuer.propose_deps(),
    )
    .await
    .expect("propose ok");

    let payload = issuer.next_pending_payload(OUTBOUND_SHARE_KIND).await;
    let handler = PropagationHandler::outbound_share(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        loopback_peer(),
    );
    let outcome = handler.handle(&payload).await;
    match outcome {
        HandlerOutcome::Permanent { reason, .. } => {
            assert!(
                reason.contains("rebac denied"),
                "permanent reason should propagate, got {reason:?}",
            );
        }
        other => panic!("expected Permanent, got {other:?}"),
    }
}
