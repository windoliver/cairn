//! End-to-end federation propagation tests (brief §12.a).
//!
//! Two in-memory nodes (issuer + receiver) connected by
//! [`LoopbackTransport`]. Exercises:
//!
//! * **Happy path:** `propose_share` mints + enqueues; `PropagationHandler`
//!   drains the queue and forwards via the transport; `accept_share`
//!   applies the envelope (which now carries a populated manifest). The
//!   receiver projection carries the share-link provenance and the tier cap.
//! * **Transient retry:** the transport returns `Transient` twice, then
//!   `Ack`. The handler surfaces `Retry`, `Retry`, `Done`. The receiver
//!   applies the first envelope only — subsequent envelopes hit the
//!   dedup gate and return `Duplicate` (single-apply guarantee).
//! * **Permanent failure:** the transport returns `Permanent`. The
//!   handler surfaces `HandlerOutcome::Permanent` (which the scheduler
//!   maps to a dead-letter row).

use std::sync::Arc;

use cairn_core::contract::federation_transport::FederationTransport;
use cairn_core::domain::{ConsentKind, MemoryVisibility};
use cairn_core::verbs::accept_share::{AcceptOutcome, AcceptShareRequest, accept_share};
use cairn_core::verbs::propose_share::{ProposeShareRequest, propose_share};
use cairn_core::verbs::revoke_share::{RevokeShareRequest, revoke_share};
use cairn_test_fixtures::federation::{LoopbackTransport, ProgrammedOutcome};
use cairn_workflows::propagation::handler::PropagationHandler;
use cairn_workflows::propagation::payload::{OUTBOUND_REVOKE_KIND, OUTBOUND_SHARE_KIND};
use cairn_workflows::scheduler::handler::{HandlerOutcome, JobHandler};

