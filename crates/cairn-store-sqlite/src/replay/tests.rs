//! Replay-ledger + challenge unit tests.
//!
//! Issue #52 acceptance criteria covered here:
//! - Duplicate `operation_id` / nonce → `Duplicate`.
//! - Out-of-order sequence → `OutOfOrder` without state advance.
//! - First-seen issuer bootstrap via UPSERT preserves strict-advance.
//! - Challenge mode consumes exactly one outstanding challenge.
//! - Challenge nonce is single-use.
//! - Expired challenges are rejected with TTL semantics.
//!
//! Concurrency tests live in `tests/replay_concurrency.rs`.

use cairn_core::generated::common::{Identity, Nonce16Base64, Ulid};
use cairn_core::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};
use rusqlite::Connection;

use super::challenge::{MintedChallenge, mint_challenge, purge_expired_challenges};
use super::{ReplayError, WalPrepareInputs, prepare_wal_with_replay};

/// Open an in-memory store + apply every migration.
fn fresh_store() -> Connection {
    crate::vec_ext::register_vec0();
    let mut conn = Connection::open_in_memory().expect("open in-memory");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("pragma");
    crate::migrations::migrations()
        .to_latest(&mut conn)
        .expect("migrate to head");
    conn
}

const NOW_MS: i64 = 1_700_000_000_000;
const TTL_MS: i64 = 60_000;

/// Fixed signed intent that is well-formed but whose signature is a
/// placeholder. Replay-ledger code does not verify signatures — that is
/// the upstream verifier's job.
fn intent_with(
    op_id: &str,
    nonce_b64: &str,
    sequence: Option<u64>,
    challenge_b64: Option<&str>,
) -> SignedIntent {
    SignedIntent {
        chain_parents: vec![],
        expires_at: "2026-04-22T14:07:11Z".into(),
        issued_at: "2026-04-22T14:02:11Z".into(),
        issuer: Identity("hmn:tafeng".into()),
        key_version: 1,
        nonce: Nonce16Base64(nonce_b64.into()),
        operation_id: Ulid(op_id.into()),
        scope: SignedIntentScope {
            tenant: "acme".into(),
            workspace: "ws".into(),
            entity: "ent".into(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence,
        server_challenge: challenge_b64.map(|s| Nonce16Base64(s.into())),
        signature: cairn_core::generated::common::Ed25519Signature(format!(
            "ed25519:{}",
            "0".repeat(128)
        )),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    }
}

fn op_id(suffix: u8) -> String {
    // 26-char Crockford base32 ULID stub. The replay code does not parse
    // ULIDs; the migration column is TEXT.
    let mut s = "01HQZX9F5N00000000000000".to_string();
    s.push((b'0' + suffix / 10) as char);
    s.push((b'0' + suffix % 10) as char);
    s
}

fn nonce_b64(suffix: u8) -> String {
    // 24-char base64 (16 raw bytes + `==`). Build from raw bytes so the
    // result is always a valid Nonce16Base64 — encoding the same length
    // input byte-by-byte preserves the standard alphabet.
    use base64::Engine as _;
    let mut bytes = [0u8; 16];
    bytes[0] = suffix;
    bytes[1] = suffix.wrapping_mul(31);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn inputs(now_ms: i64) -> WalPrepareInputs<'static> {
    WalPrepareInputs {
        kind: "upsert",
        plan_ref: None,
        now_ms,
    }
}

#[test]
fn sequence_mode_first_insert_creates_issuer_seq_row() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let intent = intent_with(&op_id(1), &nonce_b64(1), Some(1), None);
    prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS)).expect("admit");
    tx.commit().expect("commit");

    let high_water: i64 = conn
        .query_row(
            "SELECT high_water FROM issuer_seq WHERE issuer = 'hmn:tafeng'",
            [],
            |r| r.get(0),
        )
        .expect("issuer_seq row");
    assert_eq!(high_water, 1);
}

#[test]
fn sequence_mode_strict_advance() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(1), &nonce_b64(1), Some(1), None),
        &inputs(NOW_MS),
    )
    .expect("first");
    prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(2), &nonce_b64(2), Some(2), None),
        &inputs(NOW_MS + 1),
    )
    .expect("strict advance ok");
    tx.commit().expect("commit");
}

#[test]
fn sequence_mode_rejects_repeat_sequence() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(1), &nonce_b64(1), Some(5), None),
        &inputs(NOW_MS),
    )
    .expect("first");
    let err = prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(2), &nonce_b64(2), Some(5), None),
        &inputs(NOW_MS + 1),
    )
    .expect_err("repeat seq");
    match err {
        ReplayError::OutOfOrder {
            high_water,
            attempted,
            ..
        } => {
            assert_eq!(high_water, 5);
            assert_eq!(attempted, 5);
        }
        other => panic!("expected OutOfOrder, got {other:?}"),
    }
}

