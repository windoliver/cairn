-- Migration 0011: sessions table for auto-discovery + auto-create.
-- Brief source: §8.1 Session Lifecycle.
--
-- Sessions key turns to a (user_id, agent_id, project_root) triple. The
-- ingest/search/retrieve/etc. verbs accept an optional session_id; when
-- absent, the resolver looks up the most recent active row for the caller's
-- identity and decides reuse-vs-create against an idle window.

CREATE TABLE sessions (
  session_id        TEXT PRIMARY KEY,
  user_id           TEXT NOT NULL,
  agent_id          TEXT NOT NULL,
  project_root      TEXT,
  title             TEXT NOT NULL DEFAULT '',
  channel           TEXT,
  priority          TEXT,
  tags              TEXT,           -- JSON array, NULL when unset
  metadata_json     TEXT,           -- free-form JSON for forward extension
  created_at        INTEGER NOT NULL,
  last_activity_at  INTEGER NOT NULL,
  ended_at          INTEGER         -- NULL until idle window elapses or explicit end
);

-- Auto-discovery query: most recent active session for (user, agent, project_root).
-- `ended_at IS NULL` partial index keeps it tight even after long-lived deployments.
CREATE INDEX sessions_active_lookup_idx
  ON sessions(user_id, agent_id, project_root, last_activity_at DESC)
  WHERE ended_at IS NULL;

-- Reverse-time scan for `cairn search --scope sessions`.
CREATE INDEX sessions_last_activity_idx
  ON sessions(last_activity_at DESC);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (11, '0011_sessions', '', strftime('%s','now') * 1000);
