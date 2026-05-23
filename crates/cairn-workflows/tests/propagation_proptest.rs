//! Property tests for federation invariants (T16, brief §12.a).
//!
//! Covers:
//!
//! 1. **Accept idempotency** — applying the same envelope N times yields
//!    exactly one `Accepted` and N-1 `Duplicate` outcomes, with a single
//!    record on the receiver's projection.
//! 2. **Retry safety** — interleaved `(Transient*, Ack)` transport outcomes
//!    yield exactly one `Accepted` across all captured envelopes replayed to
//!    the receiver.
//! 3. **Revoke ordering (propose → accept → revoke)** — accept succeeds;
//!    revoke audit-row exists in the receiver's journal.
//! 4. **Revoke ordering (propose → revoke → accept)** — the late-arriving
//!    propose envelope is rejected with `FederationError::Revoked`.

#![allow(
    clippy::too_many_lines,
    reason = "property test blocks are inherently long linear narratives"
)]

mod e2e_helpers;
use e2e_helpers::{
    PeerEndpointAlias, attach_manifest_stub, build_issuer, build_receiver, loopback_peer,
};

use std::sync::Arc;

use cairn_core::contract::federation_transport::FederationTransport;
use cairn_core::domain::{ConsentKind, MemoryVisibility};
use cairn_core::error::federation::FederationError;
use cairn_core::verbs::accept_share::{AcceptOutcome, AcceptShareRequest, accept_share};
use cairn_core::verbs::propose_share::{ProposeShareRequest, propose_share};
use cairn_core::verbs::revoke_share::{RevokeShareRequest, revoke_share};
use cairn_test_fixtures::federation::{LoopbackTransport, ProgrammedOutcome};
use cairn_workflows::propagation::handler::PropagationHandler;
use cairn_workflows::propagation::payload::{OUTBOUND_REVOKE_KIND, OUTBOUND_SHARE_KIND};
use cairn_workflows::scheduler::handler::JobHandler;
use proptest::prelude::*;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("invariant: tokio runtime must init")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, .. ProptestConfig::default() })]

    /// Applying the same propose envelope N times yields exactly one
    /// `Accepted` and N-1 `Duplicate` outcomes regardless of replay count.
    ///
    /// The transport is programmed with N `Ack` outcomes so the handler sends
    /// the same envelope N times. Each captured send is independently fed
    /// into `accept_share` on the receiver. After the first hit the
    /// idempotency projection is seeded manually (mirroring what T12's
    /// adapter commits in-band) so subsequent calls return `Duplicate`.
    #[test]
    fn accept_share_is_idempotent_under_arbitrary_replay(replays in 1usize..=5) {
        rt().block_on(async {
            let issuer = build_issuer();
            let receiver = build_receiver();
            let transport = Arc::new(LoopbackTransport::new());
            transport.program(std::iter::repeat_n(ProgrammedOutcome::Ack, replays).collect::<Vec<_>>());

            let body = format!("idempotent-body-replay-{replays}");
            let record_id = issuer.insert_project_record(&body).await;
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
            .expect("invariant: propose_share must succeed");

            let payload = issuer.next_pending_payload(OUTBOUND_SHARE_KIND).await;
            let handler = PropagationHandler::outbound_share(
                Arc::clone(&transport) as Arc<dyn FederationTransport>,
                loopback_peer(),
            );
            for _ in 0..replays {
                let _ = handler.handle(&payload).await;
            }

            // Each captured send is replayed to the receiver independently.
            // After the first Accepted response we seed the idempotency
            // projection so the dedup gate fires on subsequent calls.
            let sent = transport.sent();
            prop_assert_eq!(sent.len(), replays, "transport must record exactly N sends");

            let mut outcomes = Vec::with_capacity(sent.len());
            for (env, _peer) in sent {
                let enriched = attach_manifest_stub(
                    env,
                    &record_id,
                    &issuer.scope(),
                    MemoryVisibility::Team,
                    &body,
                );
                let resp = accept_share(
                    AcceptShareRequest { envelope: enriched.clone() },
                    &receiver.accept_deps(),
                )
                .await
                .expect("accept_share must not error on replay");

                if resp.outcome == AcceptOutcome::Accepted {
                    receiver.record_accept_for_idempotency(&enriched, resp.applied_records.clone());
                }
                outcomes.push(resp.outcome);
            }

            let accepted_count = outcomes.iter().filter(|o| **o == AcceptOutcome::Accepted).count();
            let duplicate_count = outcomes.iter().filter(|o| **o == AcceptOutcome::Duplicate).count();
            prop_assert_eq!(accepted_count, 1, "exactly one Accepted regardless of replay count");
            prop_assert_eq!(duplicate_count, replays - 1, "remaining envelopes must be Duplicate");
            // Single-apply guarantee: only one inbound record on the projection.
            prop_assert_eq!(
                receiver.records_with_share_link_provenance(),
                1,
                "receiver projection must hold exactly one inbound record",
            );
            Ok(())
        }).unwrap();
    }

    /// Interleaved `(Transient*, Ack)` sequences yield exactly one `Accepted`
    /// across all captured envelopes.
    ///
    /// The transport is programmed with `transient_count` transient outcomes
    /// followed by one `Ack`. The handler is invoked for each programmed
    /// outcome; transient calls surface `HandlerOutcome::Retry` but still
    /// forward the envelope. Every captured send is then applied on the
    /// receiver — only the first should be `Accepted`.
    #[test]
    fn retry_interleavings_yield_single_apply(transient_count in 1usize..=5) {
        rt().block_on(async {
            let issuer = build_issuer();
            let receiver = build_receiver();
            let transport = Arc::new(LoopbackTransport::new());

            let mut programmed: Vec<ProgrammedOutcome> =
                std::iter::repeat_n(ProgrammedOutcome::Transient("net".into()), transient_count)
                    .collect();
            programmed.push(ProgrammedOutcome::Ack);
            transport.program(programmed);

            let body = format!("retry-body-{transient_count}");
            let record_id = issuer.insert_project_record(&body).await;
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
            .expect("invariant: propose_share must succeed");

            let payload = issuer.next_pending_payload(OUTBOUND_SHARE_KIND).await;
            let handler = PropagationHandler::outbound_share(
                Arc::clone(&transport) as Arc<dyn FederationTransport>,
                loopback_peer(),
            );

            // One handler call per programmed outcome (transients + final ack).
            for _ in 0..=transient_count {
                let _ = handler.handle(&payload).await;
            }

            // Every captured envelope (including transient-path sends) is
            // applied on the receiver. The dedup gate fires after the first
            // `Accepted` so only one record is ever written.
            let sent = transport.sent();
            prop_assert_eq!(
                sent.len(),
                transient_count + 1,
                "transport must record one send per invocation"
            );

            let mut accepted = 0usize;
            for (env, _peer) in sent {
                let enriched = attach_manifest_stub(
                    env,
                    &record_id,
                    &issuer.scope(),
                    MemoryVisibility::Team,
                    &body,
                );
                let resp = accept_share(
                    AcceptShareRequest { envelope: enriched.clone() },
                    &receiver.accept_deps(),
                )
                .await
                .expect("accept_share must not error");

                if resp.outcome == AcceptOutcome::Accepted {
                    receiver.record_accept_for_idempotency(&enriched, resp.applied_records.clone());
                    accepted += 1;
                }
            }

            prop_assert_eq!(accepted, 1, "exactly one record must be applied across all replays");
            prop_assert_eq!(
                receiver.records_with_share_link_provenance(),
                1,
                "single-apply guarantee: receiver projection holds one inbound record",
            );
            Ok(())
        }).unwrap();
    }
}

