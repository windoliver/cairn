//! Concurrency / TTL invariants for the replay ledger (issue #52).
//!
//! Issue verification list:
//! - Concurrency tests with same issuer and mixed issuers.
//! - Replay duplicate tests.
//! - Handshake TTL + single-use tests.
//!
//! These run against a shared on-disk `SQLite` file in WAL mode (the
//! production journal mode) so the per-file write lock + WAL-mode
//! reader concurrency are exercised end-to-end. WAL mode admits one
//! writer at a time, so concurrent same-issuer envelopes are
//! serialized by `SQLite` first and the per-issuer CAS second; the test
//! asserts only one of N concurrent envelopes claiming the same
//! sequence wins.
//!
//! Brief sources: §4.2 atomic replay + ordering, §8.0.a handshake TTL.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use cairn_core::generated::common::{Identity, Nonce16Base64, Ulid};
use cairn_core::generated::envelope::{SignedIntent, SignedIntentScope, SignedIntentScopeTier};
use cairn_store_sqlite::replay::challenge::{MintedChallenge, mint_challenge};
use cairn_store_sqlite::replay::test_helpers::prepare_wal_with_replay;
use cairn_store_sqlite::replay::{ReplayError, WalPrepareInputs};
use parking_lot::Mutex;
use rusqlite::Connection;
use tempfile::tempdir;

const TTL_MS: i64 = 60_000;
const NOW_MS: i64 = 1_700_000_000_000;

fn open_at(path: &std::path::Path) -> Connection {
    cairn_store_sqlite::vec_ext::register_vec0();
    let mut conn = Connection::open(path).expect("open");
    conn.pragma_update(None, "journal_mode", "WAL")
        .expect("wal mode");
    conn.pragma_update(None, "foreign_keys", "ON").expect("fk");
    cairn_store_sqlite::migrations::migrations()
        .to_latest(&mut conn)
        .expect("migrate");
    conn
}

fn intent_seq(issuer: &str, op_suffix: u8, sequence: u64, nonce_seed: u8) -> SignedIntent {
    use base64::Engine as _;
    let mut nonce = [0u8; 16];
    nonce[0] = nonce_seed;
    nonce[1] = nonce_seed.wrapping_mul(31);
    nonce[2] = op_suffix;
    SignedIntent {
        chain_parents: vec![],
        expires_at: "2026-04-22T14:07:11Z".into(),
        issued_at: "2026-04-22T14:02:11Z".into(),
        issuer: Identity(issuer.into()),
        key_version: 1,
        nonce: Nonce16Base64(base64::engine::general_purpose::STANDARD.encode(nonce)),
        operation_id: Ulid(format!("01HQZX9F5N0000000000000{op_suffix:03}")),
        scope: SignedIntentScope {
            tenant: "acme".into(),
            workspace: "ws".into(),
            entity: "ent".into(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: Some(sequence),
        server_challenge: None,
        signature: cairn_core::generated::common::Ed25519Signature(format!(
            "ed25519:{}",
            "0".repeat(128)
        )),
        target_hash: format!("sha256:{}", "a".repeat(64)),
    }
}

fn intent_chal(issuer: &str, op_suffix: u8, challenge: &str, nonce_seed: u8) -> SignedIntent {
    let mut i = intent_seq(issuer, op_suffix, 1, nonce_seed);
    i.sequence = None;
    i.server_challenge = Some(Nonce16Base64(challenge.into()));
    i
}

fn inputs_at(now_ms: i64) -> WalPrepareInputs<'static> {
    WalPrepareInputs {
        kind: "upsert",
        plan_ref: None,
        now_ms,
    }
}

/// Helper: run `f` against a transaction on the shared connection,
/// retrying on `SQLITE_BUSY` (WAL mode admits one writer at a time and
/// concurrent threads need to back off).
fn with_busy_retry<F, T>(conn: &Mutex<Connection>, mut f: F) -> Result<T, ReplayError>
where
    F: FnMut(&rusqlite::Transaction<'_>) -> Result<T, ReplayError>,
{
    for attempt in 0..200u32 {
        let mut guard = conn.lock();
        let outcome = (|| -> Result<Result<T, ReplayError>, rusqlite::Error> {
            let tx = guard.transaction()?;
            match f(&tx) {
                Ok(v) => match tx.commit() {
                    Ok(()) => Ok(Ok(v)),
                    Err(e) => Err(e),
                },
                Err(e) => Ok(Err(e)),
            }
        })();
        match outcome {
            Ok(inner) => return inner,
            Err(rusqlite::Error::SqliteFailure(e, _))
                if matches!(e.code, rusqlite::ErrorCode::DatabaseBusy) =>
            {
                drop(guard);
                thread::sleep(Duration::from_millis(1 + u64::from(attempt)));
            }
            Err(e) => return Err(ReplayError::Sqlite(e)),
        }
    }
    Err(ReplayError::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        Some("busy retry budget exhausted".into()),
    )))
}

