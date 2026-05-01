//! Integration tests for the at-rest author-lifecycle lint check
//! (issue #256, brief §1223).
//!
//! These tests pair the real `SqliteMemoryStore` with the real
//! `SqliteIdentityRegistry` so the pure check fn in
//! `cairn_core::pipeline::lint::author_lifecycle` is exercised against
//! the same adapters production will use. The `scan_vault` helper
//! mirrors the registry-lookup the cli `lint_handler` does and feeds
//! `check_author_lifecycle` directly — useful for fast unit-shape
//! assertions on the pure fn. The end-of-file `run_checks_emits_*`
//! tests instead drive `cairn_core::verbs::lint::run_checks` to prove
//! the wired dispatch produces the right `BrokenActorChain` wire-shape
//! findings.
//!
//! Corner cases covered:
//!
//! - Clean baseline (no findings).
//! - Mixed lifecycle states in one vault.
//! - Visibility level matters (`Audit` surfaces purged, `Operational`
//!   hides them).
//! - Live revocation after a record was written under `Active`.
//! - Empty vault.
//! - Single revocation cascading across many records by the same
//!   author.
//!
//! Chain-shape corner cases (no-Author, duplicate-Author,
//! role-ordering) are exhaustively covered by the unit tests in
//! `cairn-core/src/pipeline/lint/author_lifecycle.rs`. Reproducing
//! them here would require bypassing `MemoryStore::upsert`'s
//! validation via raw SQL — duplicative without buying real coverage.

use cairn_core::contract::identity_registry::{IdentityRegistry, IdentityVisibility};
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::Rfc3339Timestamp;
use cairn_core::domain::identity::keys::{
    IdentityRevision, KeyVersion, SigningKey, VaultId, WitnessHash,
};
use cairn_core::domain::identity::receipts::{ReceiptOpKind, ReceiptPayload, RevocationReceipt};
use cairn_core::domain::identity::records::{
    IdentityKeyEntry, ProvisioningState, PublicIdentityRecord, ReceiptId,
};
use cairn_core::domain::{ChainRole, Identity, IdentityKind, MemoryRecord, RecordId, TargetId};
use cairn_core::pipeline::lint::author_lifecycle::{
    AuthorLifecycle, AuthorLifecycleFinding, AuthorState, ChainStatus, check_author_lifecycle,
};
use cairn_store_sqlite::{SqliteIdentityRegistry, SqliteMemoryStore, open_in_memory};

/// Open a store + registry pair that share no state. Production wires
/// them onto the same `SQLite` file via separate adapters; for tests an
/// in-memory pair is fine.
async fn setup() -> (SqliteMemoryStore, SqliteIdentityRegistry, tempfile::TempDir) {
    let store = open_in_memory().await.expect("open store");
    let registry = SqliteIdentityRegistry::open_in_memory().expect("open registry");

    // First-bind the registry so subsequent reserves/activates work.
    let dir = tempfile::tempdir().expect("tempdir");
    let binding = dir.path().join("vault.binding.pending");
    let witness = vec![1u8; 32];
    std::fs::write(&binding, &witness).expect("write witness");
    let hash = WitnessHash::from_witness(&witness);
    let vault = VaultId::mint();
    let first = Identity::parse("hmn:bootstrap").expect("valid");
    let (rec, key) = make_identity_record(&first, &SigningKey::generate(&mut rand_core::OsRng));
    registry
        .reserve_first_identity(&vault, &rec, &key, hash, &binding)
        .await
        .expect("reserve_first_identity");
    registry
        .activate_identity(&first, KeyVersion::FIRST)
        .await
        .expect("activate bootstrap");

    (store, registry, dir)
}

fn make_identity_record(
    id: &Identity,
    sk: &SigningKey,
) -> (PublicIdentityRecord, IdentityKeyEntry) {
    let now = chrono::Utc::now();
    let rec = PublicIdentityRecord {
        id: id.clone(),
        kind: IdentityKind::Human,
        current_key_version: KeyVersion::FIRST,
        revision: IdentityRevision::FIRST,
        provisioning_state: ProvisioningState::Pending,
        created_at: now,
        activated_at: None,
        revoked_at: None,
        purge_requested_at: None,
        purged_at: None,
    };
    let key = IdentityKeyEntry {
        identity_id: id.clone(),
        key_version: KeyVersion::FIRST,
        public_key: sk.verifying_key().to_bytes(),
        signed_predecessor: None,
        created_at: now,
        superseded_at: None,
    };
    (rec, key)
}

