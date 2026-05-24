//! Tests for the `propose_share` verb (brief §12.a).

use cairn_core::domain::MemoryVisibility;
use cairn_core::error::federation::FederationError;
use cairn_core::verbs::propose_share::{
    PROPAGATION_OUTBOUND_KIND, ProposeShareRequest, propose_share,
};

mod common;
use common::TestCtx;

#[tokio::test]
async fn propose_share_happy_path_signs_and_enqueues() {
    let ctx = TestCtx::issuer_with_federation_ready();
    let record_id = ctx.insert_project_record("hello").await;

    let req = ProposeShareRequest {
        record_ids: vec![record_id.clone()],
        grantee: Some(ctx.bob_identity()),
        scope: ctx.scope(),
        grant_tier: MemoryVisibility::Team,
        expires_at: ctx.in_one_hour(),
        peer: Some(ctx.peer_endpoint()),
    };

    let resp = propose_share(req, &ctx.deps()).await.expect("ok");

    assert_eq!(resp.link.payload.grant_tier, MemoryVisibility::Team);
    assert_eq!(
        resp.link.payload.target_id_hashes.len(),
        1,
        "one record in, one hash out",
    );
    assert_eq!(
        resp.link.link_id,
        format!("share-{}", resp.operation_id),
        "link_id must follow `share-<operation_id>`",
    );

    // Signature verifies against the issuer's verifying key.
    resp.link
        .verify_signature(&ctx.signing_key.verifying_key())
        .expect("signature verifies");

    // Atomic outbox commit: one consent event AND one queued job.
    let jobs = ctx.pending_jobs(PROPAGATION_OUTBOUND_KIND).await;
    assert_eq!(jobs.len(), 1, "exactly one propagation job enqueued");
    assert_eq!(
        jobs[0].dedupe_key.as_deref(),
        Some(resp.operation_id.as_str()),
        "job dedupe_key ties back to operation_id",
    );
    assert!(
        ctx.has_consent_event(
            &resp.link.link_id,
            cairn_core::domain::ConsentKind::FederationGrant,
        )
        .await,
        "consent journal must carry a FederationGrant row",
    );
}

#[tokio::test]
async fn propose_share_rejects_when_federation_not_advertised() {
    let ctx = TestCtx::issuer_with_federation_off();
    let record_id = ctx.insert_project_record("body").await;

    let err = propose_share(
        ProposeShareRequest {
            record_ids: vec![record_id],
            grantee: Some(ctx.bob_identity()),
            scope: ctx.scope(),
            grant_tier: MemoryVisibility::Team,
            expires_at: ctx.in_one_hour(),
            peer: None,
        },
        &ctx.deps(),
    )
    .await
    .unwrap_err();
    assert_eq!(err, FederationError::CapabilityDisabled);
    // The capability gate must run *before* any outbox write — assert
    // no journal / job rows leaked.
    assert!(ctx.pending_jobs(PROPAGATION_OUTBOUND_KIND).await.is_empty());
}

#[tokio::test]
async fn propose_share_rejects_rebac_deny() {
    let ctx = TestCtx::issuer_without_share_relation();
    let record_id = ctx.insert_project_record("body").await;

    let err = propose_share(
        ProposeShareRequest {
            record_ids: vec![record_id],
            grantee: Some(ctx.bob_identity()),
            scope: ctx.scope(),
            grant_tier: MemoryVisibility::Team,
            expires_at: ctx.in_one_hour(),
            peer: Some(ctx.peer_endpoint()),
        },
        &ctx.deps(),
    )
    .await
    .unwrap_err();
    assert_eq!(err, FederationError::NoRebacRelation);
    assert!(ctx.pending_jobs(PROPAGATION_OUTBOUND_KIND).await.is_empty());
}

#[tokio::test]
async fn propose_share_rejects_private_or_session_grant_tier() {
    let ctx = TestCtx::issuer_with_federation_ready();
    let record_id = ctx.insert_project_record("body").await;
    let err = propose_share(
        ProposeShareRequest {
            record_ids: vec![record_id.clone()],
            grantee: None,
            scope: ctx.scope(),
            grant_tier: MemoryVisibility::Private,
            expires_at: ctx.in_one_hour(),
            peer: None,
        },
        &ctx.deps(),
    )
    .await
    .unwrap_err();
    assert_eq!(err, FederationError::TierMismatch);

    let err_session = propose_share(
        ProposeShareRequest {
            record_ids: vec![record_id],
            grantee: None,
            scope: ctx.scope(),
            grant_tier: MemoryVisibility::Session,
            expires_at: ctx.in_one_hour(),
            peer: None,
        },
        &ctx.deps(),
    )
    .await
    .unwrap_err();
    assert_eq!(err_session, FederationError::TierMismatch);

    assert!(ctx.pending_jobs(PROPAGATION_OUTBOUND_KIND).await.is_empty());
}
