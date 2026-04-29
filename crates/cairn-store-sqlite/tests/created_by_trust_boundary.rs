// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Regression: `version_history` must report the trusted `created_by`
//! supplied to `stage_version` by the WAL executor — NOT a value sourced
//! from `record.actor_chain`, which is caller-controlled payload data
//! and forgeable. A caller with an `ApplyToken` must not be able to
//! misattribute writes to a different actor by stuffing their identity
//! into `actor_chain` before staging.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    MemoryStore,
    apply::MemoryStoreApply,
    types::{ChangeKind, HistoryEntry, TargetId},
};
use cairn_core::domain::{
    ChainRole, EvidenceVector, MemoryClass, MemoryKind, MemoryVisibility, Provenance,
    Rfc3339Timestamp, ScopeTuple,
    actor_chain::ActorChainEntry,
    actor_ref::ActorRef,
    identity::Identity,
    principal::Principal,
    record::{Ed25519Signature, MemoryRecord, RecordId},
};
use cairn_core::wal::test_apply_token;
use cairn_store_sqlite::SqliteMemoryStore;
use tempfile::tempdir;

fn record_authored_by(forged_author: &str) -> MemoryRecord {
    let forged = Identity::parse(forged_author).expect("valid identity");
    MemoryRecord {
        id: RecordId::parse("01HQZX9F5N0000000000000000").expect("valid"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Public,
        scope: ScopeTuple::default(),
        body: "forged content".to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
            originating_agent_id: forged.clone(),
            source_hash: format!("sha256:{}", "a".repeat(64)),
            consent_ref: "consent:01HQZ".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-22T14:05:11Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: forged,
            at: Rfc3339Timestamp::parse("2026-04-22T14:02:11Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "a".repeat(128))).expect("valid"),
        tags: vec![],
        extra_frontmatter: BTreeMap::new(),
    }
}

#[tokio::test]
async fn forged_actor_chain_cannot_override_trusted_created_by() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    // The record's actor_chain claims this is authored by `victim`.
    let record = record_authored_by("usr:victim");
    let target = TargetId(record.id.as_str().to_owned());

    // The WAL executor passes the *real* actor — `attacker` — independently.
    // The store must persist this trusted value, not the forged one.
    let trusted_actor = ActorRef::from("agt:attacker:opus-4-7:main:v1");

    store
        .with_apply_tx(test_apply_token(), {
            let record = record.clone();
            let target = target.clone();
            let trusted = trusted_actor.clone();
            move |tx| {
                tx.stage_version(&target, &record, &trusted)?;
                tx.activate_version(&target, 1, None)?;
                Ok(())
            }
        })
        .await
        .expect("apply_tx");

    let principal = Principal::system(&test_apply_token());
    let history = store
        .version_history(&principal, &target)
        .await
        .expect("version_history");

    let update_actor = history
        .iter()
        .find_map(|h| match h {
            HistoryEntry::Version(v) => v
                .events
                .iter()
                .find(|e| matches!(e.kind, ChangeKind::Update))
                .and_then(|e| e.actor.as_ref().map(ActorRef::as_str)),
            _ => None,
        })
        .expect("at least one Update event");

    assert_eq!(
        update_actor, "agt:attacker:opus-4-7:main:v1",
        "version_history must report the trusted WAL-supplied actor, \
         not the forged identity in record.actor_chain"
    );
    assert_ne!(
        update_actor, "usr:victim",
        "forged actor_chain must NOT bleed into the audit column"
    );
}
