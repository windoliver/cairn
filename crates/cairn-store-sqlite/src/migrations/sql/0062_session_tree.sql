-- Migration 0062: storage substrate for v0.3 session trees.
-- Issue #133 / brief §5.7.
--
-- This adds branch/merge metadata without advertising the blocked
-- cairn.sessiontree.v1 extension. Existing v0.1 session rows remain valid:
-- read paths synthesize a one-node tree when no row exists here.

CREATE TABLE session_tree_nodes (
  session_id         TEXT PRIMARY KEY
                     REFERENCES sessions(session_id) ON DELETE CASCADE,
  parent_session_id  TEXT
                     REFERENCES sessions(session_id) ON DELETE RESTRICT,
  at_turn_id         TEXT,
  branch_kind        TEXT CHECK (branch_kind IN ('fork', 'clone', 'tool_spawned')),
  tool_call_id       TEXT,
  created_at         INTEGER NOT NULL,

  CHECK (session_id != parent_session_id),
  CHECK (
    (parent_session_id IS NULL AND at_turn_id IS NULL AND branch_kind IS NULL AND tool_call_id IS NULL)
    OR
    (parent_session_id IS NOT NULL AND at_turn_id IS NOT NULL AND branch_kind IS NOT NULL)
  ),
  CHECK (
    branch_kind IS NULL
    OR (branch_kind = 'tool_spawned' AND tool_call_id IS NOT NULL AND length(tool_call_id) > 0)
    OR (branch_kind IN ('fork', 'clone') AND tool_call_id IS NULL)
  )
);

CREATE INDEX session_tree_nodes_parent_idx
  ON session_tree_nodes(parent_session_id, created_at, session_id)
  WHERE parent_session_id IS NOT NULL;

CREATE TABLE session_tree_merges (
  merge_id             INTEGER PRIMARY KEY AUTOINCREMENT,
  source_session_id    TEXT NOT NULL
                       REFERENCES sessions(session_id) ON DELETE RESTRICT,
  destination_session_id TEXT NOT NULL
                       REFERENCES sessions(session_id) ON DELETE RESTRICT,
  strategy_kind        TEXT NOT NULL CHECK (strategy_kind IN ('reasoning_summary', 'controlled_splice')),
  summary_record_id    TEXT,
  first_turn_id        TEXT,
  last_turn_id         TEXT,
  applied_at_turn_id   TEXT NOT NULL,
  created_at           INTEGER NOT NULL,

  CHECK (source_session_id != destination_session_id),
  CHECK (
    (strategy_kind = 'reasoning_summary'
      AND summary_record_id IS NOT NULL
      AND first_turn_id IS NULL
      AND last_turn_id IS NULL)
    OR
    (strategy_kind = 'controlled_splice'
      AND summary_record_id IS NULL
      AND first_turn_id IS NOT NULL
      AND length(first_turn_id) > 0
      AND last_turn_id IS NOT NULL
      AND length(last_turn_id) > 0)
  ),
  CHECK (length(applied_at_turn_id) > 0)
);

CREATE INDEX session_tree_merges_source_idx
  ON session_tree_merges(source_session_id, created_at, merge_id);

CREATE INDEX session_tree_merges_destination_idx
  ON session_tree_merges(destination_session_id, created_at, merge_id);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (62, '0062_session_tree', '', strftime('%s','now') * 1000);
