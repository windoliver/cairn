-- Migration 0053: durable body-bearing WAL payloads for record operations.
-- Issue #57: upsert and expire recovery need operation inputs after restart.

CREATE TABLE wal_payloads (
  operation_id TEXT NOT NULL PRIMARY KEY
                    REFERENCES wal_ops(operation_id),
  kind         TEXT NOT NULL CHECK (kind IN ('upsert', 'expire')),
  payload_json TEXT NOT NULL,
  created_at   INTEGER NOT NULL
);

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
  VALUES (53, '0053_wal_payloads', '', strftime('%s','now') * 1000);
