-- Migration 0012: placeholder for the partial unique index on
-- (user_id, agent_id, project_root) WHERE ended_at IS NULL.
-- Brief source: §8.1 Session Lifecycle (single active session invariant).
--
-- An earlier draft of this migration created the partial unique index here
-- on plain `(user_id, agent_id, project_root)`. Migration 0013 supersedes
-- that index entirely (recreates it under `COALESCE(project_root, '')` so
-- NULL values participate in uniqueness, plus a dedup pass for any
-- pre-existing duplicate active rows). Creating the original index here
-- would have aborted upgrade on any vault holding such duplicates BEFORE
-- 0013's repair pass had a chance to run, since `rusqlite_migration`
-- applies migrations in a single transaction and any error rolls the
-- upgrade back. Leaving 0012 as a no-op marker keeps the migration
-- numbering append-only while letting 0013 own the index lifecycle.
--
-- See `crates/cairn-store-sqlite/src/migrations/sql/0013_sessions_unique_active_coalesce.sql`
-- for the actual index definition and dedup pass.

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (12, '0012_sessions_unique_active', '', strftime('%s','now') * 1000);
