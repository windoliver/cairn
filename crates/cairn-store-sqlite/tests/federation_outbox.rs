//! Integration tests for the `SQLite` `FederationOutbox` adapter.
//!
//! Each test opens an in-memory `SqliteMemoryStore`, calls a
//! `FederationOutbox` method, and asserts both side effects (consent
//! journal row + job/upsert/tombstone) landed atomically.

use cairn_core::contract::federation_outbox::FederationOutbox;
use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, RetryPolicy};
use cairn_core::contract::memory_store::MemoryStore;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryVisibility, Rfc3339Timestamp,
};
use cairn_store_sqlite::open_in_memory;

// ─── helpers ───────────────────────────────────────────────────────────

/// Valid 26-char Crockford ULID for `link_id` fields.
const LINK_ULID: &str = "01HQZX9F5N0000000000000000";

fn federation_grant_event(consent_id: &str, link_id: &str) -> ConsentEvent {
    ConsentEvent {
        consent_id: consent_id.to_owned(),
        kind: ConsentKind::FederationGrant,
        actor: Identity::parse("hmn:federation").expect("identity"),
        subject: format!("share_link:{}", link_id.to_ascii_lowercase()),
        scope: "tenant=default".to_owned(),
        op_id: Some(format!("op-{consent_id}")),
        sensor_id: None,
        payload: ConsentPayload::FederationGrant {
            link_id: link_id.to_owned(),
            grant_tier: MemoryVisibility::Team,
            peer_code: "loopback-node-a".to_owned(),
            grantee_id_hash: None,
        },
        decided_at: Rfc3339Timestamp::parse("2026-05-21T12:00:00Z").expect("ts"),
        expires_at: None,
    }
}

fn federation_accept_event(consent_id: &str, link_id: &str) -> ConsentEvent {
    ConsentEvent {
        consent_id: consent_id.to_owned(),
        kind: ConsentKind::FederationAccept,
        actor: Identity::parse("hmn:federation").expect("identity"),
        subject: format!("share_link:{}", link_id.to_ascii_lowercase()),
        scope: "tenant=default".to_owned(),
        op_id: Some(format!("op-{consent_id}")),
        sensor_id: None,
        payload: ConsentPayload::FederationAccept {
            link_id: link_id.to_owned(),
            grant_tier: MemoryVisibility::Team,
            applied_id_hashes: Vec::new(),
        },
        decided_at: Rfc3339Timestamp::parse("2026-05-21T12:00:00Z").expect("ts"),
        expires_at: None,
    }
}

fn federation_revoke_event(consent_id: &str, link_id: &str) -> ConsentEvent {
    ConsentEvent {
        consent_id: consent_id.to_owned(),
        kind: ConsentKind::FederationRevoke,
        actor: Identity::parse("hmn:federation").expect("identity"),
        subject: format!("share_link:{}", link_id.to_ascii_lowercase()),
        scope: "tenant=default".to_owned(),
        op_id: Some(format!("op-{consent_id}")),
        sensor_id: None,
        payload: ConsentPayload::FederationRevoke {
            link_id: link_id.to_owned(),
            reason_code: "issuer_revoked".to_owned(),
        },
        decided_at: Rfc3339Timestamp::parse("2026-05-21T12:00:00Z").expect("ts"),
        expires_at: None,
    }
}

fn sample_enqueue_request(job_id: &str, dedupe_key: Option<&str>) -> EnqueueRequest {
    EnqueueRequest {
        job_id: JobId::new(job_id),
        kind: JobKind::new("federation.propagate.outbound_share"),
        payload: b"{}".to_vec(),
        queue_key: None,
        dedupe_key: dedupe_key.map(ToOwned::to_owned),
        not_before_ms: 0,
        retry: RetryPolicy::DEFAULT,
    }
}

// ─── record_share_grant ────────────────────────────────────────────────

#[tokio::test]
async fn record_share_grant_is_atomic() {
    let store = open_in_memory().await.expect("open");
    let event = federation_grant_event("cg-1", LINK_ULID);
    let job = sample_enqueue_request("job-grant-1", Some("dedupe-grant-1"));

    store.record_share_grant(&event, job).await.expect("grant");

    // Both consent row and job row exist.
    let conn = store.read_conn().expect("conn");
    let consent_id = event.consent_id.clone();
    let (consent_count, job_count): (i64, i64) = conn
        .call(move |c| {
            let consent: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM consent_journal WHERE consent_id = ?1",
                    [&consent_id],
                    |r| r.get(0),
                )
                .expect("query consent");
            let job: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM workflow_jobs WHERE job_id = ?1",
                    ["job-grant-1"],
                    |r| r.get(0),
                )
                .expect("query jobs");
            Ok((consent, job))
        })
        .await
        .expect("call");

    assert_eq!(consent_count, 1, "consent event not found");
    assert_eq!(job_count, 1, "workflow job not found");
}

#[tokio::test]
async fn record_share_grant_duplicate_dedupe_key() {
    let store = open_in_memory().await.expect("open");
    let event1 = federation_grant_event("cg-dup-1", LINK_ULID);
    let job1 = sample_enqueue_request("job-dup-1", Some("dedupe-dup"));
    store
        .record_share_grant(&event1, job1)
        .await
        .expect("first grant");

    // Second grant with same dedupe_key should return DuplicateJob.
    let event2 = federation_grant_event("cg-dup-2", LINK_ULID);
    let job2 = sample_enqueue_request("job-dup-2", Some("dedupe-dup"));
    let err = store
        .record_share_grant(&event2, job2)
        .await
        .expect_err("expected duplicate");

    assert!(
        matches!(
            err,
            cairn_core::contract::federation_outbox::FederationOutboxError::DuplicateJob
        ),
        "expected DuplicateJob, got: {err:?}"
    );
}

