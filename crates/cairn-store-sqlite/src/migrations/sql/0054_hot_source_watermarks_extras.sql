-- Issue #83 follow-up: address codex adversarial review round 1 findings 1+2.
--
-- 1. Add two new SourceClass watermark rows that migration 0053 missed.
--    classify_record now maps MemoryKind::Project (unpinned) to the new
--    `projects` class and MemoryKind::UserSignal to `user_signals`,
--    closing the cache-invalidation hole on `top_salience_project` and
--    `recent_user_signal` recipe steps.
--
-- 2. Add `fs_fingerprint` column to hot_prefix_cache. Cached_assemble now
--    stamps a hash of (mtime+size of purpose.md, index.md, config.yaml)
--    into each cache row and re-checks it on hit, so user edits to those
--    files invalidate the cache even though they bypass any Cairn write
--    hook.

INSERT OR IGNORE INTO hot_source_watermarks (class, watermark, updated_at_ms) VALUES
  ('projects',     0, 0),
  ('user_signals', 0, 0);

ALTER TABLE hot_prefix_cache ADD COLUMN fs_fingerprint TEXT NOT NULL DEFAULT '';

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (54, '0054_hot_source_watermarks_extras', '', strftime('%s','now') * 1000);
