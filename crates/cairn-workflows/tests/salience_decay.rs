#![allow(missing_docs)]

use cairn_core::config::SalienceConfig;
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryRecord, RecordId, Rfc3339Timestamp,
    TargetId,
};
use cairn_workflows::salience_decay::run_salience_decay;

fn record(seed: char, salience: f32) -> MemoryRecord {
    let mut r = cairn_core::domain::record::tests_export::sample_record();
    let id = format!("01HQZX9F5N000000000000000{seed}");
    r.id = RecordId::parse(id.clone()).expect("valid id");
    r.target_id = TargetId::parse(id).expect("valid target");
    r.body = format!("workflow decay record {seed}");
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
async fn salience_decay_forgets_old_unpinned_candidate() {
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let rec = record('1', 0.10);
    store.upsert(&rec).await.expect("seed");
    grant_source_forget(&store, &rec).await;

    let report = run_salience_decay(
        &store,
        9_000_000_000_000,
        &SalienceConfig {
            decay_rate: 0.05,
            eviction_threshold: 0.10,
            min_age_days: 30,
            batch_limit: 100,
        },
    )
    .await
    .expect("decay workflow");

    assert_eq!(report.evicted, 1);
    assert!(store.get(&rec.id).await.expect("get").is_none());
}

#[tokio::test]
async fn salience_decay_retains_pinned_candidate() {
    let store = cairn_store_sqlite::open_in_memory().await.expect("open");
    let rec = record('2', 0.10);
    store.upsert(&rec).await.expect("seed");
    grant_source_forget(&store, &rec).await;
    store.pin_record(&rec.id, true).await.expect("pin");

    let report = run_salience_decay(
        &store,
        9_000_000_000_000,
        &SalienceConfig {
            decay_rate: 0.05,
            eviction_threshold: 0.10,
            min_age_days: 30,
            batch_limit: 100,
        },
    )
    .await
    .expect("decay workflow");

    assert_eq!(report.evicted, 0);
    assert!(store.get(&rec.id).await.expect("get").is_some());
}
