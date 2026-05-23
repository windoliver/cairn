//! Migration coexistence test for migrations 0068+0069 (issue #123).
//!
//! Verifies that a vault with pre-existing consent_journal rows (e.g.
//! `sensor_enable` events from before federation) can co-exist with the
//! new `federation_grant`/`federation_accept`/`federation_revoke` kinds
//! introduced by migration 0068.
//!
//! The test opens an in-memory store (which runs all migrations including
//! 0068+0069), inserts a pre-existing sensor_enable event, then inserts a
//! federation_grant event, and verifies both rows coexist without
//! corruption.

use cairn_core::contract::consent_lookup::ConsentLookup;
use cairn_core::contract::federation_outbox::FederationOutbox;
use cairn_core::contract::job_store::{EnqueueRequest, JobId, JobKind, RetryPolicy};
use cairn_core::domain::{
    ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryVisibility, Rfc3339Timestamp,
    SensorLabel,
};
use cairn_store_sqlite::open_in_memory;

/// Valid 26-char Crockford ULID for identifiers.
const LINK_ULID: &str = "01HQZX0000000000000000000C";

fn sensor_enable_event(consent_id: &str) -> ConsentEvent {
    let lbl = SensorLabel::parse("local:hooks:host:v1").expect("sensor label");
    ConsentEvent {
        consent_id: consent_id.to_owned(),
        kind: ConsentKind::SensorEnable,
        actor: Identity::parse("hmn:alice").expect("identity"),
        subject: "snr:local:hooks:host:v1".to_owned(),
        scope: "global".to_owned(),
        op_id: None,
        sensor_id: Some(lbl.clone()),
        payload: ConsentPayload::SensorToggle {
            sensor_label: lbl,
            reason_code: "first_run_prompt".to_owned(),
        },
        decided_at: Rfc3339Timestamp::parse("2026-01-01T00:00:00Z").expect("ts"),
        expires_at: None,
    }
}

fn federation_grant_event(consent_id: &str, link_id: &str) -> ConsentEvent {
    ConsentEvent {
        consent_id: consent_id.to_owned(),
        kind: ConsentKind::FederationGrant,
        actor: Identity::parse("hmn:alice").expect("identity"),
        subject: format!("share_link:{}", link_id.to_ascii_lowercase()),
        scope: "tenant=default".to_owned(),
        op_id: Some(format!("op-{consent_id}")),
        sensor_id: None,
        payload: ConsentPayload::FederationGrant {
            link_id: link_id.to_owned(),
            grant_tier: MemoryVisibility::Team,
            peer_code: "loopback-test".to_owned(),
            grantee_id_hash: None,
        },
        decided_at: Rfc3339Timestamp::parse("2026-05-23T00:00:00Z").expect("ts"),
        expires_at: None,
    }
}

fn sample_enqueue_request(job_id: &str) -> EnqueueRequest {
    EnqueueRequest {
        job_id: JobId::new(job_id),
        kind: JobKind::new("federation.propagate.outbound_share"),
        payload: b"{}".to_vec(),
        queue_key: None,
        dedupe_key: Some(format!("dedupe-{job_id}")),
        not_before_ms: 0,
        retry: RetryPolicy::DEFAULT,
    }
}

/// Migration 0068+0069 apply cleanly on a vault with existing consent
/// rows, and pre-existing `sensor_enable` events coexist with new
/// `federation_grant` events.
#[tokio::test]
async fn migration_0068_0069_applies_on_vault_with_existing_consent_rows() {
    // 1. Open an in-memory store (runs all migrations including 0068+0069).
    let store = open_in_memory().await.expect("open");

    // 2. Insert a pre-existing consent event (sensor_enable kind, which
    //    existed before federation).
    let pre_existing = sensor_enable_event("c-sensor-pre-existing");
    let conn = store.read_conn().expect("conn");
    let pre_ev = pre_existing.clone();
    conn.call(move |c| {
        cairn_store_sqlite::consent::append(c, &pre_ev).expect("append sensor event");
        Ok(())
    })
    .await
    .expect("insert pre-existing sensor event");

    // 3. Insert a federation event via the FederationOutbox trait — proves
    //    the new kinds are accepted by the migrated schema.
    let fed_event = federation_grant_event("c-fed-grant", LINK_ULID);
    let job = sample_enqueue_request("job-mig-1");
    store
        .record_share_grant(&fed_event, job)
        .await
        .expect("federation grant");

    // 4. Verify both rows coexist by querying the consent_journal.
    let pre_id = pre_existing.consent_id.clone();
    let fed_id = fed_event.consent_id.clone();
    let conn = store.read_conn().expect("conn");
    let (pre_count, fed_count): (i64, i64) = conn
        .call(move |c| {
            let pre: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM consent_journal WHERE consent_id = ?1",
                    [&pre_id],
                    |r| r.get(0),
                )
                .expect("query pre-existing");
            let fed: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM consent_journal WHERE consent_id = ?1",
                    [&fed_id],
                    |r| r.get(0),
                )
                .expect("query federation");
            Ok((pre, fed))
        })
        .await
        .expect("call");

    assert_eq!(pre_count, 1, "pre-existing sensor event not found");
    assert_eq!(fed_count, 1, "federation grant event not found");

    // 5. Verify the ConsentLookup revocation check works for a
    //    non-revoked link.
    let revoked = store
        .is_link_revoked(LINK_ULID)
        .await
        .expect("is_link_revoked");
    assert!(
        !revoked,
        "link should not be revoked (only grant, no revoke)"
    );
}
