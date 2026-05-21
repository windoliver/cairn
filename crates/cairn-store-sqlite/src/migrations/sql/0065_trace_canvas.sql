-- Migration 0064: task-trace canvas storage substrate.
-- Issue #134 addendum. Brief sources: §5.7 Sessions are trees, §7 Hot Memory,
-- §10 Continuous Learning.
--
-- This migration stores durable trace steps and their projected canvas graph.
-- It intentionally stops at the substrate: capture/workflow materialization,
-- hot-memory selection, lint rules, and exact retrieve lookups can be layered
-- on these tables without changing the schema shape.

CREATE TABLE trace_canvases (
  canvas_id       TEXT PRIMARY KEY,
  session_id      TEXT NOT NULL,
  title           TEXT NOT NULL CHECK (length(title) > 0),
  goal            TEXT NOT NULL CHECK (length(goal) > 0),
  status          TEXT NOT NULL
                  CHECK (status IN ('active', 'completed', 'abandoned')),
  summary         TEXT NOT NULL,
  active_node_id  TEXT,
  max_bytes       INTEGER NOT NULL DEFAULT 8192 CHECK (max_bytes > 0),
  version         INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
  created_at_ms   INTEGER NOT NULL,
  updated_at_ms   INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms)
);

CREATE INDEX trace_canvases_session_idx
  ON trace_canvases(session_id, status, updated_at_ms DESC, canvas_id);

CREATE TABLE trace_canvas_nodes (
  node_id             TEXT PRIMARY KEY,
  canvas_id           TEXT NOT NULL
                      REFERENCES trace_canvases(canvas_id) ON DELETE CASCADE,
  label               TEXT NOT NULL CHECK (length(label) > 0),
  status              TEXT NOT NULL
                      CHECK (status IN ('planned', 'active', 'completed', 'blocked', 'discarded')),
  summary             TEXT NOT NULL,
  timestamp_ms        INTEGER NOT NULL,
  source_step_ids     TEXT NOT NULL
                      CHECK (
                        json_valid(source_step_ids)
                        AND json_type(source_step_ids) = 'array'
                        AND json_array_length(source_step_ids) > 0
                      ),
  evidence_record_ids TEXT NOT NULL
                      CHECK (
                        json_valid(evidence_record_ids)
                        AND json_type(evidence_record_ids) = 'array'
                      )
);

CREATE INDEX trace_canvas_nodes_canvas_idx
  ON trace_canvas_nodes(canvas_id, status, timestamp_ms, node_id);

CREATE TABLE trace_canvas_edges (
  canvas_id     TEXT NOT NULL
                REFERENCES trace_canvases(canvas_id) ON DELETE CASCADE,
  from_node_id  TEXT NOT NULL
                REFERENCES trace_canvas_nodes(node_id) ON DELETE CASCADE,
  to_node_id    TEXT NOT NULL
                REFERENCES trace_canvas_nodes(node_id) ON DELETE CASCADE,
  kind          TEXT NOT NULL
                CHECK (kind IN ('depends_on', 'supersedes', 'branches_to', 'merges_into', 'supports')),
  label         TEXT,

  CHECK (from_node_id != to_node_id)
);

CREATE UNIQUE INDEX trace_canvas_edges_unique_idx
  ON trace_canvas_edges(canvas_id, from_node_id, to_node_id, kind, COALESCE(label, ''));

CREATE INDEX trace_canvas_edges_to_idx
  ON trace_canvas_edges(to_node_id, kind, canvas_id);

CREATE TABLE trace_steps (
  step_id               TEXT PRIMARY KEY,
  trace_id              TEXT NOT NULL,
  session_id            TEXT NOT NULL,
  turn_id               TEXT NOT NULL,
  tool_call_id          TEXT,
  timestamp_ms          INTEGER NOT NULL,
  tool_name             TEXT,
  call_summary          TEXT NOT NULL,
  result_summary        TEXT NOT NULL,
  result_ref            TEXT,
  salience              REAL NOT NULL DEFAULT 0.5
                        CHECK (salience >= 0.0 AND salience <= 1.0),
  replaceability_score  REAL NOT NULL DEFAULT 0.5
                        CHECK (replaceability_score >= 0.0 AND replaceability_score <= 1.0),
  node_id               TEXT
                        REFERENCES trace_canvas_nodes(node_id) ON DELETE SET NULL,
  source_hash           TEXT NOT NULL CHECK (length(source_hash) > 0),
  created_at_ms         INTEGER NOT NULL,
  updated_at_ms         INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms)
);

CREATE UNIQUE INDEX trace_steps_replay_key_idx
  ON trace_steps(source_hash, COALESCE(tool_call_id, ''));

CREATE INDEX trace_steps_session_turn_idx
  ON trace_steps(session_id, turn_id, timestamp_ms, step_id);

CREATE INDEX trace_steps_node_idx
  ON trace_steps(node_id, timestamp_ms, step_id)
  WHERE node_id IS NOT NULL;

CREATE INDEX trace_steps_result_ref_idx
  ON trace_steps(result_ref)
  WHERE result_ref IS NOT NULL;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (65, '0065_trace_canvas', '', strftime('%s','now') * 1000);
