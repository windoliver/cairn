//! Tests for the `accept_share` verb (brief §12.a).

use cairn_core::domain::MemoryVisibility;
use cairn_core::domain::consent::ConsentKind;
use cairn_core::error::federation::FederationError;
use cairn_core::verbs::accept_share::{AcceptOutcome, AcceptShareRequest, accept_share};

mod common;
use common::ReceiverCtx;

#[tokio::test]
async fn accept_applies_inbound_records_with_provenance_and_tier_cap() {
    let ctx = ReceiverCtx::receiver_with_federation_ready();
    let envelope = ctx
        .build_signed_propose_envelope_with_one_record(MemoryVisibility::Team)
        .await;
    let resp = accept_share(
        AcceptShareRequest {
            envelope: envelope.clone(),
        },
        &ctx.deps(),
    )
    .await
    .expect("ok");
    assert_eq!(resp.outcome, AcceptOutcome::Accepted);
    assert_eq!(resp.applied_records.len(), 1);
    let stored = ctx.fetch(&resp.applied_records[0]).await;
    assert_eq!(stored.visibility, MemoryVisibility::Team);
    assert!(ctx.has_share_link_provenance(&stored));
    assert!(
        ctx.has_consent_event_for_envelope(&envelope, ConsentKind::FederationAccept)
            .await
    );
}

#[tokio::test]
async fn accept_is_idempotent() {
    let ctx = ReceiverCtx::receiver_with_federation_ready();
    let envelope = ctx
        .build_signed_propose_envelope_with_one_record(MemoryVisibility::Team)
        .await;
    let first = accept_share(
        AcceptShareRequest {
            envelope: envelope.clone(),
        },
        &ctx.deps(),
    )
    .await
    .unwrap();
    // Teach the consent-lookup fake about the first apply so the second
    // call sees the dedup hit. The real adapter (T12) materialises this
    // from the consent_timeline projection inside the same transaction
    // as the apply.
    ctx.record_accept_for_idempotency(&envelope, first.applied_records.clone());

    let again = accept_share(
        AcceptShareRequest {
            envelope: envelope.clone(),
        },
        &ctx.deps(),
    )
    .await
    .unwrap();
    assert_eq!(first.outcome, AcceptOutcome::Accepted);
    assert_eq!(again.outcome, AcceptOutcome::Duplicate);
    assert_eq!(first.applied_records, again.applied_records);
    assert_eq!(
        ctx.count_consent_events_for_envelope(&envelope, ConsentKind::FederationAccept)
            .await,
        1,
        "duplicate accept must not append a second consent row",
    );
}

#[tokio::test]
async fn accept_rejects_expired_link() {
    let ctx = ReceiverCtx::receiver_with_federation_ready();
    let envelope = ctx.build_expired_propose_envelope().await;
    let err = accept_share(AcceptShareRequest { envelope }, &ctx.deps())
        .await
        .unwrap_err();
    assert_eq!(err, FederationError::Expired);
}

#[tokio::test]
async fn accept_rejects_bad_signature() {
    let ctx = ReceiverCtx::receiver_with_federation_ready();
    let envelope = ctx.build_tampered_propose_envelope().await;
    let err = accept_share(AcceptShareRequest { envelope }, &ctx.deps())
        .await
        .unwrap_err();
    assert_eq!(err, FederationError::BadSignature);
}

#[tokio::test]
async fn accept_rejects_when_receiver_has_no_write_rebac_relation() {
    let ctx = ReceiverCtx::receiver_without_write_relation();
    let envelope = ctx
        .build_signed_propose_envelope_with_one_record(MemoryVisibility::Team)
        .await;
    let err = accept_share(AcceptShareRequest { envelope }, &ctx.deps())
        .await
        .unwrap_err();
    assert_eq!(err, FederationError::NoRebacRelation);
}

#[tokio::test]
async fn accept_rejects_after_revocation() {
    let ctx = ReceiverCtx::receiver_with_federation_ready();
    let envelope = ctx
        .build_signed_propose_envelope_with_one_record(MemoryVisibility::Team)
        .await;
    let _ = accept_share(
        AcceptShareRequest {
            envelope: envelope.clone(),
        },
        &ctx.deps(),
    )
    .await
    .unwrap();
    let link_id = envelope.link.as_ref().expect("propose").link_id.clone();
    ctx.mark_link_revoked_for_test(&link_id).await;
    let err = accept_share(AcceptShareRequest { envelope }, &ctx.deps())
        .await
        .unwrap_err();
    assert_eq!(err, FederationError::Revoked);
}
