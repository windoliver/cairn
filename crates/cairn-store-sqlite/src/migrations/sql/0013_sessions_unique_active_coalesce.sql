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

-- Step 1: normalize any historical project_root='' rows to NULL so they
-- collide on the new index the same way they collide on the lookup query
-- (`project_root IS ?3`). Without this, an existing vault that ever wrote
-- '' would fail this migration with a uniqueness error and refuse to open.
UPDATE sessions SET project_root = NULL WHERE project_root = '';

-- Step 1b: deduplicate any pre-existing duplicate active rows for the
-- same (user, agent, COALESCE(project_root, '')) triple. Migration 0012's
-- index treated NULL values as distinct, so under the §8.1 race a vault
-- could legitimately hold multiple active rows for the same vault-only
-- identity. The new unique index would otherwise abort migration on those
-- vaults and refuse to open. Resolution: keep the row with the most
-- recent last_activity_at (tie-breaking on session_id for determinism)
-- and mark the rest ended_at = strftime millis. Live writers cannot lose
-- in-flight writes here because a database under migration is single-
-- writer by construction (rusqlite_migration runs to_latest in one tx).
WITH duplicates AS (
  SELECT
    s.session_id,
    ROW_NUMBER() OVER (
      PARTITION BY s.user_id, s.agent_id, COALESCE(s.project_root, '')
      ORDER BY s.last_activity_at DESC, s.session_id DESC
    ) AS rn
  FROM sessions s
  WHERE s.ended_at IS NULL
)
UPDATE sessions
   SET ended_at = strftime('%s','now') * 1000
 WHERE session_id IN (SELECT session_id FROM duplicates WHERE rn > 1);

-- 0012 was reduced to a no-op (see its header), so the original index it
-- created may not exist on fresh installs. Use IF EXISTS to keep this
-- migration idempotent across upgrade paths.
DROP INDEX IF EXISTS sessions_one_active_per_identity_idx;

CREATE UNIQUE INDEX sessions_one_active_per_identity_idx
  ON sessions(user_id, agent_id, COALESCE(project_root, ''))
  WHERE ended_at IS NULL;

-- Step 2: prevent future writers from re-introducing the empty-string form.
-- ALTER TABLE on SQLite cannot add a CHECK constraint in place; triggers
-- are the equivalent enforcement mechanism (also used elsewhere in this
-- store, e.g. the schema_migrations append-only triggers).
CREATE TRIGGER sessions_project_root_no_empty_insert
  BEFORE INSERT ON sessions
  FOR EACH ROW
  WHEN NEW.project_root = ''
BEGIN
  SELECT RAISE(ABORT, 'sessions.project_root must be NULL or non-empty');
END;

CREATE TRIGGER sessions_project_root_no_empty_update
  BEFORE UPDATE ON sessions
  FOR EACH ROW
  WHEN NEW.project_root = ''
BEGIN
  SELECT RAISE(ABORT, 'sessions.project_root must be NULL or non-empty');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (13, '0013_sessions_unique_active_coalesce', '', strftime('%s','now') * 1000);