#[test]
fn sequence_mode_rejects_lower_sequence() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(1), &nonce_b64(1), Some(10), None),
        &inputs(NOW_MS),
    )
    .expect("first");
    let err = prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(2), &nonce_b64(2), Some(7), None),
        &inputs(NOW_MS + 1),
    )
    .expect_err("lower seq");
    assert!(matches!(
        err,
        ReplayError::OutOfOrder {
            high_water: 10,
            attempted: 7,
            ..
        }
    ));
}

#[test]
fn sequence_mode_tolerates_gaps() {
    // Brief §4.2: "Sequence gaps are tolerated; reversals are not."
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(1), &nonce_b64(1), Some(5), None),
        &inputs(NOW_MS),
    )
    .expect("first");
    prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(2), &nonce_b64(2), Some(100), None),
        &inputs(NOW_MS + 1),
    )
    .expect("gap is fine");
    tx.commit().expect("commit");
}

#[test]
fn duplicate_operation_id_rejected_as_replay() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let intent = intent_with(&op_id(1), &nonce_b64(1), Some(1), None);
    prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS)).expect("first");

    // Same operation_id, different nonce / sequence — the wal_ops insert
    // is a no-op (ON CONFLICT) but the `used` insert hits the PK and we
    // must surface Duplicate, not Sqlite.
    let dup = intent_with(&op_id(1), &nonce_b64(2), Some(2), None);
    let err = prepare_wal_with_replay(&tx, &dup, &inputs(NOW_MS + 1)).expect_err("replay");
    // Either Duplicate (same envelope retry) or OperationMismatch
    // (same op_id, different envelope content) — both are correct
    // fail-closed responses (round-1 and round-5 review fixes).
    assert!(
        matches!(
            err,
            ReplayError::Duplicate { .. } | ReplayError::OperationMismatch { .. }
        ),
        "expected Duplicate or OperationMismatch, got {err:?}"
    );
}

#[test]
fn duplicate_nonce_rejected_as_replay() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(1), &nonce_b64(1), Some(1), None),
        &inputs(NOW_MS),
    )
    .expect("first");

    // Reused nonce on a fresh operation_id and a strictly-greater
    // sequence — only the (issuer, nonce) UNIQUE catches this.
    let dup_nonce = intent_with(&op_id(2), &nonce_b64(1), Some(2), None);
    let err =
        prepare_wal_with_replay(&tx, &dup_nonce, &inputs(NOW_MS + 1)).expect_err("nonce replay");
    assert!(matches!(err, ReplayError::Duplicate { .. }));
}

#[test]
fn xor_violation_rejected_when_both_modes_present() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let intent = intent_with(&op_id(1), &nonce_b64(1), Some(1), Some(&nonce_b64(99)));
    let err = prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS)).expect_err("xor both");
    assert!(matches!(err, ReplayError::ModeXorViolation));
}

#[test]
fn xor_violation_rejected_when_neither_mode_present() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let intent = intent_with(&op_id(1), &nonce_b64(1), None, None);
    let err = prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS)).expect_err("xor none");
    assert!(matches!(err, ReplayError::ModeXorViolation));
}

#[test]
fn challenge_mode_consumes_outstanding_row() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let MintedChallenge {
        nonce_b64: chal, ..
    } = mint_challenge(&tx, "hmn:tafeng", NOW_MS, TTL_MS).expect("mint");
    let intent = intent_with(&op_id(1), &nonce_b64(1), None, Some(&chal));
    prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS + 1)).expect("admit");

    let remaining: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM outstanding_challenges WHERE issuer = 'hmn:tafeng'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(remaining, 0, "challenge must be consumed");
}

#[test]
fn challenge_mode_single_use() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let MintedChallenge {
        nonce_b64: chal, ..
    } = mint_challenge(&tx, "hmn:tafeng", NOW_MS, TTL_MS).expect("mint");
    prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(1), &nonce_b64(1), None, Some(&chal)),
        &inputs(NOW_MS + 1),
    )
    .expect("first");
    let err = prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(2), &nonce_b64(2), None, Some(&chal)),
        &inputs(NOW_MS + 2),
    )
    .expect_err("reuse");
    assert!(matches!(err, ReplayError::ChallengeMissing { .. }));
}

#[test]
fn challenge_mode_missing_nonce_rejected() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    // No mint — referenced challenge does not exist.
    let intent = intent_with(&op_id(1), &nonce_b64(1), None, Some(&nonce_b64(99)));
    let err = prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS)).expect_err("missing");
    assert!(matches!(err, ReplayError::ChallengeMissing { .. }));
}

