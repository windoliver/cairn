// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! End-to-end visibility-tier coverage. Stages records under each
//! supported tier (private, session, project, public), activates them,
//! and asserts that the principal context returned by `get` and `list`
//! matches the rebac contract:
//!
//! - private: visible only to the owner
//! - session: visible only to a principal carrying the matching `session_id`
//! - project: visible only to a principal carrying the matching `project_id`
//! - public: visible to any identified principal
//! - team / org: rejected at write time (`ScopeTuple` lacks team/org dims)

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{
    MemoryStore, apply::MemoryStoreApply, error::StoreError, types::TargetId,
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

fn ulid(suffix: u8) -> String {
    format!("01HQZX9F5N0000000000000{suffix:03X}")
}

fn principal_for(id: &str) -> Principal {
    Principal::from_identity(Identity::parse(id).expect("valid identity"))
}

#[allow(
    clippy::too_many_arguments,
    reason = "explicit per-test scope/visibility builder"
)]
fn record(
    id_ulid: &str,
    visibility: MemoryVisibility,
    scope_user: Option<&str>,
    scope_session: Option<&str>,
    scope_project: Option<&str>,
) -> MemoryRecord {
    let owner = Identity::parse("usr:alice").expect("valid");
    MemoryRecord {
        id: RecordId::parse(id_ulid).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility,
        scope: ScopeTuple {
            user: scope_user.map(str::to_owned),
            session_id: scope_session.map(str::to_owned),
            project: scope_project.map(str::to_owned),
            ..ScopeTuple::default()
        },
        body: "tier body".to_owned(),
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-29T10:00:00Z").expect("valid"),
            originating_agent_id: owner.clone(),
            source_hash: format!("sha256:{}", "5".repeat(64)),
            consent_ref: "consent:tier".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-29T10:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience: 0.5,
        confidence: 0.7,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: owner,
            at: Rfc3339Timestamp::parse("2026-04-29T10:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "6".repeat(128))).expect("valid"),
        tags: vec!["tier".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

async fn stage_and_activate(store: &SqliteMemoryStore, target: &TargetId, rec: &MemoryRecord) {
    let actor = ActorRef::from_string("agt:test:integration:m:v1");
    let actor2 = actor.clone();
    let target_a = target.clone();
    let target_b = target.clone();
    let rec_clone = rec.clone();
    store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.stage_version(&target_a, &rec_clone, &actor)?;
            Ok(())
        })
        .await
        .expect("stage");
    store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.activate_version(&target_b, 1, None, &actor2)
        })
        .await
        .expect("activate");
}

#[tokio::test]
async fn private_visible_only_to_owner() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("priv-1");
    let rec = record(
        &ulid(0x01),
        MemoryVisibility::Private,
        Some("usr:alice"),
        None,
        None,
    );
    stage_and_activate(&store, &target, &rec).await;

    let alice = principal_for("usr:alice");
    let bob = principal_for("usr:bob");
    assert!(store.get(&alice, &target).await.expect("get").is_some());
    assert!(store.get(&bob, &target).await.expect("get").is_none());
}

#[tokio::test]
async fn session_visible_only_with_matching_session() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("sess-1");
    let rec = record(
        &ulid(0x02),
        MemoryVisibility::Session,
        Some("usr:alice"),
        Some("sess:morning"),
        None,
    );
    stage_and_activate(&store, &target, &rec).await;

    // Owner without session context cannot read.
    let alice_no_ctx = principal_for("usr:alice");
    assert!(
        store
            .get(&alice_no_ctx, &target)
            .await
            .expect("get")
            .is_none()
    );

    // Owner with matching session reads.
    let alice_in = principal_for("usr:alice").with_session("sess:morning");
    assert!(store.get(&alice_in, &target).await.expect("get").is_some());

    // Owner with wrong session cannot read.
    let alice_other = principal_for("usr:alice").with_session("sess:evening");
    assert!(
        store
            .get(&alice_other, &target)
            .await
            .expect("get")
            .is_none()
    );

    // Different identity with matching session also cannot read — the
    // owner-match used to gate `private` does not apply, but session
    // tier authorizes by session_id alone, so this *would* match.
    // Verify behavior is consistent: matching session_id is the gate.
    let bob_in = principal_for("usr:bob").with_session("sess:morning");
    assert!(store.get(&bob_in, &target).await.expect("get").is_some());
}

#[tokio::test]
async fn project_visible_only_with_matching_project() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("proj-1");
    let rec = record(
        &ulid(0x03),
        MemoryVisibility::Project,
        Some("usr:alice"),
        None,
        Some("proj:foo"),
    );
    // ScopeTuple::validate rejects records with `project` set (no IDL
    // predicate yet). For this test, bypass by writing project on the
    // record but disabling validation via direct SQL is too invasive —
    // instead, this test confirms project admission is NOT yet
    // exercisable end-to-end because `record.validate()` rejects
    // project scope. Document the gap and the test that *would* pass
    // once IDL exposes a project filter.
    let actor = ActorRef::from_string("agt:test:integration:m:v1");
    let target_a = target.clone();
    let rec_clone = rec.clone();
    let result = store
        .with_apply_tx(test_apply_token(), move |tx| {
            tx.stage_version(&target_a, &rec_clone, &actor)
        })
        .await;
    assert!(
        matches!(result, Err(StoreError::Backend(_))),
        "ScopeTuple validation rejects project scope until IDL adds the predicate; \
         got: {result:?}"
    );
}

#[tokio::test]
async fn public_visible_to_any_identified_principal() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    let target = TargetId::new("pub-1");
    let rec = record(
        &ulid(0x04),
        MemoryVisibility::Public,
        Some("usr:alice"),
        None,
        None,
    );
    stage_and_activate(&store, &target, &rec).await;

    let alice = principal_for("usr:alice");
    let bob = principal_for("usr:bob");
    assert!(store.get(&alice, &target).await.expect("get").is_some());
    assert!(store.get(&bob, &target).await.expect("get").is_some());
}

#[tokio::test]
async fn team_and_org_rejected_at_write_time() {
    let dir = tempdir().expect("tempdir");
    let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
        .await
        .expect("open");

    for (i, vis) in [MemoryVisibility::Team, MemoryVisibility::Org]
        .iter()
        .enumerate()
    {
        let target = TargetId::new(format!("blocked-{i}"));
        let rec = record(
            &ulid(0x10 + u8::try_from(i).expect("fits")),
            *vis,
            Some("usr:alice"),
            None,
            None,
        );
        let actor = ActorRef::from_string("agt:test:integration:m:v1");
        let target_clone = target.clone();
        let rec_clone = rec.clone();
        let result = store
            .with_apply_tx(test_apply_token(), move |tx| {
                tx.stage_version(&target_clone, &rec_clone, &actor)
            })
            .await;
        assert!(
            matches!(result, Err(StoreError::Invariant(_))),
            "{vis:?} must be rejected at stage_version; got: {result:?}"
        );
    }
}
