-- Migration 0012: enforce one active session per (user, agent, project_root).
-- Brief source: §8.1 Session Lifecycle (single active session invariant).
--
-- The auto-discover/auto-create path is racy without a database constraint:
-- two concurrent callers can both observe "no active session" and both
-- insert fresh rows, splitting later writes across diverging session IDs.
-- A partial unique index forces one INSERT in any racing pair to fail; the
-- store retries by going back to find_active_session, which now succeeds.
--
-- SQLite treats two NULLs as distinct in unique indexes, which is exactly
-- what we want for the project-root-less context: a session pinned to a
-- concrete /repo and a session with no project_root must coexist for the
-- same (user, agent).

CREATE UNIQUE INDEX sessions_one_active_per_identity_idx
  ON sessions(user_id, agent_id, project_root)
  WHERE ended_at IS NULL;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (12, '0012_sessions_unique_active', '', strftime('%s','now') * 1000);