#[test]
fn challenge_mode_expired_rejected() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let MintedChallenge {
        nonce_b64: chal, ..
    } = mint_challenge(&tx, "hmn:tafeng", NOW_MS, 1).expect("mint short-ttl");
    // Move clock past expiry.
    let intent = intent_with(&op_id(1), &nonce_b64(1), None, Some(&chal));
    let err = prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS + 100)).expect_err("expired");
    assert!(matches!(err, ReplayError::ChallengeExpired { .. }));
}

#[test]
fn challenge_mode_does_not_advance_issuer_seq() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let MintedChallenge {
        nonce_b64: chal, ..
    } = mint_challenge(&tx, "hmn:tafeng", NOW_MS, TTL_MS).expect("mint");
    prepare_wal_with_replay(
        &tx,
        &intent_with(&op_id(1), &nonce_b64(1), None, Some(&chal)),
        &inputs(NOW_MS + 1),
    )
    .expect("admit");
    let row: Option<i64> = tx
        .query_row(
            "SELECT high_water FROM issuer_seq WHERE issuer = 'hmn:tafeng'",
            [],
            |r| r.get(0),
        )
        .ok();
    assert!(
        row.is_none(),
        "challenge mode must NOT create an issuer_seq row; got {row:?}"
    );
}

#[test]
fn purge_expired_challenges_removes_only_stale_rows() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let _stale = mint_challenge(&tx, "hmn:a", NOW_MS, 1).expect("stale");
    let _fresh = mint_challenge(&tx, "hmn:a", NOW_MS, TTL_MS).expect("fresh");
    let dropped = purge_expired_challenges(&tx, NOW_MS + 100).expect("purge");
    assert_eq!(dropped, 1);

    let remaining: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM outstanding_challenges WHERE issuer = 'hmn:a'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(remaining, 1);
}

#[test]
fn mixed_issuers_do_not_share_state() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    // hmn:a admits sequence 5 …
    let mut a = intent_with(&op_id(1), &nonce_b64(1), Some(5), None);
    a.issuer = Identity("hmn:a".into());
    prepare_wal_with_replay(&tx, &a, &inputs(NOW_MS)).expect("a@5");
    // … and hmn:b can still admit sequence 1 because its high_water is 0.
    let mut b = intent_with(&op_id(2), &nonce_b64(2), Some(1), None);
    b.issuer = Identity("hmn:b".into());
    prepare_wal_with_replay(&tx, &b, &inputs(NOW_MS + 1)).expect("b@1");
    tx.commit().expect("commit");
}

#[test]
fn sequence_overflow_rejected() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let huge = u64::MAX;
    let intent = intent_with(&op_id(1), &nonce_b64(1), Some(huge), None);
    let err = prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS)).expect_err("overflow");
    assert!(matches!(err, ReplayError::SequenceOverflow { .. }));
}

#[test]
fn unpadded_nonce_admits_via_idl_shape() {
    // The IDL `Nonce16Base64` accepts BOTH 22-char unpadded and 24-char
    // padded forms. Issue #52 round-1 review #1: replay decoder must
    // round-trip both, not just the padded form.
    use base64::Engine as _;
    let bytes: [u8; 16] = [
        0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD,
        0xEF,
    ];
    let unpadded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes);
    assert_eq!(unpadded.len(), 22);

    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let intent = intent_with(&op_id(1), &unpadded, Some(1), None);
    prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS)).expect("unpadded nonce admits");
    tx.commit().expect("commit");
}

#[test]
fn unpadded_challenge_redeems_via_idl_shape() {
    use base64::Engine as _;
    let raw: [u8; 16] = [
        0x55, 0xAA, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
        0xEE,
    ];
    let unpadded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(raw);

    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    // Insert the challenge directly so we bypass mint_challenge's padded
    // emit and exercise the decoder's unpadded path.
    tx.execute(
        "INSERT INTO outstanding_challenges (issuer, challenge, expires_at)
         VALUES (?1, ?2, ?3)",
        rusqlite::params!["hmn:tafeng", &raw[..], NOW_MS + TTL_MS],
    )
    .expect("insert challenge");
    let intent = intent_with(&op_id(1), &nonce_b64(1), None, Some(&unpadded));
    prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS + 1))
        .expect("unpadded server_challenge redeems");
}

#[test]
fn wal_op_mismatch_rejected_on_conflict() {
    // Round-1 review #3: a same-issuer wal_ops row staged under the
    // same operation_id but different signature/envelope must NOT let
    // the replay-ledger consume succeed.
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");

    let original = intent_with(&op_id(1), &nonce_b64(1), Some(1), None);
    prepare_wal_with_replay(&tx, &original, &inputs(NOW_MS)).expect("first admit");

    // Build a second intent that shares operation_id but mutates a
    // signed-payload field (target_hash) — different envelope under
    // the same op_id.
    let mut tampered = intent_with(&op_id(1), &nonce_b64(2), Some(2), None);
    tampered.target_hash = format!("sha256:{}", "b".repeat(64));
    let err = prepare_wal_with_replay(&tx, &tampered, &inputs(NOW_MS + 1)).expect_err("mismatch");
    assert!(
        matches!(
            err,
            ReplayError::OperationMismatch { .. } | ReplayError::Duplicate { .. }
        ),
        "expected OperationMismatch or Duplicate, got {err:?}"
    );
}