/// Reserve and activate an identity in the registry. Returns the
/// signing key so callers can issue revocation receipts later.
async fn provision_active(registry: &SqliteIdentityRegistry, id: &Identity) -> SigningKey {
    let sk = SigningKey::generate(&mut rand_core::OsRng);
    let (rec, key) = make_identity_record(id, &sk);
    registry
        .reserve_identity(&rec, &key)
        .await
        .expect("reserve_identity");
    registry
        .activate_identity(id, KeyVersion::FIRST)
        .await
        .expect("activate_identity");
    sk
}

/// Drive an active identity through `begin_revocation` →
/// `finalise_revocation`, leaving it in `ProvisioningState::Revoked`.
async fn revoke(registry: &SqliteIdentityRegistry, id: &Identity, sk: &SigningKey) {
    let payload = ReceiptPayload {
        op_kind: ReceiptOpKind::Revocation,
        target: id.clone(),
        signer: id.clone(),
        signer_key_version: KeyVersion::FIRST,
        old_key_version: Some(KeyVersion::FIRST),
        new_key_version: None,
        issued_at: chrono::Utc::now(),
    };
    let sig = payload.sign(sk).expect("sign");
    let receipt = RevocationReceipt {
        id: ReceiptId(0),
        payload,
        signature: sig.to_bytes().to_vec(),
        pending_key_disable: true,
    };
    registry
        .begin_revocation(&receipt)
        .await
        .expect("begin_revocation");
    registry
        .finalise_revocation(id)
        .await
        .expect("finalise_revocation");
}

/// Build a valid `MemoryRecord` authored by `author_id`. Adjusts the
/// three places where the sample fixture binds to its default author
/// so domain validation still passes: the chain author, the
/// `provenance.originating_agent_id`, and `scope.user`.
fn record_authored_by(author_id: &Identity, ulid: &str) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    r.id = RecordId::parse(ulid).expect("valid record id");
    r.target_id = TargetId::parse(ulid).expect("valid target id");
    r.scope.user = Some(author_id.as_str().to_owned());
    r.provenance.originating_agent_id = author_id.clone();
    // Stamp every chain entry + record updated_at "now" so the §6.2
    // timestamp comparison against `activated_at` doesn't mis-flag the
    // record as a pre-activation write — the test setup activates the
    // identity immediately before calling this helper, so "now" is
    // reliably after `activated_at`. Record validators also require
    // chain `at` <= updated_at, so both move together.
    let now = Rfc3339Timestamp::parse(chrono::Utc::now().to_rfc3339()).expect("now");
    r.provenance.created_at = now.clone();
    r.updated_at = now.clone();
    for entry in &mut r.actor_chain {
        if entry.role == ChainRole::Author {
            entry.identity = author_id.clone();
        }
        entry.at = now.clone();
    }
    r.validate()
        .expect("record_authored_by must build a valid record");
    r
}

/// Prototype dispatch-shape harness: walk active records, look up each
/// author identity in the registry under the given visibility, and
/// run the pure check fn. Mirrors what #96 will do inside the lint
/// verb. Visibility is parameterized so tests can pin the operator
/// failure mode of using `Operational` instead of `Audit`.
async fn scan_vault(
    store: &SqliteMemoryStore,
    registry: &SqliteIdentityRegistry,
    visibility: IdentityVisibility,
) -> Vec<AuthorLifecycleFinding> {
    let stored = store
        .list_active_stored(&ListArgs::default())
        .await
        .expect("list_active_stored");

    let mut out = Vec::new();
    for s in stored {
        let author = s
            .record
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
            .map(|e| e.identity.clone());

        let state = match author.as_ref() {
            Some(id) => match registry
                .get_identity(id, visibility)
                .await
                .expect("registry")
            {
                Some(rec) => AuthorState::Resolved(AuthorLifecycle {
                    state: rec.provisioning_state,
                    activated_at: rec
                        .activated_at
                        .and_then(|t| Rfc3339Timestamp::parse(t.to_rfc3339()).ok()),
                    revoked_at: rec
                        .revoked_at
                        .and_then(|t| Rfc3339Timestamp::parse(t.to_rfc3339()).ok()),
                }),
                None => AuthorState::MissingFromRegistry,
            },
            // No author entry in the chain — pass any state; the check
            // fn fails on chain shape before consulting it.
            None => AuthorState::MissingFromRegistry,
        };

        if let Some(f) = check_author_lifecycle(&s.record, state) {
            out.push(f);
        }
    }
    out
}

// ── Tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn empty_vault_yields_no_findings() {
    let (store, registry, _dir) = setup().await;
    let findings = scan_vault(&store, &registry, IdentityVisibility::Audit).await;
    assert!(findings.is_empty(), "empty vault: {findings:?}");
}

#[tokio::test]
async fn clean_active_record_yields_no_finding() {
    let (store, registry, _dir) = setup().await;

    let alice = Identity::parse("hmn:alice").expect("valid");
    provision_active(&registry, &alice).await;
    store
        .upsert(&record_authored_by(&alice, "01HQZX9F5N0000000000000001"))
        .await
        .expect("upsert");

    let findings = scan_vault(&store, &registry, IdentityVisibility::Audit).await;
    assert!(
        findings.is_empty(),
        "active author should not flag: {findings:?}"
    );
}

#[tokio::test]
async fn mixed_lifecycle_states_in_one_vault() {
    let (store, registry, _dir) = setup().await;

    // Three identities: one stays Active, one gets Revoked, one is
    // never registered (missing-from-registry case).
    let alice = Identity::parse("hmn:alice").expect("valid");
    let bob = Identity::parse("hmn:bob").expect("valid");
    let mallory = Identity::parse("hmn:mallory").expect("valid");

    provision_active(&registry, &alice).await;
    let bob_sk = provision_active(&registry, &bob).await;
    revoke(&registry, &bob, &bob_sk).await;
    // mallory is never reserved — registry will return None.

    let r_alice = record_authored_by(&alice, "01HQZX9F5N0000000000000010");
    let r_bob = record_authored_by(&bob, "01HQZX9F5N0000000000000011");
    let r_mallory = record_authored_by(&mallory, "01HQZX9F5N0000000000000012");

    for r in [&r_alice, &r_bob, &r_mallory] {
        store.upsert(r).await.expect("upsert");
    }

    let findings = scan_vault(&store, &registry, IdentityVisibility::Audit).await;
    assert_eq!(
        findings.len(),
        2,
        "expected bob + mallory only: {findings:?}"
    );

    // Bob's record is written *after* his revocation (the revoke
    // happens above before record_authored_by stamps "now"), so the
    // chain timestamp is at-or-after `revoked_at` → the timestamp
    // comparison upgrades this from legitimate-history to a real
    // trust violation.
    let bob_finding = findings
        .iter()
        .find(|f| f.record_id == r_bob.id)
        .expect("bob flagged");
    assert_eq!(bob_finding.status, ChainStatus::PostRevocationWrite);

    let mallory_finding = findings
        .iter()
        .find(|f| f.record_id == r_mallory.id)
        .expect("mallory flagged");
    assert_eq!(mallory_finding.status, ChainStatus::Malformed);

    assert!(
        findings.iter().all(|f| f.record_id != r_alice.id),
        "alice (active) must not be flagged"
    );
}

#[tokio::test]
async fn audit_visibility_required_for_revoked_author() {
    // After finalise_revocation, alice is in `Revoked` lifecycle. Per
    // ProvisioningState::is_operational, `Revoked` IS operational
    // (historical sigs verify) so it surfaces under both Operational
    // and Audit. To pin the visibility-matters-for-dispatch failure
    // mode we use a state only Audit exposes: the Pending bootstrap
    // identity, which we author a record under directly.
    let (store, registry, _dir) = setup().await;

    // hmn:bootstrap was first-bound and activated, but make a NEW
    // pending identity to test visibility filtering.
    let nascent = Identity::parse("hmn:nascent").expect("valid");
    let sk = SigningKey::generate(&mut rand_core::OsRng);
    let (rec, key) = make_identity_record(&nascent, &sk);
    registry
        .reserve_identity(&rec, &key)
        .await
        .expect("reserve");
    // Deliberately do NOT activate — leaves identity in Pending.

    let r = record_authored_by(&nascent, "01HQZX9F5N0000000000000020");
    store.upsert(&r).await.expect("upsert");

    // Audit visibility surfaces Pending → Malformed finding.
    let audit = scan_vault(&store, &registry, IdentityVisibility::Audit).await;
    assert_eq!(audit.len(), 1, "audit must surface pending: {audit:?}");
    assert_eq!(audit[0].status, ChainStatus::Malformed);

    // Operational visibility hides Pending — registry returns None →
    // dispatch maps to MissingFromRegistry → still flagged as
    // Malformed but with a misleading message. This pins WHY dispatch
    // must use Audit: the verdict is the same here, but only because
    // both states map to Malformed; for Revoked-only states using
    // Operational vs Audit changes the verdict.
    let operational = scan_vault(&store, &registry, IdentityVisibility::Operational).await;
    assert_eq!(operational.len(), 1);
    assert!(
        operational[0]
            .message
            .contains("no row in IdentityRegistry"),
        "operational misclassifies Pending as missing-from-registry: {}",
        operational[0].message
    );
    assert!(
        audit[0].message.contains("Pending"),
        "audit correctly identifies Pending: {}",
        audit[0].message
    );
}

