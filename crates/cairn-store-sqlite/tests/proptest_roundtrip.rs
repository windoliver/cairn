// Integration test files are not public API; doc-comments are not required.
#![allow(missing_docs)]

//! Property-based stage→activate→get round-trip.
//!
//! Generates `MemoryRecord`s with varying body bytes, salience, and
//! confidence and asserts that the read path returns the same values
//! the apply path persisted. Pinned to 32 cases to keep CI under a
//! second per run.

use std::collections::BTreeMap;

use cairn_core::contract::memory_store::{MemoryStore, apply::MemoryStoreApply, types::TargetId};
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
use proptest::prelude::*;

fn build_record(seed: u64, body: String, salience: f32, confidence: f32) -> MemoryRecord {
    let user_id = Identity::parse("usr:proptest").expect("valid");
    // ULIDs are exactly 26 chars from Crockford base32. Fix the prefix
    // (22 chars), encode the seed as 4 chars in [0-9A-Z\\{I,L,O,U}].
    let alphabet: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"; // 32 chars
    let mut suffix = [0u8; 4];
    let mut s = seed;
    for byte in &mut suffix {
        *byte = alphabet[usize::try_from(s % 32).expect("mod 32")];
        s /= 32;
    }
    // 22-char fixed prefix + 4-char varying suffix = 26 chars total.
    let id = format!(
        "01HQZX9F5N000000000000{}",
        std::str::from_utf8(&suffix).expect("valid utf8")
    );
    debug_assert_eq!(id.len(), 26, "ULID must be 26 chars");
    MemoryRecord {
        id: RecordId::parse(id).expect("valid ULID"),
        kind: MemoryKind::User,
        class: MemoryClass::Semantic,
        visibility: MemoryVisibility::Private,
        scope: ScopeTuple {
            user: Some("usr:proptest".to_owned()),
            ..ScopeTuple::default()
        },
        body,
        provenance: Provenance {
            source_sensor: Identity::parse("snr:local:hook:cc-session:v1").expect("valid"),
            created_at: Rfc3339Timestamp::parse("2026-04-23T19:00:00Z").expect("valid"),
            originating_agent_id: user_id.clone(),
            source_hash: format!("sha256:{}", "0".repeat(64)),
            consent_ref: "consent:pt1".to_owned(),
            llm_id_if_any: None,
        },
        updated_at: Rfc3339Timestamp::parse("2026-04-23T19:01:00Z").expect("valid"),
        evidence: EvidenceVector::default(),
        salience,
        confidence,
        actor_chain: vec![ActorChainEntry {
            role: ChainRole::Author,
            identity: user_id,
            at: Rfc3339Timestamp::parse("2026-04-23T19:00:00Z").expect("valid"),
        }],
        signature: Ed25519Signature::parse(format!("ed25519:{}", "4".repeat(128))).expect("valid"),
        tags: vec!["pt".to_owned()],
        extra_frontmatter: BTreeMap::new(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn stage_activate_get_roundtrip(
        // ULIDs are 32-char [0-9A-F]; build the body from any printable ASCII range.
        body in prop::string::string_regex("[ -~]{1,128}").expect("regex"),
        salience in 0.0_f32..=1.0,
        confidence in 0.0_f32..=1.0,
        seed in 0u64..u64::from(u32::MAX),
    ) {
        let body_clone = body.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        rt.block_on(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let store = SqliteMemoryStore::open(&dir.path().join("cairn.db"))
                .await
                .expect("open");

            let target = TargetId::new(format!("pt-{seed}"));
            let record = build_record(seed, body_clone, salience, confidence);

            let t = target.clone();
            let r = record.clone();
            store
                .with_apply_tx(test_apply_token(), move |tx| {
                    tx.stage_version(&t, &r, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                    tx.activate_version(&t, 1, None, &cairn_core::domain::actor_ref::ActorRef::from("agt:test:integration:m:v1"))?;
                    Ok(())
                })
                .await
                .expect("stage+activate");

            let principal = Principal::system(&test_apply_token());
            let got = store
                .get(&principal, &target)
                .await
                .expect("get")
                .expect("record present");
            prop_assert_eq!(got.body, record.body);
            prop_assert!((got.salience - record.salience).abs() < 1e-6);
            prop_assert!((got.confidence - record.confidence).abs() < 1e-6);
            Ok(())
        })?;
    }
}
