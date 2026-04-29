// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! COW (copy-on-write) versioning: `version_history` must return one entry
//! per staged+activated version, with exactly one row marked active.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    MemoryStore,
    apply::MemoryStoreApply,
    types::{HistoryEntry, TargetId},
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

fn make_record(id_suffix: u8, body: &str) -> MemoryRecord {
    let user_id = Identity::parse("usr:cowtest").expect("valid");
    let id = format!("01HQZX9F5N000000000000000{id_suffix:X}");
    MemoryRecord {
        id: RecordId::parse(id).expect("valid"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:cowtest".to_owned()),
            ..ScopeTuple::default()
        },
        body: body.to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-22T10:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "b".repeat(64)),
            consent_ref: "consent:cow1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-22T10:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.6,
        confidence: 0.8,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-22T10:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "c".repeat(128))).expect("valid"),
        tags: vec!["cow".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

/// Stage v1 + v2 under the same target. Assert:
/// - `version_history` returns 2 `HistoryEntry::Version` entries.
/// - version numbers are 1 and 2.
/// - The active version is v2; v1 is superseded (active=false).
#[tokio::test]
async fn version_history_returns_two_entries_after_two_activations() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("cow-target-history");
    let v1 = make_record(0, "version one body");
    let v2 = make_record(1, "version two body");

    // Stage + activate v1.
    store
        .with_apply_tx(test_apply_token(), {
            let rec = v1.clone();
            let t = target.clone();
            move |tx| {
                tx.stage_version(&t, &rec, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 1, None)?;
                Ok(())
            }
        })
        .await
        .expect("stage+activate v1");

    // Stage + activate v2.
    store
        .with_apply_tx(test_apply_token(), {
            let rec = v2.clone();
            let t = target.clone();
            move |tx| {
                tx.stage_version(&t, &rec, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                tx.activate_version(&t, 2, Some(1))?;
                Ok(())
            }
        })
        .await
        .expect("stage+activate v2");

    // version_history (system principal sees all).
    let principal = Principal::system(&test_apply_token());
    let history = store
        .version_history(&principal, &target)
        .await
        .expect("version_history");

    assert_eq!(
        history.len(),
        2,
        "version_history must return exactly 2 entries; got: {history:?}"
    );

    // Both must be Version entries; extract them.
    let versions: Vec<_> = history
        .iter()
        .filter_map(|e| {
            if let HistoryEntry::Version(v) = e {
                Some(v)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(versions.len(), 2, "both entries must be Version variants");

    // Versions are ordered ascending by version number.
    assert_eq!(versions[0].version, 1, "first entry is version 1");
    assert_eq!(versions[1].version, 2, "second entry is version 2");

    // v1 is superseded (not active); v2 is active.
    assert!(!versions[0].active, "v1 must be superseded");
    assert!(versions[1].active, "v2 must be the active version");
}
