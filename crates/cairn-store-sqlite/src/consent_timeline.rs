//! `ConsentLookup` impl for [`SqliteMemoryStore`] — Issue #253, brief §14.
//!
//! Reads the append-only `consent_timeline` table (migration 0032) and
//! hydrates rows into [`ConsentTimelineEvent`]s. The default
//! [`ConsentLookup::covering_grant`] impl on the trait already walks
//! `timeline()` and delegates to `CoveringGrant::resolve`, so we only
//! implement `timeline()` here.
//!
//! ## Federation methods (T12, issue #123)
//!
//! Four additional methods override the default-`None` stubs:
//! - [`SqliteMemoryStore::find_federation_accept`] — dedup check on
//!   `(issuer_key_id, link_id)` for inbound propose envelopes.
//! - [`SqliteMemoryStore::is_link_revoked`] — fast revocation check.
//! - [`SqliteMemoryStore::find_share_link`] — issuer-side grant lookup,
//!   joins `consent_journal` to `workflow_jobs` to recover the
//!   `SignedShareLink` payload the propagation job carries.
//! - [`SqliteMemoryStore::find_revocation`] — idempotency lookup for
//!   `revoke_share`, same join pattern as `find_share_link`.
//!
//! ### Column mapping for federation kinds
//!
//! | `consent_journal` column | `federation_grant` | `federation_accept` | `federation_revoke` |
//! |------------------------|------------------|-------------------|-------------------|
//! | `actor`                | issuer identity  | issuer identity   | `hmn:federation`  |
//! | `subject`              | `share_link:<link_id>` | `share_link:<link_id>` | `share_link:<link_id>` |
//! | `op_id`                | `operation_id`   | `operation_id`    | NULL              |
//! | `payload_json.peer_code` | peer code     | —                 | —                 |
//! | `payload_json.applied_id_hashes` | — | salted record-id hashes | —          |
//!
//! `workflow_jobs.dedupe_key` equals `consent_journal.op_id` for the
//! grant/revoke rows, enabling the JOIN used by `find_share_link` and
//! `find_revocation`.

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};

use cairn_core::contract::consent_lookup::{
    ConsentLookup, ConsentLookupError, FederationAcceptRecord, StoredRevocation, StoredShareLink,
};
use cairn_core::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};
use cairn_core::domain::federation::DedupKey;
use cairn_core::domain::{Rfc3339Timestamp, SensorLabel};

use crate::store::SqliteMemoryStore;

