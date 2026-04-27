-- Migration 0001: records, FTS5, edges, records_latest view, schema_migrations.
-- Brief sources: §3 (lines ~340-426) for records / FTS / edges / view DDL.

CREATE TABLE schema_migrations (
  migration_id  INTEGER NOT NULL PRIMARY KEY,
  name          TEXT    NOT NULL,
  sql_hash      TEXT    NOT NULL,
  applied_at    INTEGER NOT NULL
);

CREATE TRIGGER schema_migrations_no_delete
  BEFORE DELETE ON schema_migrations
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'schema_migrations is append-only');
END;

-- Identity columns are immutable; sql_hash is allowed exactly one
-- transition from '' to a non-empty value (stamped post-migration so the
-- ledger records a content hash for drift detection).
CREATE TRIGGER schema_migrations_immutable
  BEFORE UPDATE ON schema_migrations
  FOR EACH ROW
  WHEN NEW.migration_id IS NOT OLD.migration_id
    OR NEW.name         IS NOT OLD.name
    OR NEW.applied_at   IS NOT OLD.applied_at
    OR NOT (OLD.sql_hash = '' AND length(NEW.sql_hash) > 0)
BEGIN
  SELECT RAISE(ABORT, 'schema_migrations rows are immutable (only `` -> hash on sql_hash allowed)');
END;

CREATE TABLE records (
  record_id   TEXT PRIMARY KEY,
  target_id   TEXT NOT NULL,
  version     INTEGER NOT NULL,
  path        TEXT NOT NULL,
  kind        TEXT NOT NULL,
  class       TEXT NOT NULL,
  visibility  TEXT NOT NULL,
  scope       TEXT NOT NULL,
  actor_chain TEXT NOT NULL,
  body        TEXT NOT NULL,
  body_hash   TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  active      INTEGER NOT NULL DEFAULT 0,
  tombstoned  INTEGER NOT NULL DEFAULT 0,
  is_static   INTEGER NOT NULL DEFAULT 0,
  UNIQUE (target_id, version)
);

CREATE UNIQUE INDEX records_active_target_idx
  ON records(target_id) WHERE active = 1;
CREATE INDEX records_path_idx
  ON records(path) WHERE active = 1 AND tombstoned = 0;
CREATE INDEX records_kind_idx
  ON records(kind) WHERE active = 1 AND tombstoned = 0;
CREATE INDEX records_visibility_idx
  ON records(visibility) WHERE active = 1 AND tombstoned = 0;
CREATE INDEX records_scope_idx
  ON records(scope) WHERE active = 1 AND tombstoned = 0;

CREATE VIRTUAL TABLE records_fts USING fts5(
  body,
  content='records',
  content_rowid='rowid',
  tokenize='porter unicode61'
);

CREATE TRIGGER records_fts_ai AFTER INSERT ON records BEGIN
  INSERT INTO records_fts(rowid, body) VALUES (new.rowid, new.body);
END;
CREATE TRIGGER records_fts_ad AFTER DELETE ON records BEGIN
  INSERT INTO records_fts(records_fts, rowid, body) VALUES ('delete', old.rowid, old.body);
END;
CREATE TRIGGER records_fts_au AFTER UPDATE ON records BEGIN
  INSERT INTO records_fts(records_fts, rowid, body) VALUES ('delete', old.rowid, old.body);
  INSERT INTO records_fts(rowid, body) VALUES (new.rowid, new.body);
END;

CREATE TABLE edges (
  src    TEXT NOT NULL,
  dst    TEXT NOT NULL,
  kind   TEXT NOT NULL,
  weight REAL,
  PRIMARY KEY (src, dst, kind),
  FOREIGN KEY (src) REFERENCES records(record_id) DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY (dst) REFERENCES records(record_id) DEFERRABLE INITIALLY DEFERRED
);

-- An `updates` edge expresses fact-supersession across distinct target_ids
-- (brief §3 line ~409). Endpoints must be non-tombstoned at insert time.
CREATE TRIGGER edges_updates_supersede_insert
  BEFORE INSERT ON edges
  FOR EACH ROW
  WHEN NEW.kind = 'updates'
   AND (
        NOT EXISTS (SELECT 1 FROM records WHERE record_id = NEW.src AND tombstoned = 0)
     OR NOT EXISTS (SELECT 1 FROM records WHERE record_id = NEW.dst AND tombstoned = 0)
     OR (SELECT target_id FROM records WHERE record_id = NEW.src) IS
        (SELECT target_id FROM records WHERE record_id = NEW.dst)
   )
BEGIN
  SELECT RAISE(ABORT, 'updates edge requires non-tombstoned endpoints with distinct target_ids');
END;

CREATE TRIGGER edges_updates_immutable_after_insert
  BEFORE UPDATE ON edges
  FOR EACH ROW
  WHEN OLD.kind = 'updates'
   AND (NEW.src IS NOT OLD.src OR NEW.dst IS NOT OLD.dst OR NEW.kind IS NOT OLD.kind)
BEGIN
  SELECT RAISE(ABORT, 'updates edges are immutable');
END;

-- Block UPDATEs that would convert a non-`updates` edge into an `updates`
-- edge or otherwise re-target it; without this an attacker can bypass the
-- INSERT-time predicate by inserting a benign edge then flipping `kind`.
CREATE TRIGGER edges_updates_no_kind_flip
  BEFORE UPDATE ON edges
  FOR EACH ROW
  WHEN NEW.kind = 'updates'
   AND (
        OLD.kind IS NOT 'updates'
     OR NEW.src  IS NOT OLD.src
     OR NEW.dst  IS NOT OLD.dst
   )
BEGIN
  SELECT RAISE(ABORT, 'updates edge identity must be set at INSERT time and is immutable');
END;

CREATE VIEW records_latest AS
  SELECT r.*
    FROM records r
   WHERE r.active = 1
     AND r.tombstoned = 0
     AND NOT EXISTS (
       SELECT 1 FROM edges e
        WHERE e.kind = 'updates' AND e.dst = r.record_id
     );

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (1, '0001_records', '', strftime('%s','now') * 1000);
