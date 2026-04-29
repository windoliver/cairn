-- Migration 0011: align records columns with the metadata filter DSL.
-- Brief sources: §5.1 (Read path filter narrowing), §8.0.d (filter DSL).
--
-- core::domain::filter::compile_filter emits SQL that references three
-- column names that do not yet exist as physical columns on `records`:
-- `provenance`, `extra_frontmatter`, and `tags`. The canonical body for
-- each lives inside `record_json` (provenance, extra_frontmatter) or in
-- the existing `tags_json` column. Adding VIRTUAL generated columns lets
-- the compiled filter target the same names without duplicating storage.
--
-- VIRTUAL keeps re-computation cheap on read and avoids the rewrite
-- semantics that STORED would force on every upsert; the FTS path joins
-- back to records anyway, so the per-row json_extract is fast enough.

ALTER TABLE records
  ADD COLUMN extra_frontmatter TEXT
  GENERATED ALWAYS AS (json_extract(record_json, '$.extra_frontmatter'))
  VIRTUAL;

ALTER TABLE records
  ADD COLUMN provenance TEXT
  GENERATED ALWAYS AS (json_extract(record_json, '$.provenance'))
  VIRTUAL;

ALTER TABLE records
  ADD COLUMN tags TEXT
  GENERATED ALWAYS AS (tags_json)
  VIRTUAL;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (11, '0011_filter_alignment', '', strftime('%s','now') * 1000);
