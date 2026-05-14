-- Migration 0060: salience access tracking metadata.
-- Brief sources: §5.1 (read ranking), §10 (background workflows),
-- issue #313 (access-frequency strengthening + decay guardrails).

ALTER TABLE records ADD COLUMN last_accessed_at_ms INTEGER;
ALTER TABLE records ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1));

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (60, '0060_salience_access', '', strftime('%s','now') * 1000);