#[tokio::test]
async fn revocation_after_write_now_flags_record() {
    let (store, registry, _dir) = setup().await;

    let alice = Identity::parse("hmn:alice").expect("valid");
    let alice_sk = provision_active(&registry, &alice).await;
    let r = record_authored_by(&alice, "01HQZX9F5N0000000000000030");
    store.upsert(&r).await.expect("upsert");

    // Pre-revocation: clean.
    let pre = scan_vault(&store, &registry, IdentityVisibility::Audit).await;
    assert!(pre.is_empty(), "pre-revocation must be clean: {pre:?}");

    // Revoke alice.
    revoke(&registry, &alice, &alice_sk).await;

    // Post-revocation: record is flagged.
    let post = scan_vault(&store, &registry, IdentityVisibility::Audit).await;
    assert_eq!(post.len(), 1);
    assert_eq!(post[0].record_id, r.id);
    assert_eq!(post[0].status, ChainStatus::Revoked);
}

#[tokio::test]
async fn post_revocation_write_emits_post_revocation_write_status() {
    // Round-9 fix (timestamp evidence): a record whose chain author
    // timestamp is at-or-after `revoked_at` is the suspicious-write
    // case. The check must escalate from `Revoked` (Warning) to
    // `PostRevocationWrite` (Error) so timestamp evidence does NOT
    // get downgraded to advisory output.
    let (store, registry, _dir) = setup().await;

    let alice = Identity::parse("hmn:alice").expect("valid");
    let alice_sk = provision_active(&registry, &alice).await;
    revoke(&registry, &alice, &alice_sk).await;
    // Now write a record under the (already-revoked) identity. The
    // chain timestamp will be "now" (after `revoked_at`), so the
    // check has the evidence it needs to escalate.
    let r = record_authored_by(&alice, "01HQZX9F5N0000000000000060");
    store.upsert(&r).await.expect("upsert");

    let findings = scan_vault(&store, &registry, IdentityVisibility::Audit).await;
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].record_id, r.id);
    assert_eq!(findings[0].status, ChainStatus::PostRevocationWrite);
}

// ── Dispatch-layer coverage (post-#96) ────────────────────────────

/// Prototype of the cairn-cli `lint_handler` registry-prefetch step:
/// gather unique chain-author identities, look each up under
/// `IdentityVisibility::Audit`, and assemble the
/// `LintInputs.author_states` map.
async fn prefetch_author_states(
    store: &SqliteMemoryStore,
    registry: &SqliteIdentityRegistry,
) -> std::collections::HashMap<Identity, AuthorLifecycle> {
    let stored = store
        .list_active_stored(&ListArgs::default())
        .await
        .expect("list");
    let mut unique: std::collections::HashSet<Identity> = std::collections::HashSet::new();
    for s in &stored {
        if let Some(e) = s
            .record
            .actor_chain
            .iter()
            .find(|e| e.role == ChainRole::Author)
        {
            unique.insert(e.identity.clone());
        }
    }
    let mut map = std::collections::HashMap::with_capacity(unique.len());
    for id in unique {
        if let Some(rec) = registry
            .get_identity(&id, IdentityVisibility::Audit)
            .await
            .expect("get_identity")
        {
            let lc = AuthorLifecycle {
                state: rec.provisioning_state,
                activated_at: rec
                    .activated_at
                    .and_then(|t| Rfc3339Timestamp::parse(t.to_rfc3339()).ok()),
                revoked_at: rec
                    .revoked_at
                    .and_then(|t| Rfc3339Timestamp::parse(t.to_rfc3339()).ok()),
            };
            map.insert(id, lc);
        }
    }
    map
}