// ─── record_share_accept ───────────────────────────────────────────────

#[tokio::test]
async fn record_share_accept_upserts_and_consents_atomically() {
    let store = open_in_memory().await.expect("open");
    let event = federation_accept_event("ca-1", LINK_ULID);
    let records = vec![sample_record()];

    store
        .record_share_accept(&event, &records)
        .await
        .expect("accept");

    let conn = store.read_conn().expect("conn");
    let record_id = records[0].id.as_str().to_owned();
    let consent_id = event.consent_id.clone();

    let (consent_count, record_count): (i64, i64) = conn
        .call(move |c| {
            let consent: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM consent_journal WHERE consent_id = ?1",
                    [&consent_id],
                    |r| r.get(0),
                )
                .expect("query consent");
            let rec: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM records WHERE record_id = ?1",
                    [&record_id],
                    |r| r.get(0),
                )
                .expect("query records");
            Ok((consent, rec))
        })
        .await
        .expect("call");

    assert_eq!(consent_count, 1, "consent event not found");
    assert!(record_count >= 1, "record not found");
}

// ─── record_share_revoke ───────────────────────────────────────────────

#[tokio::test]
async fn record_share_revoke_tombstones_and_consents_atomically() {
    let store = open_in_memory().await.expect("open");

    // First insert a record to tombstone.
    let record = sample_record();
    store.upsert(&record).await.expect("upsert");

    let event = federation_revoke_event("cr-1", LINK_ULID);
    store
        .record_share_revoke(&event, &[record.id.as_str().to_owned()])
        .await
        .expect("revoke");

    let conn = store.read_conn().expect("conn");
    let consent_id = event.consent_id.clone();
    let rid = record.id.as_str().to_owned();

    let (consent_count, tombstoned): (i64, i64) = conn
        .call(move |c| {
            let consent: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM consent_journal WHERE consent_id = ?1",
                    [&consent_id],
                    |r| r.get(0),
                )
                .expect("query consent");
            let tomb: i64 = c
                .query_row(
                    "SELECT tombstoned FROM records WHERE record_id = ?1 AND active = 1",
                    [&rid],
                    |r| r.get(0),
                )
                .expect("query tombstoned");
            Ok((consent, tomb))
        })
        .await
        .expect("call");

    assert_eq!(consent_count, 1, "consent event not found");
    assert_eq!(tombstoned, 1, "record not tombstoned");
}

#[tokio::test]
async fn record_share_revoke_with_empty_tombstone_ids() {
    let store = open_in_memory().await.expect("open");
    let event = federation_revoke_event("cr-empty", LINK_ULID);

    // Empty tombstone_ids is valid — the consent event still lands.
    store
        .record_share_revoke(&event, &[])
        .await
        .expect("revoke empty");

    let conn = store.read_conn().expect("conn");
    let consent_id = event.consent_id.clone();

    let consent_count: i64 = conn
        .call(move |c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM consent_journal WHERE consent_id = ?1",
                [&consent_id],
                |r| r.get(0),
            )
            .expect("query consent"))
        })
        .await
        .expect("call");

    assert_eq!(consent_count, 1, "consent event not found");
}

// ─── record_share_revoke_grant ─────────────────────────────────────────

#[tokio::test]
async fn record_share_revoke_grant_is_atomic() {
    let store = open_in_memory().await.expect("open");
    let event = federation_revoke_event("crg-1", LINK_ULID);
    let job = sample_enqueue_request("job-rg-1", Some("dedupe-rg-1"));

    store
        .record_share_revoke_grant(&event, job)
        .await
        .expect("revoke_grant");

    let conn = store.read_conn().expect("conn");
    let consent_id = event.consent_id.clone();

    let (consent_count, job_count): (i64, i64) = conn
        .call(move |c| {
            let consent: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM consent_journal WHERE consent_id = ?1",
                    [&consent_id],
                    |r| r.get(0),
                )
                .expect("query consent");
            let job: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM workflow_jobs WHERE job_id = ?1",
                    ["job-rg-1"],
                    |r| r.get(0),
                )
                .expect("query jobs");
            Ok((consent, job))
        })
        .await
        .expect("call");

    assert_eq!(consent_count, 1, "consent event not found");
    assert_eq!(job_count, 1, "workflow job not found");
}

#[tokio::test]
async fn record_share_revoke_grant_duplicate_dedupe_key() {
    let store = open_in_memory().await.expect("open");
    let event1 = federation_revoke_event("crg-dup-1", LINK_ULID);
    let job1 = sample_enqueue_request("job-rgdup-1", Some("dedupe-rg-dup"));
    store
        .record_share_revoke_grant(&event1, job1)
        .await
        .expect("first revoke_grant");

    let event2 = federation_revoke_event("crg-dup-2", LINK_ULID);
    let job2 = sample_enqueue_request("job-rgdup-2", Some("dedupe-rg-dup"));
    let err = store
        .record_share_revoke_grant(&event2, job2)
        .await
        .expect_err("expected duplicate");

    assert!(
        matches!(
            err,
            cairn_core::contract::federation_outbox::FederationOutboxError::DuplicateJob
        ),
        "expected DuplicateJob, got: {err:?}"
    );
}
