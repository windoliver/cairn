-- Migration 0065: cached task-trace canvas markdown projection.
-- Issue #134 addendum. SQLite rows remain authoritative; this stores a
-- rebuildable markdown cache so lint can detect projection drift.

ALTER TABLE trace_canvases
  ADD COLUMN projection_markdown TEXT;

ALTER TABLE trace_canvases
  ADD COLUMN projection_hash TEXT;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (66, '0066_trace_canvas_projection', '', strftime('%s','now') * 1000);