#[test]
fn same_issuer_concurrent_sequence_only_one_wins() {
    // N threads all claim sequence=1 against issuer hmn:race. WAL-mode
    // single-writer + per-issuer CAS ⇒ exactly one Ok, the rest get
    // OutOfOrder once the high_water has advanced past their attempt.
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("c.db");
    let conn = Arc::new(Mutex::new(open_at(&db)));

    let n_threads = 8;
    let success = Arc::new(AtomicUsize::new(0));
    let out_of_order = Arc::new(AtomicUsize::new(0));
    let other = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for i in 0..n_threads {
        let conn = Arc::clone(&conn);
        let success = Arc::clone(&success);
        let out_of_order = Arc::clone(&out_of_order);
        let other = Arc::clone(&other);
        handles.push(thread::spawn(move || {
            // Each thread uses a unique op_id and nonce so the only
            // shared contention is `(issuer, sequence)` = (hmn:race, 1).
            let intent = intent_seq("hmn:race", i, 1, i + 1);
            let result = with_busy_retry(&conn, |tx| {
                prepare_wal_with_replay(tx, &intent, &inputs_at(NOW_MS + i64::from(i)))
            });
            match result {
                Ok(()) => success.fetch_add(1, Ordering::SeqCst),
                Err(ReplayError::OutOfOrder { .. } | ReplayError::Duplicate { .. }) => {
                    out_of_order.fetch_add(1, Ordering::SeqCst)
                }
                Err(e) => {
                    eprintln!("unexpected: {e:?}");
                    other.fetch_add(1, Ordering::SeqCst)
                }
            };
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    assert_eq!(success.load(Ordering::SeqCst), 1, "exactly one winner");
    assert_eq!(other.load(Ordering::SeqCst), 0, "no unexpected errors");
    assert_eq!(
        out_of_order.load(Ordering::SeqCst),
        n_threads as usize - 1,
        "every loser is OutOfOrder or Duplicate"
    );
}

#[test]
fn mixed_issuers_concurrent_admit_independently() {
    // Each thread uses a unique issuer; all envelopes should admit
    // because per-issuer state is independent. Asserts no cross-issuer
    // pollution under contention.
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("m.db");
    let conn = Arc::new(Mutex::new(open_at(&db)));

    let n_threads = 8;
    let mut handles = Vec::new();
    for i in 0..n_threads {
        let conn = Arc::clone(&conn);
        handles.push(thread::spawn(move || {
            let issuer = format!("hmn:user-{i}");
            let intent = intent_seq(&issuer, i, 1, i + 1);
            with_busy_retry(&conn, |tx| {
                prepare_wal_with_replay(tx, &intent, &inputs_at(NOW_MS + i64::from(i)))
            })
        }));
    }
    let mut successes = 0;
    for h in handles {
        if let Ok(Ok(())) = h.join() {
            successes += 1;
        }
    }
    assert_eq!(
        successes, n_threads as usize,
        "every distinct issuer must admit"
    );

    let row_count: i64 = conn
        .lock()
        .query_row("SELECT COUNT(*) FROM used", [], |r| r.get(0))
        .expect("count");
    assert_eq!(row_count, i64::from(n_threads));
}

#[test]
fn challenge_mode_single_use_under_concurrency() {
    // Mint a single challenge, fire N threads at it — only one should
    // win the consume; the rest get ChallengeMissing. Asserts the
    // single-use guarantee survives concurrent contention.
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("ch.db");
    let conn = Arc::new(Mutex::new(open_at(&db)));

    let chal: MintedChallenge = {
        let mut guard = conn.lock();
        let tx = guard.transaction().expect("tx");
        let m = mint_challenge(&tx, "hmn:race", NOW_MS, TTL_MS).expect("mint");
        tx.commit().expect("commit");
        m
    };

    let n_threads = 8;
    let success = Arc::new(AtomicUsize::new(0));
    let missing = Arc::new(AtomicUsize::new(0));
    let other = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for i in 0..n_threads {
        let conn = Arc::clone(&conn);
        let chal = chal.clone();
        let success = Arc::clone(&success);
        let missing = Arc::clone(&missing);
        let other = Arc::clone(&other);
        handles.push(thread::spawn(move || {
            let intent = intent_chal("hmn:race", i, &chal.nonce_b64, i + 1);
            let result = with_busy_retry(&conn, |tx| {
                prepare_wal_with_replay(tx, &intent, &inputs_at(NOW_MS + 1 + i64::from(i)))
            });
            match result {
                Ok(()) => success.fetch_add(1, Ordering::SeqCst),
                Err(ReplayError::ChallengeMissing { .. }) => missing.fetch_add(1, Ordering::SeqCst),
                Err(e) => {
                    eprintln!("unexpected: {e:?}");
                    other.fetch_add(1, Ordering::SeqCst)
                }
            };
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    assert_eq!(success.load(Ordering::SeqCst), 1, "exactly one winner");
    assert_eq!(other.load(Ordering::SeqCst), 0, "no unexpected errors");
    assert_eq!(
        missing.load(Ordering::SeqCst),
        n_threads as usize - 1,
        "every loser sees ChallengeMissing"
    );
}

#[test]
fn duplicate_operation_id_under_concurrency() {
    // Same op_id, different threads — exactly one wins, others get
    // Duplicate or OutOfOrder. Verifies the (operation_id) PK survives
    // concurrent insertion attempts.
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("d.db");
    let conn = Arc::new(Mutex::new(open_at(&db)));

    let n_threads = 6;
    let success = Arc::new(AtomicUsize::new(0));
    let dup_or_oo = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for i in 0..n_threads {
        let conn = Arc::clone(&conn);
        let success = Arc::clone(&success);
        let dup_or_oo = Arc::clone(&dup_or_oo);
        handles.push(thread::spawn(move || {
            // Same op_id (suffix 7) across all threads, but unique nonce
            // per thread so the (issuer, nonce) UNIQUE doesn't fire
            // first. The (operation_id) PK is the load-bearing
            // duplicate guard tested here.
            let mut intent = intent_seq("hmn:dup", 7, 1, i + 1);
            intent.sequence = Some(u64::from(i + 1));
            let result = with_busy_retry(&conn, |tx| {
                prepare_wal_with_replay(tx, &intent, &inputs_at(NOW_MS + 1 + i64::from(i)))
            });
            match result {
                Ok(()) => success.fetch_add(1, Ordering::SeqCst),
                Err(
                    ReplayError::Duplicate { .. }
                    | ReplayError::OutOfOrder { .. }
                    | ReplayError::OperationMismatch { .. },
                ) => dup_or_oo.fetch_add(1, Ordering::SeqCst),
                Err(e) => panic!("unexpected: {e:?}"),
            };
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    assert_eq!(success.load(Ordering::SeqCst), 1, "exactly one winner");
    assert_eq!(
        dup_or_oo.load(Ordering::SeqCst),
        n_threads as usize - 1,
        "rest are Duplicate or OutOfOrder"
    );
}

#[test]
fn handshake_ttl_expires_unused_challenge() {
    // Mint a challenge with a short TTL, advance the clock past expiry,
    // then attempt to consume — must return ChallengeExpired and leave
    // the row in `outstanding_challenges` (the DELETE was skipped).
    let dir = tempdir().expect("tempdir");
    let db = dir.path().join("t.db");
    let mut conn = open_at(&db);

    let chal = {
        let tx = conn.transaction().expect("tx");
        let m = mint_challenge(&tx, "hmn:tafeng", NOW_MS, /* ttl_ms */ 100).expect("mint");
        tx.commit().expect("commit");
        m
    };

    let now_after_expiry = NOW_MS + 200;
    let intent = intent_chal("hmn:tafeng", 1, &chal.nonce_b64, 1);
    let tx = conn.transaction().expect("tx");
    let err = prepare_wal_with_replay(&tx, &intent, &inputs_at(now_after_expiry))
        .expect_err("must reject expired");
    assert!(matches!(err, ReplayError::ChallengeExpired { .. }));
    drop(tx);

    // Row remains for the operator's `purge_expired_challenges` sweep.
    let still_present: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM outstanding_challenges WHERE issuer = 'hmn:tafeng'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(still_present, 1);
}
