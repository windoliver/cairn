// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! `ListQuery::kind_filter` must constrain `list()` to rows whose
//! taxonomy `kind` matches the requested string. Mixed-kind data
//! must not leak into the result set.

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

fn make_record(id_ulid: &str, body: &str, kind: MemoryKind) -> MemoryRecord {
    let user_id = Identity::parse("usr:kindtest").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Public,
        scope: ScopeTuple {
            user: Some("usr:kindtest".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T20:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "5".repeat(64)),
            consent_ref: "consent:kf1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T20:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.6,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T20:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "6".repeat(128))).expect("valid"),
        tags: vec!["kf".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

#[tokio::test]
async fn list_with_kind_filter_returns_only_matching_kind() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    for (target, ulid, kind, body) in [
        (
            TargetId::new("kf-user"),
            "01HQZX9F5N00000000000000B0",
            MemoryKind::User,
            "user-kind body",
        ),
        (
            TargetId::new("kf-feedback"),
            "01HQZX9F5N00000000000000B1",
            MemoryKind::Feedback,
            "feedback-kind body",
        ),
        (
            TargetId::new("kf-project"),
            "01HQZX9F5N00000000000000B2",
            MemoryKind::Project,
            "project-kind body",
        ),
    ] {
        let rec = make_record(ulid, body, kind);
        store
            .with_apply_tx(test_apply_token(), move |tx| {
                tx.stage_version(
                    &target,
                    &rec,
                    &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"),
                )?;
                tx.activate_version(
                    &target,
                    1,
                    None,
                    &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"),
                )?;
                Ok(())
            })
            .await
            .expect("stage+activate");
    }

    let principal = Principal::system(&test_apply_token());

    // No filter: all three rows.
    let all = store
        .list(&ListQuery::new(principal.clone()))
        .await
        .expect("list all");
    assert_eq!(all.rows.len(), 3, "without filter, all kinds visible");

    // Filter to user kind only.
    let mut q = ListQuery::new(principal.clone());
    q.kind_filter = Some("user".to_owned());
    let users = store.list(&q).await.expect("list user kind");
    assert_eq!(users.rows.len(), 1, "kind_filter must drop other kinds");
    assert_eq!(users.rows[0].kind, MemoryKind::User);

    // Filter to feedback kind.
    let mut q = ListQuery::new(principal);
    q.kind_filter = Some("feedback".to_owned());
    let feedback = store.list(&q).await.expect("list feedback kind");
    assert_eq!(feedback.rows.len(), 1);
    assert_eq!(feedback.rows[0].kind, MemoryKind::Feedback);
}
