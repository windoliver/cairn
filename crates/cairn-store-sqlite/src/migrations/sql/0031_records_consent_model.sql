-- Migration 0031: records.consent_model -- Phase-A per-row gate.
-- Brief sources: §14 (privacy / consent). Issue #253.
-- Purpose: tag each record with the consent storage model that authorized
-- it. Phase-A (this migration) adds the column with default 'legacy_event'
-- so existing rows + every new ingest stay on the legacy consent journal
-- path. Phase-B (#255) flips the default to 'receipt_timeline' once the
-- timeline table is broadly populated.

ALTER TABLE records
  ADD COLUMN consent_model TEXT NOT NULL DEFAULT 'legacy_event'
  CHECK (consent_model IN ('legacy_event', 'receipt_timeline'));

CREATE INDEX records_consent_model_idx
  ON records(consent_model)
  WHERE active = 1 AND tombstoned = 0;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (31, '0031_records_consent_model', '', strftime('%s','now') * 1000);
