-- Migration 0044: entity_episodes (record ↔ entity link).
-- Issue #186 — bitemporal knowledge-graph schema. Spec §3.4.

CREATE TABLE entity_episodes (
    episode_id     TEXT NOT NULL REFERENCES records(record_id) ON DELETE CASCADE,
    entity_node_id TEXT NOT NULL REFERENCES entity_nodes(id) ON DELETE CASCADE,
    linked_at      INTEGER NOT NULL,
    PRIMARY KEY (episode_id, entity_node_id)
);

CREATE INDEX entity_episodes_entity_idx ON entity_episodes(entity_node_id);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (44, '0044_entity_episodes', '', strftime('%s','now') * 1000);