/// **Propose → accept → revoke** ordering.
///
/// Verifies that:
/// * `accept_share` succeeds with `AcceptOutcome::Accepted` when the
///   revoke arrives *after* the accept.
/// * The receiver's consent journal records both a `FederationAccept` and
///   a `FederationRevoke` row under the same share-link id.
#[tokio::test]
async fn propose_accept_revoke_ordering() {
    let issuer = build_issuer();
    let receiver = build_receiver();
    let transport = Arc::new(LoopbackTransport::new());
    // Two acks: one for the propose envelope, one for the revoke.
    transport.program([ProgrammedOutcome::Ack, ProgrammedOutcome::Ack]);

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

    // Drain + accept the propose envelope first.
    let share_payload = issuer.next_pending_payload(OUTBOUND_SHARE_KIND).await;
    let share_handler = PropagationHandler::outbound_share(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        loopback_peer(),
    );
    let _ = share_handler.handle(&share_payload).await;

    let (propose_env, _) = transport
        .sent()
        .into_iter()
        .next()
        .expect("transport recorded the propose send");
    let propose_env = attach_manifest_stub(
        propose_env,
        &record_id,
        &issuer.scope(),
        MemoryVisibility::Team,
        "revocable body",
    );
    let accepted = accept_share(
        AcceptShareRequest { envelope: propose_env },
        &receiver.accept_deps(),
    )
    .await
    .expect("accept ok");
    assert_eq!(
        accepted.outcome,
        AcceptOutcome::Accepted,
        "propose envelope must be Accepted before revoke",
    );

    // Issuer now revokes → drain → receiver applies the revoke envelope.
    let _ = revoke_share(
        RevokeShareRequest { link_id: propose.link.link_id.clone() },
        &issuer.revoke_deps(),
    )
    .await
    .expect("revoke ok");

    let revoke_payload = issuer.next_pending_payload(OUTBOUND_REVOKE_KIND).await;
    let revoke_handler = PropagationHandler::outbound_revoke(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        loopback_peer(),
    );
    let _ = revoke_handler.handle(&revoke_payload).await;

    let (revoke_env, _) = transport
        .sent()
        .into_iter()
        .nth(1)
        .expect("transport recorded the revoke send");
    let revoke_resp = accept_share(
        AcceptShareRequest { envelope: revoke_env },
        &receiver.accept_deps(),
    )
    .await
    .expect("accept revoke ok");
    assert_eq!(
        revoke_resp.outcome,
        AcceptOutcome::Accepted,
        "revoke envelope must be accepted by the receiver",
    );

    // Both sides of the audit trail are intact.
    let kinds = receiver.consent_event_kinds_for_link(&propose.link.link_id);
    assert!(
        kinds.contains(&ConsentKind::FederationAccept),
        "receiver journal must record FederationAccept; got {kinds:?}",
    );
    assert!(
        kinds.contains(&ConsentKind::FederationRevoke),
        "receiver journal must record FederationRevoke; got {kinds:?}",
    );
}