#[tokio::test]
async fn run_checks_emits_broken_actor_chain_warning_for_revoked_author() {
    use cairn_core::config::CairnConfig;
    use cairn_core::contract::memory_store::IndexStats;
    use cairn_core::generated::verbs::lint::{Kind, Severity};
    use cairn_core::verbs::lint::{
        ConsentModel, LintInputs, LintRecord, SchemaVersion, run_checks,
    };

    let (store, registry, _dir) = setup().await;

    let alice = Identity::parse("hmn:alice").expect("valid");
    let alice_sk = provision_active(&registry, &alice).await;
    let r = record_authored_by(&alice, "01HQZX9F5N0000000000000050");
    store.upsert(&r).await.expect("upsert");
    revoke(&registry, &alice, &alice_sk).await;

    let stored = store
        .list_active_stored(&ListArgs::default())
        .await
        .expect("list");
    let lint_records: Vec<LintRecord> = stored
        .into_iter()
        .map(|s| LintRecord {
            stored: s,
            consent_model: ConsentModel::LegacyEvent,
        })
        .collect();
    let states = prefetch_author_states(&store, &registry).await;
    let cfg = CairnConfig::default();
    let inputs = LintInputs {
        records: &lint_records,
        config: &cfg,
        index_stats: IndexStats::new(lint_records.len() as u64, lint_records.len() as u64),
        schema_version: SchemaVersion { major: 0, minor: 1 },
        author_states: &states,
    };

    let data = run_checks(&inputs);

    let chain_warnings: Vec<_> = data
        .findings
        .iter()
        .filter(|f| {
            matches!(f.kind, Kind::BrokenActorChain) && matches!(f.severity, Severity::Warning)
        })
        .collect();
    assert_eq!(
        chain_warnings.len(),
        1,
        "expected exactly one BrokenActorChain warning: {data:?}"
    );
    let f = chain_warnings[0];
    assert!(f.target.is_some(), "finding must carry a record-id target");
    assert_eq!(f.tracking_issue, Some(256));
    assert!(f.suggested_fix.is_some());
    // Privacy invariant 9: messages must not embed body content.
    assert!(!f.message.contains(&r.body));
    // Persistent-issue resolution (rounds 5/7/8): terminal Revoked is
    // non-blocking; remediation steers away from destructive cleanup.
    assert!(
        f.suggested_fix
            .as_deref()
            .unwrap_or("")
            .contains("do NOT auto-tombstone"),
        "remediation must steer operators away from destructive cleanup",
    );

    // No BrokenActorChain Error severities for this scenario — terminal
    // revocation must not surface as a blocking finding (legitimate
    // pre-revocation history).
    let chain_errors: Vec<_> = data
        .findings
        .iter()
        .filter(|f| {
            matches!(f.kind, Kind::BrokenActorChain) && matches!(f.severity, Severity::Error)
        })
        .collect();
    assert!(
        chain_errors.is_empty(),
        "terminal revocation must not produce blocking errors: {chain_errors:?}"
    );

    // The deferred-info finding for #256 must NOT be present once
    // author_states is populated — the leaf has run for real.
    let deferred_for_256 = data
        .findings
        .iter()
        .any(|f| matches!(f.kind, Kind::DeferredCheck) && f.tracking_issue == Some(256));
    assert!(
        !deferred_for_256,
        "deferred-info finding must disappear once dispatch plumbs the registry"
    );
}

#[tokio::test]
async fn one_revocation_cascades_to_all_records_by_same_author() {
    let (store, registry, _dir) = setup().await;

    let alice = Identity::parse("hmn:alice").expect("valid");
    let alice_sk = provision_active(&registry, &alice).await;

    let ulids = [
        "01HQZX9F5N0000000000000040",
        "01HQZX9F5N0000000000000041",
        "01HQZX9F5N0000000000000042",
        "01HQZX9F5N0000000000000043",
    ];
    let mut ids = Vec::new();
    for u in ulids {
        let r = record_authored_by(&alice, u);
        ids.push(r.id.clone());
        store.upsert(&r).await.expect("upsert");
    }

    revoke(&registry, &alice, &alice_sk).await;

    let findings = scan_vault(&store, &registry, IdentityVisibility::Audit).await;
    assert_eq!(findings.len(), ulids.len(), "cascade: {findings:?}");
    for id in &ids {
        assert!(
            findings
                .iter()
                .any(|f| f.record_id == *id && f.status == ChainStatus::Revoked),
            "every record by alice must surface as Revoked"
        );
    }
}
