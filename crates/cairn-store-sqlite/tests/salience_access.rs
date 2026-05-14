//! Salience access tracking integration tests.

use cairn_core::contract::memory_store::{DecayPolicy, KeywordSearchArgs, MemoryStore};
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryRecord, RecordId, Rfc3339Timestamp,
    ScopeTuple, TargetId,
};
use cairn_store_sqlite::open_in_memory;

fn record(seed: char, salience: f32) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    let mut id = String::from("01HQZX9F5N000000000000000");
    id.push(seed.to_ascii_uppercase());
    r.id = RecordId::parse(id.clone()).expect("valid id");
    r.target_id = TargetId::parse(id).expect("valid target");
    r.body = format!("record {seed}");
    r.salience = salience;
    r
}

async fn grant_source_forget(store: &cairn_store_sqlite::SqliteMemoryStore, rec: &MemoryRecord) {
    let event = ConsentEvent {
        consent_id: ulid::Ulid::new().to_string(),
        kind: ConsentKind::Grant,
        actor: Identity::parse("hmn:test-operator").expect("identity"),
        subject: rec.provenance.source_hash.clone(),
        scope: rec.scope.canonical_wire(),
        op_id: None,
        sensor_id: None,
        payload: ConsentPayload::Decision {
            subject_code: "source_forget:auto_eviction".to_owned(),
            policy_code: Some("salience_decay".to_owned()),
        },
        decided_at: Rfc3339Timestamp::parse("2025-01-01T00:00:00Z").expect("timestamp"),
        expires_at: None,
    };
    store
        .with_tx(move |tx| {
            tx.append_consent_event(&event)?;
            Ok(())
        })
        .await
        .expect("grant source forget");
}

#[tokio::test]
async fn record_access_strengthens_salience_and_stamps_last_access() {
    let store = open_in_memory().await.expect("open");
    let rec = record('1', 0.5);
    store.upsert(&rec).await.expect("seed");

    let updates = store
        .record_access(std::slice::from_ref(&rec.id), 1_800_000, "search")
        .await
        .expect("record access");

    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].record_id, rec.id);
    assert!((updates[0].old_salience - 0.5).abs() < 0.000_001);
    assert!((updates[0].new_salience - 0.525).abs() < 0.000_001);
    assert_eq!(updates[0].last_accessed_at_ms, 1_800_000);

    let hydrated = store.get(&rec.id).await.expect("get").expect("record");
    assert!((hydrated.salience - 0.525).abs() < 0.000_001);
}

#[tokio::test]
async fn keyword_search_strengthens_returned_records() {
    let store = open_in_memory().await.expect("open");
    let mut rec = record('4', 0.5);
    rec.body = "issue313searchboost unique token".to_owned();
    store.upsert(&rec).await.expect("seed");

    let page = store
        .search_keyword(&KeywordSearchArgs {
            query: "issue313searchboost".to_owned(),
            filter: None,
            auth_scope: ScopeTuple::default(),
            visibility_allowlist: vec![MemoryVisibility::Private],
            limit: 10,
            cursor: None,
            with_explain: false,
        })
        .await
        .expect("search");

    assert_eq!(page.candidates.len(), 1);
    let hydrated = store.get(&rec.id).await.expect("get").expect("record");
    assert!(hydrated.salience > 0.5);
}

#[tokio::test]
async fn pin_record_excludes_record_from_decay() {
    let store = open_in_memory().await.expect("open");
    let rec = record('2', 1.0);
    store.upsert(&rec).await.expect("seed");
    store.pin_record(&rec.id, true).await.expect("pin");

    let outcome = store
        .decay_salience_batch(
            9_000_000_000_000,
            DecayPolicy {
                decay_rate: 0.05,
                eviction_threshold: 0.10,
                min_age_days: 30,
                batch_limit: 100,
            },
        )
        .await
        .expect("decay");

    assert_eq!(outcome.records_processed, 0);
    assert!(outcome.eviction_candidates.is_empty());
    let hydrated = store.get(&rec.id).await.expect("get").expect("record");
    assert!((hydrated.salience - 1.0).abs() < 0.000_001);
}

#[tokio::test]
async fn decay_returns_old_low_salience_eviction_candidate() {
    let store = open_in_memory().await.expect("open");
    let rec = record('3', 0.10);
    store.upsert(&rec).await.expect("seed");
    grant_source_forget(&store, &rec).await;

    let outcome = store
        .decay_salience_batch(
            9_000_000_000_000,
            DecayPolicy {
                decay_rate: 0.05,
                eviction_threshold: 0.10,
                min_age_days: 30,
                batch_limit: 100,
            },
        )
        .await
        .expect("decay");

    assert_eq!(outcome.records_processed, 1);
    assert_eq!(outcome.eviction_candidates.len(), 1);
    assert_eq!(outcome.eviction_candidates[0].record_id, rec.id);
}

#[tokio::test]
async fn decay_skips_eviction_candidate_without_consent_grant() {
    let store = open_in_memory().await.expect("open");
    let rec = record('5', 0.10);
    store.upsert(&rec).await.expect("seed");

    let outcome = store
        .decay_salience_batch(
            9_000_000_000_000,
            DecayPolicy {
                decay_rate: 0.05,
                eviction_threshold: 0.10,
                min_age_days: 30,
                batch_limit: 100,
            },
        )
        .await
        .expect("decay");

    assert_eq!(outcome.records_processed, 1);
    assert!(outcome.eviction_candidates.is_empty());
}