mod e2e_helpers;
use e2e_helpers::{PeerEndpointAlias, build_issuer, build_receiver, loopback_peer};

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

    // 3. Capture the envelope the transport saw — the handler now
    //    populates the manifest from the job payload, so no enrichment
    //    step is needed.
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
    assert!(
        envelope.manifest.is_some(),
        "handler must populate the manifest",
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
#[allow(
    clippy::too_many_lines,
    reason = "linear E2E narrative: propose → drain → accept → revoke → \
              drain → accept revoke → assert audit trail on both ends; \
              splitting hurts the readability of the full handshake."
)]
async fn revoke_propagates_and_tombstones_receiver_projection() {
    // End-to-end revoke (brief §12.a, §14):
    //   1. issuer proposes → drain → receiver accepts
    //   2. issuer revokes → drain → receiver applies the revoke envelope
    //
    // We verify:
    //   * `revoke_share` enqueues an `OUTBOUND_REVOKE_KIND` job.
    //   * The propagation handler drains it via the transport (Ack).
    //   * The receiver's `accept_share` happily applies the inbound
    //     revoke envelope (`AcceptOutcome::Accepted`, empty
    //     applied_records — T8's revoke path commits a body-free
    //     consent row but does not yet materialise the per-record
    //     tombstone list; see the test note below).
    //   * The receiver's consent journal records BOTH the original
    //     `FederationAccept` and the inbound `FederationRevoke` row
    //     under the same share-link id.
    //
    // Tombstone-side note: T8's `accept_revoke` passes an empty
    // `tombstone_ids` slice to the outbox — the projection lookup
    // that would populate it is the unfinished half of the revoke
    // path. The asserts below intentionally do NOT claim the
    // receiver's projection is tombstoned per-record; they only
    // claim the audit-trail side of the contract holds. Once T8
    // grows the per-record tombstone fan-out, this test should be
    // tightened to assert `receiver.try_fetch(id).is_none()` for
    // every previously-applied record.

    let issuer = build_issuer();
    let receiver = build_receiver();
    let transport = Arc::new(LoopbackTransport::new());
    // Two acks: one for the propose envelope, one for the revoke.
    transport.program([ProgrammedOutcome::Ack, ProgrammedOutcome::Ack]);

    // 1. Issuer proposes; drain via PropagationHandler; receiver
    //    accepts after manifest enrichment (same shape as
    //    `propose_drain_accept_makes_record_visible_on_receiver`).
    let record_id = issuer.insert_project_record("revocable body").await;
    let propose = propose_share(
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

    let share_payload = issuer.next_pending_payload(OUTBOUND_SHARE_KIND).await;
    let share_handler = PropagationHandler::outbound_share(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        loopback_peer(),
    );
    assert_eq!(
        share_handler.handle(&share_payload).await,
        HandlerOutcome::Done
    );

    let (propose_env, _) = transport
        .sent()
        .into_iter()
        .next()
        .expect("transport recorded the propose send");
    let accepted = accept_share(
        AcceptShareRequest {
            envelope: propose_env.clone(),
        },
        &receiver.accept_deps(),
    )
    .await
    .expect("accept ok");
    assert_eq!(accepted.outcome, AcceptOutcome::Accepted);
    assert_eq!(
        accepted.applied_records.len(),
        1,
        "receiver applied exactly one inbound record",
    );
    // Mirror the apply into the consent-lookup projection so the
    // revoke path's `find_federation_accept` lookup finds the
    // original record ids. In production T12's adapter materialises
    // this inside the same outbox transaction.
    receiver.record_accept_for_idempotency(&propose_env, accepted.applied_records.clone());

    // 2. Issuer revokes the link. The revoke verb appends a
    //    FederationRevoke consent row and enqueues an
    //    OUTBOUND_REVOKE_KIND propagation job — both under the
    //    operation_id minted here.
    let revoke = revoke_share(
        RevokeShareRequest {
            link_id: propose.link.link_id.clone(),
        },
        &issuer.revoke_deps(),
    )
    .await
    .expect("revoke ok");
    assert!(
        !revoke.operation_id.is_empty(),
        "revoke must mint a non-empty operation id",
    );

    // 3. Drain the revoke job through the outbound_revoke handler.
    //    The transport's second programmed Ack covers this send.
    let revoke_payload = issuer.next_pending_payload(OUTBOUND_REVOKE_KIND).await;
    let revoke_handler = PropagationHandler::outbound_revoke(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        loopback_peer(),
    );
    assert_eq!(
        revoke_handler.handle(&revoke_payload).await,
        HandlerOutcome::Done,
    );

    // 4. The transport has now seen two envelopes (propose, revoke).
    //    Feed the revoke envelope into the receiver's accept_share.
    let sent = transport.sent();
    assert_eq!(sent.len(), 2, "transport observed propose + revoke");
    let (revoke_env, _) = sent
        .into_iter()
        .nth(1)
        .expect("transport recorded the revoke send");
    let revoke_resp = accept_share(
        AcceptShareRequest {
            envelope: revoke_env,
        },
        &receiver.accept_deps(),
    )
    .await
    .expect("accept revoke ok");
    assert_eq!(
        revoke_resp.outcome,
        AcceptOutcome::Accepted,
        "receiver applies the inbound revoke envelope",
    );
    // The revoke path now tombstones the receiver's projected records.
    // `applied_records` carries the tombstoned record IDs.
    assert_eq!(
        revoke_resp.applied_records, accepted.applied_records,
        "revoke must tombstone every record the original accept projected",
    );
    // Every previously-applied record should be gone from the store.
    for id in &revoke_resp.applied_records {
        assert!(
            receiver.try_fetch(id).is_none(),
            "record {id} should be tombstoned after revoke",
        );
    }

    // 5. The receiver's consent journal carries both the original
    //    FederationAccept and the inbound FederationRevoke row,
    //    keyed off the same share-link id.
    let kinds = receiver.consent_event_kinds_for_link(&propose.link.link_id);
    assert!(
        kinds.contains(&ConsentKind::FederationAccept),
        "receiver journal should record the accept; got {kinds:?}",
    );
    assert!(
        kinds.contains(&ConsentKind::FederationRevoke),
        "receiver journal should record the revoke; got {kinds:?}",
    );

    // 6. Issuer's consent journal records the FederationGrant and
    //    FederationRevoke rows (issuer side) so the audit trail is
    //    complete on both ends of the wire.
    let issuer_kinds = issuer.consent_event_kinds_for_link(&propose.link.link_id);
    assert!(
        issuer_kinds.contains(&ConsentKind::FederationGrant),
        "issuer journal should record the grant; got {issuer_kinds:?}",
    );
    assert!(
        issuer_kinds.contains(&ConsentKind::FederationRevoke),
        "issuer journal should record the revoke; got {issuer_kinds:?}",
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
