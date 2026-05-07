-- Migration 0006: drift-detection hardening.
--   * Rename schema_migrations.sql_blake3 -> sql_hash (algorithm-agnostic).
--   * Replace schema_migrations_immutable trigger with a stamp-once form
--     that allows exactly one '' -> hash transition on sql_hash.
--   * Add edges_updates_no_kind_flip to close the UPDATE-time bypass of
--     the INSERT predicate on `updates` edges.

ALTER TABLE schema_migrations RENAME COLUMN sql_blake3 TO sql_hash;

DROP TRIGGER schema_migrations_immutable;
CREATE TRIGGER schema_migrations_immutable
  BEFORE UPDATE ON schema_migrations
  FOR EACH ROW
  WHEN NEW.migration_id IS NOT OLD.migration_id
    OR NEW.name         IS NOT OLD.name
    OR NEW.applied_at   IS NOT OLD.applied_at
    OR NOT (OLD.sql_hash = '' AND length(NEW.sql_hash) > 0)
BEGIN
  SELECT RAISE(ABORT, 'schema_migrations rows are immutable (only `` -> hash on sql_hash allowed)');
END;

CREATE TRIGGER edges_updates_no_kind_flip
  BEFORE UPDATE ON edges
  FOR EACH ROW
  WHEN NEW.kind = 'updates'
   AND (
        OLD.kind IS NOT 'updates'
     OR NEW.src  IS NOT OLD.src
     OR NEW.dst  IS NOT OLD.dst
   )
BEGIN
  SELECT RAISE(ABORT, 'updates edge identity must be set at INSERT time and is immutable');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (6, '0006_drift_hardening', '', strftime('%s','now') * 1000);
