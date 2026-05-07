-- Migration 0032: consent_timeline — ordered receipt events keyed by
-- (consent_ref, seq). Brief §14, Issue #253.
--
-- Each row is one transition for a covering grant: issued (the grant came
-- into force), expired (TTL elapsed via writer-emitted event), revoked
-- (user/operator pulled it). The lint `consent` check resolves the
-- *covering* grant for a record's provenance.consent_ref + sensor + scope
-- + created_at by walking this table; ingest writers (Phase-B, #255)
-- append rows here and stamp records.consent_model='receipt_timeline'.
--
-- `decided_at` / `expires_at` store the canonical RFC3339 wire form
-- (TEXT, sortable lexicographically when normalized to UTC). Whole-second
-- INTEGER columns would collapse `provenance.created_at`'s sub-second
-- precision and let lint mistakenly cover a record written before its
-- grant came into force.

CREATE TABLE consent_timeline (
  consent_ref   TEXT NOT NULL,
  seq           INTEGER NOT NULL,
  kind          TEXT NOT NULL CHECK (kind IN ('issued','expired','revoked')),
  sensor_id     TEXT NOT NULL,
  scope         TEXT NOT NULL,
  decided_at    TEXT NOT NULL,
  expires_at    TEXT,
  payload_json  TEXT NOT NULL DEFAULT '{}',
  PRIMARY KEY (consent_ref, seq)
);

CREATE INDEX consent_timeline_sensor_idx
  ON consent_timeline(sensor_id, decided_at);
CREATE INDEX consent_timeline_decided_idx
  ON consent_timeline(decided_at);

CREATE TRIGGER consent_timeline_immutable
  BEFORE UPDATE ON consent_timeline
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_timeline rows are immutable');
END;

CREATE TRIGGER consent_timeline_no_delete
  BEFORE DELETE ON consent_timeline
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'consent_timeline is append-only');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (32, '0032_consent_timeline', '', strftime('%s','now') * 1000);