#[async_trait]
impl ConsentLookup for SqliteMemoryStore {
    async fn timeline(
        &self,
        consent_ref: &str,
    ) -> Result<Vec<ConsentTimelineEvent>, ConsentLookupError> {
        let conn = self
            .require_conn("consent_lookup.timeline")
            .map_err(|e| ConsentLookupError::Backend {
                source: Box::new(e),
            })?
            .clone();
        let key = consent_ref.to_owned();
        conn.call(move |c| {
            let mut stmt = c.prepare(
                "SELECT consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at \
                   FROM consent_timeline \
                  WHERE consent_ref = ?1 \
                  ORDER BY seq ASC",
            )?;
            let rows = stmt
                .query_map(params![key], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            let mut out = Vec::with_capacity(rows.len());
            for (consent_ref, seq, kind_s, sensor_s, scope, decided_at_s, expires_at_s) in rows {
                let kind = match kind_s.as_str() {
                    "issued" => ConsentTimelineEventKind::Issued,
                    "expired" => ConsentTimelineEventKind::Expired,
                    "revoked" => ConsentTimelineEventKind::Revoked,
                    other => {
                        return Err(tokio_rusqlite::Error::Other(
                            format!("consent_timeline.kind unknown variant: {other}").into(),
                        ));
                    }
                };
                let sensor_id = SensorLabel::parse(sensor_s)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                let decided_at = Rfc3339Timestamp::parse(decided_at_s)
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                let expires_at = expires_at_s
                    .map(Rfc3339Timestamp::parse)
                    .transpose()
                    .map_err(|e| tokio_rusqlite::Error::Other(Box::new(e)))?;
                let seq_u64 = u64::try_from(seq).map_err(|_| {
                    tokio_rusqlite::Error::Other(
                        format!("consent_timeline.seq is negative: {seq}").into(),
                    )
                })?;
                out.push(ConsentTimelineEvent {
                    consent_ref,
                    seq: seq_u64,
                    kind,
                    sensor_id,
                    scope,
                    decided_at,
                    expires_at,
                });
            }
            Ok::<_, tokio_rusqlite::Error>(out)
        })
        .await
        .map_err(|e| ConsentLookupError::Backend {
            source: Box::new(e),
        })
    }

    /// Idempotency lookup for `accept_share` (brief §12.a, T12).
    ///
    /// Queries `consent_journal` for a `federation_accept` row whose
    /// `actor` equals `dedup.issuer_key_id` (the issuer's identity string)
    /// and `subject` equals `"share_link:<link_id>"`. Returns the first
    /// hit ordered by `decided_at DESC`, hydrating `applied_id_hashes`
    /// from `payload_json`.
    ///
    /// `applied_records` in the returned [`FederationAcceptRecord`] are the
    /// §14-salted record-id hashes that the original apply committed — not
    /// raw record ids.
    ///
    /// # Errors
    /// Returns [`ConsentLookupError::Backend`] on adapter I/O failure.
    async fn find_federation_accept(
        &self,
        dedup: DedupKey<'_>,
    ) -> Result<Option<FederationAcceptRecord>, ConsentLookupError> {
        let conn = self
            .require_conn("consent_lookup.find_federation_accept")
            .map_err(|e| ConsentLookupError::Backend {
                source: Box::new(e),
            })?
            .clone();
        let issuer = dedup.issuer_key_id.to_owned();
        let subject = format!("share_link:{}", dedup.link_id.to_ascii_lowercase());
        conn.call(move |c| {
            let result: Option<(String, String)> = {
                let mut stmt = c.prepare(
                    "SELECT cj.op_id, cj.payload_json \
                       FROM consent_journal cj \
                      WHERE cj.kind = 'federation_accept' \
                        AND cj.actor = ?1 \
                        AND cj.subject = ?2 \
                      ORDER BY cj.decided_at DESC \
                      LIMIT 1",
                )?;
                stmt.query_row(params![issuer, subject], |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        row.get::<_, String>(1)?,
                    ))
                })
                .optional()?
            };
            let Some((op_id, payload_json)) = result else {
                return Ok::<_, tokio_rusqlite::Error>(None);
            };
            let v: serde_json::Value = serde_json::from_str(&payload_json)
                .map_err(|e| tokio_rusqlite::Error::Other(e.into()))?;
            let applied_id_hashes: Vec<String> = v
                .get("applied_id_hashes")
                .and_then(serde_json::Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(ToOwned::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            // `link_id` is recovered from `op_id` (the subject would also
            // work, but op_id is the canonical share-link id stored by T8).
            let link_id = op_id;
            Ok(Some(FederationAcceptRecord {
                link_id,
                applied_records: applied_id_hashes,
            }))
        })
        .await
        .map_err(|e| ConsentLookupError::Backend {
            source: Box::new(e),
        })
    }

    /// Revocation check for `accept_share` (brief §12.a, T12).
    ///
    /// Returns `true` when any `federation_revoke` row exists in
    /// `consent_journal` whose `subject` equals `"share_link:<link_id>"`.
    ///
    /// # Errors
    /// Returns [`ConsentLookupError::Backend`] on adapter I/O failure.
    async fn is_link_revoked(&self, link_id: &str) -> Result<bool, ConsentLookupError> {
        let conn = self
            .require_conn("consent_lookup.is_link_revoked")
            .map_err(|e| ConsentLookupError::Backend {
                source: Box::new(e),
            })?
            .clone();
        let subject = format!("share_link:{}", link_id.to_ascii_lowercase());
        conn.call(move |c| {
            let found: Option<i64> = c
                .query_row(
                    "SELECT 1 FROM consent_journal \
                      WHERE kind = 'federation_revoke' \
                        AND subject = ?1 \
                      LIMIT 1",
                    params![subject],
                    |row| row.get(0),
                )
                .optional()?;
            Ok::<_, tokio_rusqlite::Error>(found.is_some())
        })
        .await
        .map_err(|e| ConsentLookupError::Backend {
            source: Box::new(e),
        })
    }

