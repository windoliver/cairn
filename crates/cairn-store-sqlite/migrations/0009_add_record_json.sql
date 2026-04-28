-- Migration 0009: add record_json column to records for full MemoryRecord
-- round-trip. The individual columns (body, provenance, etc.) are retained
-- for SQL filtering; record_json is the canonical round-trip form for reads.
--
-- Pre-0009 rows lack columns for `id`, `signature`, `tags`, and
-- `extra_frontmatter`, so a deterministic backfill into a valid
-- `MemoryRecord` is impossible. The read path treats legacy rows whose
-- `record_json` is NULL or unparseable as missing (logged) rather than
-- erroring; operators are expected to repopulate or drop those rows.
ALTER TABLE records ADD COLUMN record_json TEXT;
