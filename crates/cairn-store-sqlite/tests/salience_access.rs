//! Salience access tracking integration tests.

use cairn_core::contract::memory_store::{DecayPolicy, KeywordSearchArgs, ListArgs, MemoryStore};
use cairn_core::domain::taxonomy::MemoryVisibility;
use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryRecord, RecordId, Rfc3339Timestamp,
    ScopeTuple, TargetId,
};
use cairn_store_sqlite::open_in_memory;

async fn salience_row(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    record_id: &RecordId,
) -> (f64, f64, i64, Option<i64>) {
    let conn = store.raw_conn().expect("conn").clone();
    let id = record_id.as_str().to_owned();
    conn.call(move |c| {
        c.query_row(
            "SELECT salience, json_extract(record_json, '$.salience'), updated_at, last_accessed_at_ms \
             FROM records WHERE record_id = ?1",
            rusqlite::params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(Into::into)
    })
    .await
    .expect("salience row")
}

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
    let before = salience_row(&store, &rec.id).await;

    let updates = store
        .record_access(std::slice::from_ref(&rec.id), 1_800_000, "search")
        .await
        .expect("record access");

    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].record_id, rec.id);
    assert!((updates[0].old_salience - 0.5).abs() < 0.000_001);
    assert!((updates[0].new_salience - 0.525).abs() < 0.000_001);
    assert_eq!(updates[0].last_accessed_at_ms, 1_800_000);

    let after = salience_row(&store, &rec.id).await;
    assert!((after.0 - 0.525).abs() < 0.000_001);
    assert_eq!(
        after.1, before.1,
        "signed record_json salience must not be rewritten"
    );
    assert_eq!(after.2, before.2, "read access must not change updated_at");
    assert_eq!(after.3, Some(1_800_000));

    let hydrated = store.get(&rec.id).await.expect("get").expect("record");
    assert!((hydrated.salience - 0.525).abs() < 0.000_001);
    let listed = store
        .list(&ListArgs {
            record_ids: vec![rec.id.clone()],
            ..ListArgs::default()
        })
        .await
        .expect("list");
    assert_eq!(listed.records.len(), 1);
    assert!((listed.records[0].salience - 0.525).abs() < 0.000_001);
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
    let row = salience_row(&store, &rec.id).await;
    assert!(row.0 > 0.5);
    assert_eq!(
        row.1, 0.5,
        "keyword search must not rewrite signed record_json"
    );
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
async fn pin_record_rejects_superseded_versions() {
    let store = open_in_memory().await.expect("open");
    let rec = record('7', 1.0);
    let first = store.upsert(&rec).await.expect("seed");

    let mut updated = rec.clone();
    updated.body = "record 7 updated body".to_owned();
    let second = store.upsert(&updated).await.expect("supersede");
    assert_ne!(first.record_id, second.record_id);

    let err = store
        .pin_record(&first.record_id, true)
        .await
        .expect_err("superseded record must not be pinnable");
    let Some(err) = err.downcast_ref::<cairn_store_sqlite::StoreError>() else {
        panic!("expected sqlite store error, got {err}");
    };
    assert!(
        matches!(err, cairn_store_sqlite::StoreError::NotFound { .. }),
        "expected NotFound for superseded record, got {err:?}",
    );

    store
        .pin_record(&second.record_id, true)
        .await
        .expect("active successor remains pinnable");
}

#[tokio::test]
async fn decay_updates_hot_salience_without_rewriting_signed_json() {
    let store = open_in_memory().await.expect("open");
    let rec = record('6', 0.5);
    store.upsert(&rec).await.expect("seed");
    let before = salience_row(&store, &rec.id).await;

    let outcome = store
        .decay_salience_batch(
            9_000_000_000_000,
            DecayPolicy {
                decay_rate: 0.05,
                eviction_threshold: 0.01,
                min_age_days: 30,
                batch_limit: 100,
            },
        )
        .await
        .expect("decay");

    assert_eq!(outcome.records_processed, 1);
    let after = salience_row(&store, &rec.id).await;
    assert!(after.0 < before.0, "hot salience should decay");
    assert_eq!(
        after.1, before.1,
        "signed record_json salience must stay stable"
    );
    let hydrated = store.get(&rec.id).await.expect("get").expect("record");
    assert!(
        hydrated.salience < rec.salience,
        "get should overlay decayed hot salience"
    );
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