/// **Propose → revoke → accept** ordering.
///
/// Verifies that a propose envelope arriving *after* the receiver has
/// already applied the revocation is rejected with
/// [`FederationError::Revoked`]. The revoke tombstone is checked before
/// the upsert so the record is never written.
#[tokio::test]
async fn propose_revoke_accept_rejects_after_revocation() {
    let issuer = build_issuer();
    let receiver = build_receiver();
    let transport = Arc::new(LoopbackTransport::new());
    // Two acks: one for the propose envelope, one for the revoke.
    transport.program([ProgrammedOutcome::Ack, ProgrammedOutcome::Ack]);

    let record_id = issuer.insert_project_record("to-be-revoked").await;
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

    // Drain the propose envelope via the handler, capture it but do NOT
    // apply it to the receiver yet.
    let share_payload = issuer.next_pending_payload(OUTBOUND_SHARE_KIND).await;
    let share_handler = PropagationHandler::outbound_share(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        loopback_peer(),
    );
    let _ = share_handler.handle(&share_payload).await;
    let (propose_env, _) = transport
        .sent()
        .into_iter()
        .next()
        .expect("transport recorded the propose send");

    // Issuer revokes *before* the receiver ever applies the propose.
    // Drain + apply the revoke envelope so the receiver's projection
    // marks the link as revoked.
    let _ = revoke_share(
        RevokeShareRequest { link_id: propose.link.link_id.clone() },
        &issuer.revoke_deps(),
    )
    .await
    .expect("revoke ok");

    let revoke_payload = issuer.next_pending_payload(OUTBOUND_REVOKE_KIND).await;
    let revoke_handler = PropagationHandler::outbound_revoke(
        Arc::clone(&transport) as Arc<dyn FederationTransport>,
        loopback_peer(),
    );
    let _ = revoke_handler.handle(&revoke_payload).await;
    let (revoke_env, _) = transport
        .sent()
        .into_iter()
        .nth(1)
        .expect("transport recorded the revoke send");
    let _ = accept_share(
        AcceptShareRequest { envelope: revoke_env },
        &receiver.accept_deps(),
    )
    .await
    .expect("apply revoke ok");

    // Now the originally-captured propose envelope finally arrives at the
    // receiver. Because `is_link_revoked` returns true the verb must
    // short-circuit with `FederationError::Revoked` before touching the
    // store.
    let propose_env = attach_manifest_stub(
        propose_env,
        &record_id,
        &issuer.scope(),
        MemoryVisibility::Team,
        "to-be-revoked",
    );
    let err = accept_share(
        AcceptShareRequest { envelope: propose_env },
        &receiver.accept_deps(),
    )
    .await
    .expect_err("late propose must be rejected with Revoked");
    assert_eq!(
        err,
        FederationError::Revoked,
        "receiver must return Revoked when link was already revoked",
    );

    // No inbound records were ever written to the receiver's projection.
    assert_eq!(
        receiver.records_with_share_link_provenance(),
        0,
        "no inbound records must be present when propose arrived after revoke",
    );
}
