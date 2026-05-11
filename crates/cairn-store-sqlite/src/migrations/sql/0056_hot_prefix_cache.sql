CREATE TABLE hot_source_watermarks (
  class TEXT PRIMARY KEY,
  watermark INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
) WITHOUT ROWID;

INSERT INTO hot_source_watermarks (class, watermark, updated_at_ms) VALUES
  ('profile_evidence', 0, 0),
  ('pinned',           0, 0),
  ('purpose_index',    0, 0),
  ('summaries',        0, 0),
  ('playbooks',        0, 0),
  ('policy',           0, 0);

CREATE TABLE hot_prefix_cache (
  agent_id TEXT NOT NULL,
  recipe_hash TEXT NOT NULL,
  prefix BLOB NOT NULL,
  segments_json TEXT NOT NULL,
  bytes INTEGER NOT NULL,
  watermarks_json TEXT NOT NULL,
  assembled_at_ms INTEGER NOT NULL,
  assembly_latency_ms INTEGER NOT NULL,
  PRIMARY KEY (agent_id, recipe_hash)
) WITHOUT ROWID;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (56, '0056_hot_prefix_cache', '', strftime('%s','now') * 1000);