    /// Original share-link lookup for `revoke_share` (brief §12.a, T12).
    ///
    /// Joins `consent_journal` (`federation_grant`) to `workflow_jobs`
    /// (`federation.propagate.outbound_share`) via
    /// `consent_journal.op_id = workflow_jobs.dedupe_key` to recover the
    /// full `SignedShareLink` payload the propagation job carries.
    ///
    /// The `peer_code` field from `payload_json` is exposed as
    /// `StoredShareLink::peer` so the revocation propagation job routes the
    /// revoke to the same destination.
    ///
    /// # Errors
    /// Returns [`ConsentLookupError::Backend`] on adapter I/O failure.
    async fn find_share_link(
        &self,
        link_id: &str,
    ) -> Result<Option<StoredShareLink>, ConsentLookupError> {
        let conn = self
            .require_conn("consent_lookup.find_share_link")
            .map_err(|e| ConsentLookupError::Backend {
                source: Box::new(e),
            })?
            .clone();
        let subject = format!("share_link:{}", link_id.to_ascii_lowercase());
        conn.call(move |c| {
            let result: Option<(String, Vec<u8>)> = {
                let mut stmt = c.prepare(
                    "SELECT cj.payload_json, wj.payload \
                       FROM consent_journal cj \
                       JOIN workflow_jobs wj ON wj.dedupe_key = cj.op_id \
                      WHERE cj.kind = 'federation_grant' \
                        AND cj.subject = ?1 \
                        AND wj.kind = 'federation.propagate.outbound_share' \
                      LIMIT 1",
                )?;
                stmt.query_row(params![subject], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .optional()?
            };
            let Some((cj_payload_json, wj_payload)) = result else {
                return Ok::<_, tokio_rusqlite::Error>(None);
            };
            // Parse the outbound share payload wrapper (link + manifest)
            // from the workflow_jobs payload blob. The verb serializes
            // `{ "link": SignedShareLink, "manifest": [...] }`.
            let link: cairn_core::domain::sharing::SignedShareLink = {
                let wrapper: serde_json::Value = serde_json::from_slice(&wj_payload)
                    .map_err(|e| tokio_rusqlite::Error::Other(e.into()))?;
                if let Some(link_val) = wrapper.get("link") {
                    serde_json::from_value(link_val.clone())
                        .map_err(|e| tokio_rusqlite::Error::Other(e.into()))?
                } else {
                    // Fallback: try parsing as bare SignedShareLink (legacy payloads).
                    serde_json::from_value(wrapper)
                        .map_err(|e| tokio_rusqlite::Error::Other(e.into()))?
                }
            };
            // Extract the peer_code from the consent_journal payload.
            let cj_v: serde_json::Value = serde_json::from_str(&cj_payload_json)
                .map_err(|e| tokio_rusqlite::Error::Other(e.into()))?;
            let peer = cj_v
                .get("peer_code")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned);
            Ok(Some(StoredShareLink {
                link_id: link.link_id.clone(),
                payload: link.payload.clone(),
                peer,
            }))
        })
        .await
        .map_err(|e| ConsentLookupError::Backend {
            source: Box::new(e),
        })
    }

