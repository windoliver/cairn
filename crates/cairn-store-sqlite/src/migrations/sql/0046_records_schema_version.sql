-- Migration 0046: per-record schema_version stamp for §6.4 stale_schema lint.
-- Brief sources: lint spec §6.4 (`docs/superpowers/specs/2026-04-30-lint-checks-design.md`).
-- Issue #258. Carries the major.minor of MemoryStore::CONTRACT_VERSION at the
-- moment a row was written; older rows surface as warnings/errors when the
-- host bumps its contract.

-- Existing rows pre-date the stamp; they are stored as NULL (the
-- "unstamped legacy" sentinel) rather than backfilled to a literal that
-- would race the host's current contract version: a vault that jumps a
-- release boundary would otherwise have every legacy row flagged stale
-- against an advanced host. The read path hydrates NULL columns to the
-- live `SchemaVersion::current()` so lint stays silent on legacy data
-- until a rewrite re-stamps each row with a real version.
ALTER TABLE records
  ADD COLUMN schema_version_major INTEGER;
ALTER TABLE records
  ADD COLUMN schema_version_minor INTEGER;

CREATE INDEX records_schema_version_idx
  ON records(schema_version_major, schema_version_minor)
  WHERE active = 1
    AND tombstoned = 0
    AND schema_version_major IS NOT NULL;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (46, '0046_records_schema_version', '', strftime('%s','now') * 1000);
