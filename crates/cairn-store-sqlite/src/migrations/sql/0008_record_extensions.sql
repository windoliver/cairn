-- Migration 0008: extend records with the columns needed to persist
-- a full MemoryRecord (record_json source-of-truth) plus denormalized
-- hot columns used by ranking and filters.
-- Brief sources: §3 (records-in-SQLite), §4.2 (record fields), §6.5.

ALTER TABLE records ADD COLUMN record_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE records ADD COLUMN confidence REAL NOT NULL DEFAULT 0.0;
ALTER TABLE records ADD COLUMN salience REAL NOT NULL DEFAULT 0.0;
ALTER TABLE records ADD COLUMN target_id_explicit TEXT;
ALTER TABLE records ADD COLUMN tags_json TEXT NOT NULL DEFAULT '[]';

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (8, '0008_record_extensions', '', strftime('%s','now') * 1000);
