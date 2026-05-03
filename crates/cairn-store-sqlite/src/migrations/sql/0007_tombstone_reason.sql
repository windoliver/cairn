-- Migration 0007: distinguish tombstone reasons.
-- Brief source: §5.6 (operation kinds), §10 (lifecycle).

ALTER TABLE records ADD COLUMN tombstone_reason TEXT;

CREATE INDEX records_tombstoned_reason_idx
  ON records(tombstone_reason)
  WHERE tombstoned = 1;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (7, '0007_tombstone_reason', '', strftime('%s','now') * 1000);
