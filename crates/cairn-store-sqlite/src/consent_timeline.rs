//! `ConsentLookup` impl for [`SqliteMemoryStore`] — Issue #253, brief §14.
//!
//! Reads the append-only `consent_timeline` table (migration 0032) and
//! hydrates rows into [`ConsentTimelineEvent`]s. The default
//! [`ConsentLookup::covering_grant`] impl on the trait already walks
//! `timeline()` and delegates to `CoveringGrant::resolve`, so we only
//! implement `timeline()` here.

use async_trait::async_trait;
use rusqlite::params;

use cairn_core::contract::consent_lookup::{ConsentLookup, ConsentLookupError};
use cairn_core::domain::consent_timeline::{ConsentTimelineEvent, ConsentTimelineEventKind};
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
}
