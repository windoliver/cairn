-- Migration 0055: allow record-forget payloads and body-scrub stubs.
-- Issue #58: forget_record recovery needs a body-free payload, and Phase B
-- must scrub body-bearing upsert/expire WAL retention without deleting audit rows.

DROP TRIGGER IF EXISTS wal_payloads_kind_matches_wal;
DROP TRIGGER IF EXISTS wal_payloads_immutable;
DROP TRIGGER IF EXISTS wal_payloads_no_delete;

ALTER TABLE wal_payloads RENAME TO wal_payloads_old;

CREATE TABLE wal_payloads (
  operation_id TEXT NOT NULL PRIMARY KEY
                    REFERENCES wal_ops(operation_id),
  kind         TEXT NOT NULL CHECK (kind IN ('upsert', 'expire', 'forget_record', 'purged')),
  payload_json TEXT NOT NULL,
  created_at   INTEGER NOT NULL
);

INSERT INTO wal_payloads(operation_id, kind, payload_json, created_at)
SELECT operation_id, kind, payload_json, created_at
  FROM wal_payloads_old;

DROP TABLE wal_payloads_old;

CREATE TRIGGER wal_payloads_kind_matches_wal
  BEFORE INSERT ON wal_payloads
  FOR EACH ROW
  WHEN NEW.kind <> 'purged'
   AND EXISTS (
    SELECT 1
      FROM wal_ops
     WHERE operation_id = NEW.operation_id
       AND kind IS NOT NEW.kind
  )
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads.kind must match wal_ops.kind');
END;

CREATE TRIGGER wal_payloads_scrub_only
  BEFORE UPDATE ON wal_payloads
  FOR EACH ROW
  WHEN NEW.operation_id IS NOT OLD.operation_id
    OR NEW.created_at IS NOT OLD.created_at
    OR NEW.kind <> 'purged'
    OR json_extract(NEW.payload_json, '$.type') <> 'purged'
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads updates are limited to purged scrub stubs');
END;

CREATE TRIGGER wal_payloads_no_delete
  BEFORE DELETE ON wal_payloads
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads is append-only');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (55, '0055_forget_record_payload_scrub', '', strftime('%s','now') * 1000);
