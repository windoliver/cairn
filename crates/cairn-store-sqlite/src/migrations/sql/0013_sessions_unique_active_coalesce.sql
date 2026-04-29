-- Migration 0013: enforce the active-session uniqueness invariant for
-- vault-only contexts (project_root IS NULL).
-- Brief source: §8.1 Session Lifecycle (single active session invariant).
--
-- Migration 0012 created the partial unique index on plain
-- (user_id, agent_id, project_root). SQLite treats NULL values in unique
-- indexes as distinct, so two rows with project_root = NULL for the same
-- (user, agent) both pass the constraint — leaving the vault-only path
-- open to the same race the index was meant to close. The lookup query
-- in resolve_or_create_session uses `project_root IS ?3`, which matches
-- NULL = NULL, so callers expect the constraint to apply to the NULL case.
--
-- Coercing project_root to '' inside the index makes NULL participate in
-- uniqueness. The lookup query continues to use the non-unique
-- sessions_active_lookup_idx for speed; this index is purely a guardrail.

DROP INDEX sessions_one_active_per_identity_idx;

CREATE UNIQUE INDEX sessions_one_active_per_identity_idx
  ON sessions(user_id, agent_id, COALESCE(project_root, ''))
  WHERE ended_at IS NULL;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (13, '0013_sessions_unique_active_coalesce', '', strftime('%s','now') * 1000);
