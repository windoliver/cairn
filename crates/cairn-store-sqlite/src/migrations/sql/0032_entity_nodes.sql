-- Migration 0032: entity_nodes + FTS5 mirror + shrink-guard.
-- Issue #186 — bitemporal knowledge-graph schema. Spec §3.2.

CREATE TABLE entity_nodes (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    name_norm       TEXT NOT NULL UNIQUE,
    summary         TEXT,
    created_at      INTEGER NOT NULL,
    expired_at      INTEGER,
    tombstone_reason TEXT,
    embedding_id    TEXT,
    -- Tombstone audit invariant: any expired row MUST carry a reason.
    -- The shrink_guard trigger below only catches UPDATE OF expired_at;
    -- this CHECK closes the INSERT path and the "clear tombstone_reason
    -- without touching expired_at" UPDATE path. A row with an audit
    -- reason but no expired_at is allowed (in-progress tombstone work).
    CHECK (expired_at IS NULL OR tombstone_reason IS NOT NULL)
);

-- UNIQUE(name_norm) on the column above already creates an implicit
-- unique index; an explicit secondary index would be dead weight on
-- writes and never preferred by the planner over the unique one.

CREATE VIRTUAL TABLE entity_nodes_fts USING fts5(
    name, summary,
    content='entity_nodes',
    content_rowid='rowid'
);

CREATE TRIGGER entity_nodes_shrink_guard
  BEFORE UPDATE OF expired_at ON entity_nodes
  FOR EACH ROW
  WHEN NEW.expired_at IS NOT NULL AND NEW.tombstone_reason IS NULL
BEGIN
  SELECT RAISE(ABORT, 'entity_nodes.expired_at requires tombstone_reason');
END;

CREATE TRIGGER entity_nodes_fts_ai AFTER INSERT ON entity_nodes BEGIN
  INSERT INTO entity_nodes_fts(rowid, name, summary)
    VALUES (NEW.rowid, NEW.name, COALESCE(NEW.summary, ''));
END;

CREATE TRIGGER entity_nodes_fts_au AFTER UPDATE ON entity_nodes BEGIN
  INSERT INTO entity_nodes_fts(entity_nodes_fts, rowid, name, summary)
    VALUES ('delete', OLD.rowid, OLD.name, COALESCE(OLD.summary, ''));
  INSERT INTO entity_nodes_fts(rowid, name, summary)
    VALUES (NEW.rowid, NEW.name, COALESCE(NEW.summary, ''));
END;

CREATE TRIGGER entity_nodes_fts_ad AFTER DELETE ON entity_nodes BEGIN
  INSERT INTO entity_nodes_fts(entity_nodes_fts, rowid, name, summary)
    VALUES ('delete', OLD.rowid, OLD.name, COALESCE(OLD.summary, ''));
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (32, '0032_entity_nodes', '', strftime('%s','now') * 1000);