#[test]
fn chain_parents_persisted_into_wal_op_deps() {
    // Round-5 review #1: signed `chain_parents` must land in
    // `wal_op_deps` so the WAL recovery / commit scheduler honours
    // the partial ordering the issuer signed.
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");

    let parent_a = intent_with(&op_id(1), &nonce_b64(1), Some(1), None);
    prepare_wal_with_replay(&tx, &parent_a, &inputs(NOW_MS)).expect("parent A");
    let parent_b = intent_with(&op_id(2), &nonce_b64(2), Some(2), None);
    prepare_wal_with_replay(&tx, &parent_b, &inputs(NOW_MS + 1)).expect("parent B");

    // Child cites both parents in chain_parents.
    let mut child = intent_with(&op_id(3), &nonce_b64(3), Some(3), None);
    child.chain_parents = vec![
        cairn_core::generated::common::Ulid(op_id(1)),
        cairn_core::generated::common::Ulid(op_id(2)),
    ];
    prepare_wal_with_replay(&tx, &child, &inputs(NOW_MS + 2)).expect("child");

    let dep_count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM wal_op_deps WHERE operation_id = ?1",
            rusqlite::params![&op_id(3)],
            |r| r.get(0),
        )
        .expect("count deps");
    assert_eq!(dep_count, 2, "two parent edges must persist");
    tx.commit().expect("commit");
}

#[test]
fn chain_parents_unknown_parent_rejected() {
    // Unknown parent operation_id ⇒ FK violation on wal_op_deps; the
    // whole admission rolls back so replay state stays consistent.
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");

    let mut intent = intent_with(&op_id(1), &nonce_b64(1), Some(1), None);
    intent.chain_parents = vec![cairn_core::generated::common::Ulid(op_id(99))]; // not in wal_ops
    let err = prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS)).expect_err("unknown parent");
    // Surfaces as Sqlite (FK constraint failure); the trigger / FK
    // chain may produce different SQLite error codes across versions,
    // so just assert the failure mode is Sqlite-level (i.e., not
    // Duplicate / OutOfOrder / OperationMismatch).
    assert!(matches!(err, ReplayError::Sqlite(_)), "got {err:?}");
}

#[test]
fn signed_payload_columns_derive_from_intent() {
    // Round-5 review #2: scope_json + expires_at_ms come from the
    // verified intent, not from caller-supplied WalPrepareInputs.
    // Caller cannot widen scope or extend TTL beyond the signature.
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");

    let intent = intent_with(&op_id(1), &nonce_b64(1), Some(1), None);
    prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS)).expect("admit");

    let (scope_json, expires_at): (String, i64) = tx
        .query_row(
            "SELECT scope_json, expires_at FROM wal_ops WHERE operation_id = ?1",
            rusqlite::params![&op_id(1)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("read row");
    // The fixture intent's `expires_at` is "2026-04-22T14:07:11Z".
    // chrono's RFC-3339 parser converts this to 1_776_866_831_000 ms
    // (epoch-ms). Compare exactly so future parse drift fails loudly.
    assert_eq!(expires_at, 1_776_866_831_000);
    assert!(
        scope_json.contains("\"tenant\":\"acme\""),
        "scope_json: {scope_json}"
    );
    assert!(scope_json.contains("\"workspace\":\"ws\""));
    assert!(scope_json.contains("\"entity\":\"ent\""));
    assert!(scope_json.contains("\"tier\":\"project\""));
}

#[test]
fn rolled_back_transaction_leaves_no_state() {
    let mut conn = fresh_store();
    let tx = conn.transaction().expect("tx");
    let intent = intent_with(&op_id(1), &nonce_b64(1), Some(1), None);
    prepare_wal_with_replay(&tx, &intent, &inputs(NOW_MS)).expect("admit");
    drop(tx); // rollback

    let used: i64 = conn
        .query_row("SELECT COUNT(*) FROM used", [], |r| r.get(0))
        .expect("count");
    let wal: i64 = conn
        .query_row("SELECT COUNT(*) FROM wal_ops", [], |r| r.get(0))
        .expect("count");
    let issuer_seq: i64 = conn
        .query_row("SELECT COUNT(*) FROM issuer_seq", [], |r| r.get(0))
        .expect("count");
    assert_eq!((used, wal, issuer_seq), (0, 0, 0));
}
