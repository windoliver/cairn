-- Migration 0054: explicit internal marker for pre-activation COW rows.
-- Issue #57: staged rows must stay invisible to public read/history APIs
-- until the WAL `primary.activate` step commits.

ALTER TABLE records
  ADD COLUMN cow_staged INTEGER NOT NULL DEFAULT 0 CHECK (cow_staged IN (0, 1));

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (54, '0054_records_cow_staging', '', strftime('%s','now') * 1000);
