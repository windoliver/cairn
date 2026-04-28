// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! `expire_active`: setting an expiry in the past hides the row from normal
//! reads; `include_expired = true` makes it visible again.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    MemoryStore,
    apply::MemoryStoreApply,
    types::{ListQuery, TargetId},
};
use cairn_core::domain::{
    ChainRole, EvidenceVector, MemoryClass, MemoryKind, MemoryVisibility, Provenance,
    Rfc3339Timestamp, ScopeTuple,
    actor_chain::ActorChainEntry,
    identity::Identity,
    principal::Principal,
    record::{Ed25519Signature, MemoryRecord, RecordId},
};
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::tempdir;

fn make_record(body: &str) -> MemoryRecord {
    let user_id = Identity::parse("usr:exptest").expect("valid");
    MemoryRecord {
        id: RecordId::parse("01HQZX9F5N0000000000000030").expect("valid"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:exptest".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T08:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "f".repeat(64)),
            consent_ref: "consent:exp1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T08:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.3,
        confidence: 0.5,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T08:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "1".repeat(128))).expect("valid"),
        tags: vec!["exp".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

/// Stage + activate + `expire_active` (past timestamp) → `get` returns `None`,
/// `list(include_expired=false)` shows 0 rows, `list(include_expired=true)`
/// shows 1 row.
#[tokio::test]
async fn get_returns_none_after_past_expiry() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("expire-target-1");
    let rec = make_record("expires soon");

    // Stage + activate.
    store
        .with_apply_tx(test_apply_token(), {
            let r = rec.clone();
            let t = target.clone();
            move |tx| {
                tx.stage_version(&t, &r)?;
                tx.activate_version(&t, 1, None)?;
                Ok(())
            }
        })
        .await
        .expect("stage+activate");

    // Confirm it's visible before expiry.
    let principal = Principal::system(&test_apply_token());
    assert!(
        store
            .get(&principal, &target)
            .await
            .expect("get pre-expire")
            .is_some(),
        "record must be visible before expiry"
    );

    // Set expired_at to a time in the past.
    let past = Rfc3339Timestamp::parse("2000-01-01T00:00:00Z").expect("valid past");
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let exp = past.clone();
            move |tx| {
                tx.expire_active(&t, exp)?;
                Ok(())
            }
        })
        .await
        .expect("expire_active");

    // get must return None for an expired record.
    let got = store
        .get(&principal, &target)
        .await
        .expect("get post-expire");
    assert!(got.is_none(), "get must return None after past expiry");

    // Normal list: no expired rows shown.
    let mut q = ListQuery::new(principal.clone());
    q.include_expired = false;
    let normal = store.list(&q).await.expect("list normal");
    assert_eq!(normal.rows.len(), 0, "normal list must show 0 rows");

    // Forensic list: expired row is visible.
    let mut q_exp = ListQuery::new(principal.clone());
    q_exp.include_expired = true;
    let forensic = store.list(&q_exp).await.expect("list with include_expired");
    assert_eq!(
        forensic.rows.len(),
        1,
        "forensic list must show the expired row"
    );
    assert_eq!(forensic.rows[0].body, "expires soon");
}

/// `expire_active` is idempotent: calling it twice does not error.
#[tokio::test]
async fn expire_active_is_idempotent() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("expire-target-idem");
    let rec = make_record("idempotent expire");

    store
        .with_apply_tx(test_apply_token(), {
            let r = rec.clone();
            let t = target.clone();
            move |tx| {
                tx.stage_version(&t, &r)?;
                tx.activate_version(&t, 1, None)?;
                Ok(())
            }
        })
        .await
        .expect("stage+activate");

    let past = Rfc3339Timestamp::parse("2000-01-01T00:00:00Z").expect("valid past");

    // First expire.
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let exp = past.clone();
            move |tx| {
                tx.expire_active(&t, exp)?;
                Ok(())
            }
        })
        .await
        .expect("first expire");

    // Second expire — no-op by COALESCE; must not error.
    store
        .with_apply_tx(test_apply_token(), {
            let t = target.clone();
            let exp = past.clone();
            move |tx| {
                tx.expire_active(&t, exp)?;
                Ok(())
            }
        })
        .await
        .expect("second expire (idempotent)");
}
