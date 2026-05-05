//! End-to-end: mint a challenge via `StoreTx::mint_challenge`, then
//! redeem it via `prepare_wal_with_replay` against a real on-disk
//! `SQLite` vault. Mirrors the production flow that the CLI handshake
//! verb drives (issue #52).

use cairn_core::generated::common::{Identity, Nonce16Base64, Ulid};
use cairn_core::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};
use cairn_store_sqlite::open;
use cairn_store_sqlite::replay::{ReplayError, WalPrepareInputs};
use tempfile::tempdir;

const ISSUER: &str = "hmn:tafeng";
const TTL_MS: i64 = 60_000;

fn intent_with_challenge(op_suffix: &str, nonce_seed: u8, challenge: &str) -> SignedIntent {
    use base64::Engine as _;
    let mut nonce = [0u8; 16];
    nonce[0] = nonce_seed;
    nonce[1] = nonce_seed.wrapping_mul(31);
    SignedIntent {
        chain_parents: vec![],
        expires_at: "2026-04-22T14:07:11Z".into(),
        issued_at: "2026-04-22T14:02:11Z".into(),
        issuer: Identity(ISSUER.into()),
        key_version: 1,
        nonce: Nonce16Base64(base64::engine::general_purpose::STANDARD.encode(nonce)),
        operation_id: Ulid(format!("01HQZX9F5N00000000000000{op_suffix}")),
        scope: SignedIntentScope {
            tenant: "acme".into(),
            workspace: "ws".into(),
            entity: "ent".into(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: None,
        server_challenge: Some(Nonce16Base64(challenge.into())),
        signature: cairn_core::generated::common::Ed25519Signature(format!(
            "ed25519:{}",
            "0".repeat(128)
        )),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    }
}

fn inputs() -> WalPrepareInputs<'static> {
    WalPrepareInputs {
        kind: "upsert",
        plan_ref: None,
    }
}

/// Wrap `ReplayError` in `StoreError::Invariant` so the closure return
/// type matches `with_tx`'s required `StoreError`.
fn replay_to_store(e: &ReplayError) -> cairn_store_sqlite::StoreError {
    cairn_store_sqlite::StoreError::Invariant {
        what: format!("replay: {e}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mint_then_consume_via_public_store_api() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    let store = open(&db).await.expect("open store");

    let now_ms: i64 = 1_700_000_000_000;

    // Mint via the same path the CLI handshake verb uses.
    let chal = store
        .with_tx(move |tx| tx.mint_challenge(ISSUER, now_ms, TTL_MS))
        .await
        .expect("mint");

    // First redeem succeeds.
    let chal_b64 = chal.nonce_b64.clone();
    let intent_one = intent_with_challenge("AB", 1, &chal_b64);
    store
        .with_tx(move |tx| {
            tx.prepare_wal_with_replay_unverified(&intent_one, &inputs(), now_ms + 1)
                .map_err(|e| replay_to_store(&e))
        })
        .await
        .expect("first redeem");

    // Second redeem with the same challenge fails — single-use guarantee.
    let chal_b64_again = chal.nonce_b64.clone();
    let intent_two = intent_with_challenge("AC", 2, &chal_b64_again);
    let err = store
        .with_tx(move |tx| {
            tx.prepare_wal_with_replay_unverified(&intent_two, &inputs(), now_ms + 2)
                .map_err(|e| replay_to_store(&e))
        })
        .await
        .expect_err("second redeem must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("ChallengeMissing") || msg.contains("has no outstanding row"),
        "expected ChallengeMissing, got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ttl_expired_challenge_rejected_via_public_store_api() {
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("cairn.db");
    let store = open(&db).await.expect("open store");

    let now_ms: i64 = 1_700_000_000_000;
    let short_ttl = 10_i64;
    let chal = store
        .with_tx(move |tx| tx.mint_challenge(ISSUER, now_ms, short_ttl))
        .await
        .expect("mint short-ttl");

    let chal_b64 = chal.nonce_b64.clone();
    let intent = intent_with_challenge("AD", 3, &chal_b64);
    let err = store
        .with_tx(move |tx| {
            tx.prepare_wal_with_replay_unverified(&intent, &inputs(), now_ms + 1_000)
                .map_err(|e| replay_to_store(&e))
        })
        .await
        .expect_err("expired challenge must reject");
    let msg = format!("{err}");
    assert!(
        msg.contains("ChallengeExpired") || msg.contains("expired"),
        "expected ChallengeExpired, got: {msg}"
    );
}
