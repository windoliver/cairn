-- Migration 0043: entity_edges with bitemporal validity windows.
-- Issue #186 — bitemporal knowledge-graph schema. Spec §3.3.

CREATE TABLE entity_edges (
    id                TEXT PRIMARY KEY,
    source_id         TEXT NOT NULL REFERENCES entity_nodes(id),
    target_id         TEXT NOT NULL REFERENCES entity_nodes(id),
    relation          TEXT NOT NULL,
    confidence        TEXT NOT NULL CHECK(confidence IN ('EXTRACTED','INFERRED','AMBIGUOUS')),
    confidence_score  REAL NOT NULL CHECK(confidence_score BETWEEN 0.0 AND 1.0),
    valid_at          INTEGER NOT NULL,
    invalid_at        INTEGER,
    created_at        INTEGER NOT NULL,
    expired_at        INTEGER,
    tombstone_reason  TEXT,
    source_record_id  TEXT REFERENCES records(record_id) ON DELETE SET NULL,
    body_hash         BLOB NOT NULL,
    -- Bitemporal window integrity: end-bounds may not precede their
    -- start. Without these, a backdated upsert or contradiction can
    -- persist `invalid_at < valid_at` and corrupt as-of slicing.
    -- We allow the degenerate `invalid_at = valid_at` (empty window)
    -- because the contradiction branch sets `old.invalid_at =
    -- new.valid_at` when both edges' valid_at agree — semantically
    -- "instantaneous replacement". A degenerate window matches no
    -- as-of predicate (`valid_at <= T AND invalid_at > T` rejects
    -- T = valid_at = invalid_at), so it sits inert.
    CHECK (invalid_at IS NULL OR invalid_at >= valid_at),
    CHECK (expired_at IS NULL OR expired_at >= created_at),
    -- Tombstone audit invariant: any expired row MUST carry a reason.
    -- The shrink_guard trigger below only catches UPDATE OF expired_at;
    -- this CHECK closes the INSERT path and the "clear tombstone_reason
    -- without touching expired_at" UPDATE path. A row with an audit
    -- reason but no expired_at is allowed (in-progress tombstone work).
    CHECK (expired_at IS NULL OR tombstone_reason IS NOT NULL)
);

CREATE UNIQUE INDEX entity_edges_live_triple
  ON entity_edges(source_id, target_id, relation)
  WHERE invalid_at IS NULL AND expired_at IS NULL;

CREATE INDEX entity_edges_valid_at_idx
  ON entity_edges(valid_at);

CREATE INDEX entity_edges_invalid_at_idx
  ON entity_edges(invalid_at)
  WHERE invalid_at IS NOT NULL;

CREATE INDEX entity_edges_source_relation_idx
  ON entity_edges(source_id, relation);

CREATE INDEX entity_edges_target_relation_idx
  ON entity_edges(target_id, relation);

CREATE TRIGGER entity_edges_shrink_guard
  BEFORE UPDATE OF expired_at ON entity_edges
  FOR EACH ROW
  WHEN NEW.expired_at IS NOT NULL AND NEW.tombstone_reason IS NULL
BEGIN
  SELECT RAISE(ABORT, 'entity_edges.expired_at requires tombstone_reason');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (43, '0043_entity_edges', '', strftime('%s','now') * 1000);
