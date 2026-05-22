//! Tests for the `revoke_share` verb (brief §12.a).

use cairn_core::contract::consent_lookup::StoredRevocation;
use cairn_core::domain::consent::ConsentKind;
use cairn_core::error::federation::FederationError;
use cairn_core::verbs::revoke_share::{
    REVOKE_OUTBOUND_KIND, RevokeShareRequest, revoke_share,
};

mod common;
use common::TestCtx;

#[tokio::test]
async fn revoke_marks_link_revoked_and_enqueues_outbound() {
    let ctx = TestCtx::issuer_with_federation_ready();
    let propose = ctx.mint_propose_share().await;
    let resp = revoke_share(
        RevokeShareRequest {
            link_id: propose.link.link_id.clone(),
        },
        &ctx.revoke_deps(),
    )
    .await
    .expect("ok");
    assert!(!resp.operation_id.is_empty());
    // Signed revocation carries the issuer + key_version + signature.
    assert_eq!(resp.revocation.issuer.0, "hmn:alice");
    assert_eq!(resp.revocation.key_version, 1);
    assert!(resp.revocation.signature.0.starts_with("ed25519:"));
    assert_eq!(resp.revocation.signature.0.len(), "ed25519:".len() + 128);
    assert_eq!(resp.revocation.link_id, propose.link.payload.operation_id);

    assert!(
        ctx.has_consent_event(&propose.link.link_id, ConsentKind::FederationRevoke)
            .await
    );
    assert_eq!(
        ctx.pending_jobs(REVOKE_OUTBOUND_KIND).await.len(),
        1,
        "exactly one outbound revoke job enqueued",
    );
}

#[tokio::test]
async fn revoke_rejects_unknown_link() {
    let ctx = TestCtx::issuer_with_federation_ready();
    let err = revoke_share(
        RevokeShareRequest {
            link_id: "no-such-link".into(),
        },
        &ctx.revoke_deps(),
    )
    .await
    .unwrap_err();
    assert_eq!(err, FederationError::UnknownLink);
    // Capability gate ran *after* an established federation_ready=true;
    // an unknown-link rejection must still leave no audit trail.
    assert!(ctx.pending_jobs(REVOKE_OUTBOUND_KIND).await.is_empty());
}

#[tokio::test]
async fn revoke_is_idempotent() {
    let ctx = TestCtx::issuer_with_federation_ready();
    let propose = ctx.mint_propose_share().await;
    let first = revoke_share(
        RevokeShareRequest {
            link_id: propose.link.link_id.clone(),
        },
        &ctx.revoke_deps(),
    )
    .await
    .unwrap();
    // Teach the consent-lookup fake about the first revoke so the
    // second call sees the dedup hit. The real adapter (T12)
    // materialises this from the consent_timeline projection inside
    // the same transaction as the apply.
    ctx.consent_lookup.record_revocation(
        &propose.link.link_id,
        StoredRevocation {
            operation_id: first.operation_id.clone(),
            revocation: first.revocation.clone(),
        },
    );

    let again = revoke_share(
        RevokeShareRequest {
            link_id: propose.link.link_id.clone(),
        },
        &ctx.revoke_deps(),
    )
    .await
    .unwrap();
    assert_eq!(first.operation_id, again.operation_id);
    assert_eq!(first.revocation, again.revocation);
    // Only one FederationRevoke event and one outbound_revoke job —
    // the second call's replay must not append a duplicate row.
    assert_eq!(
        ctx.count_consent_events(&propose.link.link_id, ConsentKind::FederationRevoke)
            .await,
        1,
    );
    assert_eq!(ctx.pending_jobs(REVOKE_OUTBOUND_KIND).await.len(), 1);
}

#[tokio::test]
async fn revoke_rejects_when_federation_disabled() {
    let ctx = TestCtx::issuer_with_federation_off();
    let err = revoke_share(
        RevokeShareRequest {
            link_id: "anything".into(),
        },
        &ctx.revoke_deps(),
    )
    .await
    .unwrap_err();
    assert_eq!(err, FederationError::CapabilityDisabled);
    // The capability gate must run *before* any outbox write or
    // consent lookup, so no journal / job rows leaked.
    assert!(ctx.pending_jobs(REVOKE_OUTBOUND_KIND).await.is_empty());
}