    /// Idempotency lookup for `revoke_share` (brief §12.a, T12).
    ///
    /// Joins `consent_journal` (`federation_revoke`) to `workflow_jobs`
    /// (`federation.propagate.outbound_revoke`) via
    /// `consent_journal.op_id = workflow_jobs.dedupe_key` to recover the
    /// `SignedRevocation` payload the propagation job carries.
    ///
    /// # Errors
    /// Returns [`ConsentLookupError::Backend`] on adapter I/O failure.
    async fn find_revocation(
        &self,
        link_id: &str,
    ) -> Result<Option<StoredRevocation>, ConsentLookupError> {
        let conn = self
            .require_conn("consent_lookup.find_revocation")
            .map_err(|e| ConsentLookupError::Backend {
                source: Box::new(e),
            })?
            .clone();
        let subject = format!("share_link:{}", link_id.to_ascii_lowercase());
        conn.call(move |c| {
            let result: Option<(Option<String>, Vec<u8>)> = {
                let mut stmt = c.prepare(
                    "SELECT cj.op_id, wj.payload \
                       FROM consent_journal cj \
                       JOIN workflow_jobs wj ON wj.dedupe_key = cj.op_id \
                      WHERE cj.kind = 'federation_revoke' \
                        AND cj.subject = ?1 \
                        AND wj.kind = 'federation.propagate.outbound_revoke' \
                      LIMIT 1",
                )?;
                stmt.query_row(params![subject], |row| {
                    Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Vec<u8>>(1)?))
                })
                .optional()?
            };
            let Some((op_id, wj_payload)) = result else {
                return Ok::<_, tokio_rusqlite::Error>(None);
            };
            let revocation: cairn_core::domain::federation::SignedRevocation =
                serde_json::from_slice(&wj_payload)
                    .map_err(|e| tokio_rusqlite::Error::Other(e.into()))?;
            Ok(Some(StoredRevocation {
                operation_id: op_id.unwrap_or_default(),
                revocation,
            }))
        })
        .await
        .map_err(|e| ConsentLookupError::Backend {
            source: Box::new(e),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_in_memory;

    #[tokio::test]
    async fn timeline_round_trips_through_sqlite_and_resolves_grant() {
        let store = open_in_memory().await.expect("open in-memory store");
        let conn = store
            .require_conn("test.seed")
            .expect("invariant: connected store")
            .clone();

        // Seed two rows: an issue at 2025-01-01 with expiry at 2026-01-01,
        // then a revoke at 2025-07-01 before the natural expiry.
        conn.call(|c| {
            c.execute_batch(
                "INSERT INTO consent_timeline \
                    (consent_ref, seq, kind, sensor_id, scope, decided_at, expires_at, payload_json) \
                  VALUES \
                    ('c:1', 1, 'issued',  'local:screen:h:v1', 'private', '2025-01-01T00:00:00.000000000Z', '2026-01-01T00:00:00.000000000Z', '{}'), \
                    ('c:1', 2, 'revoked', 'local:screen:h:v1', 'private', '2025-07-01T00:00:00.000000000Z', NULL,                              '{}')",
            )?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed");

        let events = store.timeline("c:1").await.expect("timeline ok");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, ConsentTimelineEventKind::Issued);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[0].scope, "private");
        assert!(events[0].expires_at.is_some());
        assert_eq!(events[1].kind, ConsentTimelineEventKind::Revoked);
        assert_eq!(events[1].seq, 2);
        assert!(events[1].expires_at.is_none());

        let sensor =
            SensorLabel::parse("local:screen:h:v1").expect("invariant: valid sensor label");
        // Pre-revoke instant: grant is in force.
        let pre =
            Rfc3339Timestamp::parse("2025-02-19T00:00:00Z").expect("invariant: valid timestamp");
        // Post-revoke instant: grant has been revoked.
        let post =
            Rfc3339Timestamp::parse("2025-10-09T00:00:00Z").expect("invariant: valid timestamp");

        assert!(
            store
                .covering_grant("c:1", &sensor, "private", &pre)
                .await
                .expect("invariant: covering_grant ok")
                .is_some(),
            "grant should cover pre-revoke instant",
        );
        assert!(
            store
                .covering_grant("c:1", &sensor, "private", &post)
                .await
                .expect("invariant: covering_grant ok")
                .is_none(),
            "grant should not cover post-revoke instant",
        );
    }

    #[tokio::test]
    async fn timeline_empty_for_unknown_consent_ref() {
        let store = open_in_memory().await.expect("open in-memory store");
        let events = store
            .timeline("c:does-not-exist")
            .await
            .expect("timeline ok");
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn timeline_returns_backend_when_store_unconnected() {
        let store = SqliteMemoryStore::default();
        let err = store
            .timeline("c:any")
            .await
            .expect_err("must fail on unconnected store");
        matches!(err, ConsentLookupError::Backend { .. })
            .then_some(())
            .expect("invariant: Backend variant");
    }

    // ── federation ConsentLookup tests (T12, issue #123) ──────────────────────

    use cairn_core::domain::{
        ConsentEvent, ConsentKind, ConsentPayload, Identity, MemoryVisibility,
    };

    /// Convenience: insert a minimal `workflow_jobs` row so the JOIN-based
    /// federation lookups have something to link to.
    fn insert_workflow_job(
        c: &rusqlite::Connection,
        job_id: &str,
        kind: &str,
        dedupe_key: &str,
        payload: &[u8],
    ) {
        c.execute(
            "INSERT INTO workflow_jobs \
              (job_id, kind, payload, state, attempts, delivery_count, \
               max_attempts, base_backoff_ms, backoff_multiplier, max_backoff_ms, \
               dedupe_key, next_run_at, enqueued_at, updated_at) \
             VALUES (?1, ?2, ?3, 'queued', 0, 0, 3, 100, 2, 60000, ?4, 0, 0, 0)",
            params![job_id, kind, payload, dedupe_key],
        )
        .expect("invariant: insert workflow_jobs row must succeed");
    }

    fn federation_accept_event(
        consent_id: &str,
        actor: &str,
        link_id: &str,
        op_id: &str,
        applied_id_hashes: Vec<String>,
    ) -> ConsentEvent {
        ConsentEvent {
            consent_id: consent_id.to_owned(),
            kind: ConsentKind::FederationAccept,
            actor: Identity::parse(actor).expect("invariant: valid actor"),
            subject: format!("share_link:{}", link_id.to_ascii_lowercase()),
            scope: "tenant=default".to_owned(),
            op_id: Some(op_id.to_owned()),
            sensor_id: None,
            payload: ConsentPayload::FederationAccept {
                link_id: op_id.to_owned(),
                grant_tier: MemoryVisibility::Team,
                applied_id_hashes,
            },
            decided_at: Rfc3339Timestamp::parse("2026-05-21T12:00:00Z")
                .expect("invariant: literal timestamp"),
            expires_at: None,
        }
    }

    /// Build a `federation_revoke` event.
    ///
    /// `link_id` is the full share-link id (e.g. `"share-<op_id>"`), used
    /// for the `subject`. `op_id_ulid` is the raw 26-char ULID (same as the
    /// link's `operation_id`), used in the payload's `link_id` field which
    /// the validator requires to be a 26-char Crockford base32 ULID.
    fn federation_revoke_event(
        consent_id: &str,
        link_id: &str,
        op_id_ulid: &str,
        journal_op_id: Option<&str>,
    ) -> ConsentEvent {
        ConsentEvent {
            consent_id: consent_id.to_owned(),
            kind: ConsentKind::FederationRevoke,
            actor: Identity::parse("hmn:federation").expect("invariant: valid actor"),
            subject: format!("share_link:{}", link_id.to_ascii_lowercase()),
            scope: "tenant=default".to_owned(),
            op_id: journal_op_id.map(ToOwned::to_owned),
            sensor_id: None,
            payload: ConsentPayload::FederationRevoke {
                // payload.link_id must be the raw 26-char ULID (operation_id),
                // not the "share-..." prefixed form.
                link_id: op_id_ulid.to_owned(),
                reason_code: "receiver-revoked".to_owned(),
            },
            decided_at: Rfc3339Timestamp::parse("2026-05-21T12:05:00Z")
                .expect("invariant: literal timestamp"),
            expires_at: None,
        }
    }

    fn federation_grant_event(
        consent_id: &str,
        actor: &str,
        link_id: &str,
        op_id: &str,
        peer_code: &str,
    ) -> ConsentEvent {
        ConsentEvent {
            consent_id: consent_id.to_owned(),
            kind: ConsentKind::FederationGrant,
            actor: Identity::parse(actor).expect("invariant: valid actor"),
            subject: format!("share_link:{}", link_id.to_ascii_lowercase()),
            scope: "tenant=default".to_owned(),
            op_id: Some(op_id.to_owned()),
            sensor_id: None,
            payload: ConsentPayload::FederationGrant {
                link_id: op_id.to_owned(),
                grant_tier: MemoryVisibility::Team,
                peer_code: peer_code.to_owned(),
                grantee_id_hash: None,
            },
            decided_at: Rfc3339Timestamp::parse("2026-05-21T11:00:00Z")
                .expect("invariant: literal timestamp"),
            expires_at: Some(
                Rfc3339Timestamp::parse("2026-05-21T13:00:00Z")
                    .expect("invariant: literal timestamp"),
            ),
        }
    }

    // Test constants: op_id is a 26-char uppercase Crockford base32 ULID.
    // link_id is "share-<op_id>" (the full canonical link identifier).
    // The DedupKey.link_id field stores the full link_id for the query;
    // payload.link_id fields must be the raw 26-char ULID (operation_id).

    const OP_10: &str = "01HQZX9F5N0000000000000010";
    const OP_11: &str = "01HQZX9F5N0000000000000011";
    const OP_12: &str = "01HQZX9F5N0000000000000012";
    const OP_13: &str = "01HQZX9F5N0000000000000013";
    const OP_14: &str = "01HQZX9F5N0000000000000014";

    fn link_id(op: &str) -> String {
        format!("share-{op}")
    }

    #[tokio::test]
    async fn find_federation_accept_none_before_any_event() {
        let store = open_in_memory().await.expect("open in-memory store");

        let dedup = DedupKey {
            issuer_key_id: "hmn:alice",
            link_id: &link_id(OP_10),
        };
        let result = store
            .find_federation_accept(dedup)
            .await
            .expect("lookup must not error on empty store");

        assert!(
            result.is_none(),
            "expected None before any event is written"
        );
    }

    #[tokio::test]
    async fn find_federation_accept_some_after_event() {
        let store = open_in_memory().await.expect("open in-memory store");

        let lk_id = link_id(OP_11);
        let actor = "hmn:alice";
        let applied = vec![
            "sha256:aabbccdd00000000000000000000000000000000000000000000000000000011".to_owned(),
        ];

        let conn = store
            .require_conn("test.seed")
            .expect("invariant: connected store")
            .clone();
        let event = federation_accept_event("cid-accept-11", actor, &lk_id, OP_11, applied.clone());
        conn.call(move |c| {
            crate::consent::append(c, &event)
                .map_err(|e| tokio_rusqlite::Error::Other(e.into()))?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed federation_accept");

        let dedup = DedupKey {
            issuer_key_id: actor,
            link_id: &lk_id,
        };
        let result = store
            .find_federation_accept(dedup)
            .await
            .expect("lookup must not error");

        let record = result.expect("expected Some after event is written");
        assert_eq!(
            record.applied_records, applied,
            "applied_records must match the hashes from the consent row",
        );
        // link_id in the record is the op_id stored in the accept row.
        assert_eq!(record.link_id, OP_11);
    }

    #[tokio::test]
    async fn is_link_revoked_flips_after_revoke_event() {
        let store = open_in_memory().await.expect("open in-memory store");

        let lk_id = link_id(OP_12);

        let before = store
            .is_link_revoked(&lk_id)
            .await
            .expect("lookup must not error on empty store");
        assert!(!before, "link must not be revoked before any event");

        let conn = store
            .require_conn("test.seed")
            .expect("invariant: connected store")
            .clone();
        let event = federation_revoke_event("cid-revoke-12", &lk_id, OP_12, None);
        conn.call(move |c| {
            crate::consent::append(c, &event)
                .map_err(|e| tokio_rusqlite::Error::Other(e.into()))?;
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed federation_revoke");

        let after = store
            .is_link_revoked(&lk_id)
            .await
            .expect("lookup must not error");
        assert!(after, "link must be marked revoked after event is written");
    }

    #[tokio::test]
    async fn find_share_link_returns_payload_via_join() {
        use cairn_core::domain::sharing::{ShareLinkPayload, SignedShareLink};
        use cairn_core::domain::{Ed25519Signature, ScopeTuple};

        let store = open_in_memory().await.expect("open in-memory store");

        let lk_id = link_id(OP_13);
        let actor = "hmn:alice";
        let peer_code = "loopback-node-b";

        // Build a minimal SignedShareLink to store as the workflow_jobs payload.
        let scope = ScopeTuple {
            tenant: Some("default".to_owned()),
            workspace: Some("vault-a".to_owned()),
            ..ScopeTuple::default()
        };
        let payload = ShareLinkPayload {
            operation_id: OP_13.to_owned(),
            nonce: "AAAAAAAAAAAAAAAAAAAAAA==".to_owned(),
            target_hash: format!("sha256:{}", "0".repeat(64)),
            target_id_hashes: Vec::new(),
            scope,
            grant_tier: MemoryVisibility::Team,
            grantee: None,
            issuer: Identity::parse(actor).expect("invariant: valid identity"),
            issued_at: Rfc3339Timestamp::parse("2026-05-21T11:00:00Z")
                .expect("invariant: literal timestamp"),
            expires_at: Rfc3339Timestamp::parse("2026-05-21T13:00:00Z")
                .expect("invariant: literal timestamp"),
            key_version: 1,
        };
        let link = SignedShareLink {
            link_id: lk_id.clone(),
            payload,
            signature: Ed25519Signature::parse(format!("ed25519:{}", "0".repeat(128)))
                .expect("invariant: sentinel signature"),
        };
        let job_payload = serde_json::to_vec(&link).expect("invariant: serialize link");

        let conn = store
            .require_conn("test.seed")
            .expect("invariant: connected store")
            .clone();
        let grant_event = federation_grant_event("cid-grant-13", actor, &lk_id, OP_13, peer_code);
        let job_payload_clone = job_payload.clone();
        conn.call(move |c| {
            crate::consent::append(c, &grant_event)
                .map_err(|e| tokio_rusqlite::Error::Other(e.into()))?;
            insert_workflow_job(
                c,
                "job-grant-13",
                "federation.propagate.outbound_share",
                OP_13,
                &job_payload_clone,
            );
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed grant + workflow_job");

        let result = store
            .find_share_link(&lk_id)
            .await
            .expect("lookup must not error");

        let stored = result.expect("expected Some after seeding grant + workflow_job");
        assert_eq!(stored.link_id, lk_id);
        assert_eq!(stored.payload.operation_id, OP_13);
        assert_eq!(stored.payload.grant_tier, MemoryVisibility::Team);
        assert_eq!(
            stored.peer.as_deref(),
            Some(peer_code),
            "peer_code must round-trip from payload_json",
        );
    }

    #[tokio::test]
    async fn find_revocation_returns_outcome_via_join() {
        use cairn_core::domain::federation::SignedRevocation;
        use cairn_core::generated::common::{
            Ed25519Signature as WireEd25519Signature, Identity as WireIdentity,
        };

        let store = open_in_memory().await.expect("open in-memory store");

        let lk_id = link_id(OP_14);
        // The journal op_id for the revoke job (distinct from the share link op).
        let revoke_op_id = "REVOKE01HQZX9F5N000000014";
        let actor = "hmn:alice";

        // Build a minimal SignedRevocation for the workflow_jobs payload.
        let revocation = SignedRevocation {
            issuer: WireIdentity(actor.to_owned()),
            key_version: 1,
            link_id: lk_id.clone(),
            revoked_at: "2026-05-21T12:05:00Z".to_owned(),
            signature: WireEd25519Signature(format!("ed25519:{}", "0".repeat(128))),
        };
        let job_payload = serde_json::to_vec(&revocation).expect("invariant: serialize revocation");

        let conn = store
            .require_conn("test.seed")
            .expect("invariant: connected store")
            .clone();
        // For the issuer-side revoke, op_id is set to the revoke operation id
        // so the JOIN links the consent_journal row to the workflow_jobs row.
        let revoke_event =
            federation_revoke_event("cid-revoke-14", &lk_id, OP_14, Some(revoke_op_id));
        let revoke_op_id_clone = revoke_op_id.to_owned();
        let job_payload_clone = job_payload.clone();
        conn.call(move |c| {
            crate::consent::append(c, &revoke_event)
                .map_err(|e| tokio_rusqlite::Error::Other(e.into()))?;
            insert_workflow_job(
                c,
                "job-revoke-14",
                "federation.propagate.outbound_revoke",
                &revoke_op_id_clone,
                &job_payload_clone,
            );
            Ok::<_, tokio_rusqlite::Error>(())
        })
        .await
        .expect("seed revoke + workflow_job");

        let result = store
            .find_revocation(&lk_id)
            .await
            .expect("lookup must not error");

        let stored = result.expect("expected Some after seeding revoke + workflow_job");
        assert_eq!(stored.operation_id, revoke_op_id);
        assert_eq!(stored.revocation.link_id, lk_id);
        assert_eq!(stored.revocation.issuer.0, actor);
    }
}
