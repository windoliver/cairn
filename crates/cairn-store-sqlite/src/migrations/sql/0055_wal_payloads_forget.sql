-- Migration 0055: widen wal_payloads.kind to allow forget_record.
-- Issue #58: record-level forget needs a durable payload row keyed by
-- operation_id, just like upsert and expire (added in 0053).

-- SQLite cannot ALTER a CHECK constraint in place; recreate the table.
CREATE TABLE wal_payloads_new (
  operation_id TEXT NOT NULL PRIMARY KEY
                    REFERENCES wal_ops(operation_id),
  kind         TEXT NOT NULL CHECK (kind IN ('upsert', 'expire', 'forget_record')),
  payload_json TEXT NOT NULL,
  created_at   INTEGER NOT NULL
);

INSERT INTO wal_payloads_new SELECT * FROM wal_payloads;

DROP TRIGGER IF EXISTS wal_payloads_kind_matches_wal;
DROP TRIGGER IF EXISTS wal_payloads_immutable;
DROP TRIGGER IF EXISTS wal_payloads_no_delete;
DROP TABLE wal_payloads;
ALTER TABLE wal_payloads_new RENAME TO wal_payloads;

CREATE TRIGGER wal_payloads_kind_matches_wal
  BEFORE INSERT ON wal_payloads
  FOR EACH ROW
  WHEN EXISTS (
    SELECT 1
      FROM wal_ops
     WHERE operation_id = NEW.operation_id
       AND kind IS NOT NEW.kind
  )
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads.kind must match wal_ops.kind');
END;

CREATE TRIGGER wal_payloads_immutable
  BEFORE UPDATE ON wal_payloads
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads rows are immutable');
END;

CREATE TRIGGER wal_payloads_no_delete
  BEFORE DELETE ON wal_payloads
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_payloads is append-only');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (55, '0055_wal_payloads_forget', '', strftime('%s','now') * 1000);
