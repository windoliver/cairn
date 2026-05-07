-- Migration 0041: widen wal_ops.kind CHECK to admit graph mutations.
-- Issue #186 — bitemporal knowledge-graph schema.
--
-- SQLite CHECK constraints are immutable, so we table-rebuild wal_ops:
-- copy data into a new table with the widened CHECK, then swap names.
-- All triggers and indexes are recreated verbatim.
--
-- Why we save and restore wal_steps and wal_op_deps:
--   `wal_steps.operation_id` and `wal_op_deps.operation_id` reference
--   `wal_ops(operation_id)` with `ON DELETE CASCADE`. SQLite docs:
--   "DROP TABLE performs an implicit DELETE on each row of the table
--    being dropped … and does invoke foreign key actions". So when we
--   DROP wal_ops the cascade fires and tries to delete every child row.
--   The append-only `wal_steps_no_delete` and `wal_op_deps_no_delete`
--   triggers from migration 0002 then ABORT the migration on any
--   database that has prior WAL history. We preserve child rows via
--   TEMP staging tables, allow the cascade to wipe them, then re-INSERT
--   them — net data preservation, no trigger ABORT.
--
-- Why these PRAGMAs:
--   * defer_foreign_keys = ON  — the runner wraps each migration in a
--     transaction; PRAGMA foreign_keys=OFF is a documented no-op inside
--     a transaction, so we must defer FK validation to commit time
--     instead. Live rows that briefly orphan during the rebuild would
--     otherwise be rejected. defer_foreign_keys auto-resets at
--     transaction end, so no explicit OFF is needed.
--   * legacy_alter_table = ON  — modern SQLite (3.26+) rewrites trigger
--     bodies during ALTER TABLE … RENAME TO. wal_op_deps_must_be_acyclic
--     references wal_ops in its WHEN clause, and the rewrite path can
--     fail mid-rename. Disabling the rewrite is the documented escape.
PRAGMA legacy_alter_table = ON;
PRAGMA defer_foreign_keys = ON;

-- Trigger drift pre-check: this migration's safety reasoning enumerates
-- the triggers it must drop and recreate around the wal_ops rebuild. If
-- a future migration (or out-of-band patch) added a DELETE trigger on
-- wal_steps / wal_op_deps the cascade below would silently fire it
-- (data corruption, audit-table leak, or ABORT). Fail loudly here so
-- whoever introduced the trigger updates this rebuild explicitly.
--
-- Idiom: SQLite's `RAISE()` only works inside a trigger body. To raise
-- from top-level SQL we INSERT into a CHECK-constrained TEMP table; if
-- the drift query returns any row, the CHECK fails with a message that
-- carries the offending trigger name back to the caller.
CREATE TEMP TABLE _mig0041_drift_guard (
  msg TEXT NOT NULL CHECK (msg = 'ok')
);
INSERT INTO _mig0041_drift_guard (msg)
  SELECT 'migration 0041: unexpected trigger on wal_steps: ' || name
  FROM sqlite_schema
  WHERE type = 'trigger'
    AND tbl_name = 'wal_steps'
    AND name NOT IN (
      'wal_steps_state_transition',
      'wal_steps_identity_immutable',
      'wal_steps_no_delete'
    );
INSERT INTO _mig0041_drift_guard (msg)
  SELECT 'migration 0041: unexpected trigger on wal_op_deps: ' || name
  FROM sqlite_schema
  WHERE type = 'trigger'
    AND tbl_name = 'wal_op_deps'
    AND name NOT IN (
      'wal_op_deps_must_be_acyclic',
      'wal_op_deps_immutable',
      'wal_op_deps_no_delete'
    );
DROP TABLE _mig0041_drift_guard;

-- Stage children before the cascade wipes them.
CREATE TEMP TABLE _wal_steps_save AS SELECT * FROM wal_steps;
CREATE TEMP TABLE _wal_op_deps_save AS SELECT * FROM wal_op_deps;

-- Drop child no_delete triggers so the cascade can fire silently.
-- Recreated verbatim below after children are restored.
DROP TRIGGER IF EXISTS wal_steps_no_delete;
DROP TRIGGER IF EXISTS wal_op_deps_no_delete;

CREATE TABLE wal_ops_new (
  operation_id   TEXT NOT NULL PRIMARY KEY,
  issued_seq     INTEGER NOT NULL UNIQUE,
  kind           TEXT NOT NULL CHECK (kind IN (
                   'upsert','delete','promote','expire',
                   'forget_session','forget_record','evolve',
                   'graph_upsert_entity','graph_upsert_edge',
                   'graph_contradict','graph_tombstone','graph_link_episode')),
  state          TEXT NOT NULL CHECK (state IN (
                   'ISSUED','PREPARED','COMMITTED','ABORTED','REJECTED')),
  envelope       TEXT NOT NULL,
  issuer         TEXT NOT NULL,
  principal      TEXT,
  target_hash    TEXT NOT NULL,
  scope_json     TEXT NOT NULL,
  plan_ref       TEXT,
  expires_at     INTEGER NOT NULL,
  signature      TEXT NOT NULL,
  issued_at      INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL,
  reason         TEXT
);

INSERT INTO wal_ops_new SELECT * FROM wal_ops;

DROP TRIGGER IF EXISTS wal_ops_issued_seq_must_advance;
DROP TRIGGER IF EXISTS wal_ops_state_transition;
DROP TRIGGER IF EXISTS wal_ops_envelope_immutable;
DROP TRIGGER IF EXISTS wal_ops_terminal_immutable;
DROP TRIGGER IF EXISTS wal_ops_no_delete;
DROP INDEX IF EXISTS wal_ops_open_idx;
DROP TABLE wal_ops;
ALTER TABLE wal_ops_new RENAME TO wal_ops;

-- Restore children from staging.
INSERT INTO wal_steps SELECT * FROM _wal_steps_save;
INSERT INTO wal_op_deps SELECT * FROM _wal_op_deps_save;
DROP TABLE _wal_steps_save;
DROP TABLE _wal_op_deps_save;

CREATE INDEX wal_ops_open_idx
  ON wal_ops(state, issued_at)
  WHERE state IN ('ISSUED','PREPARED');

CREATE TRIGGER wal_ops_issued_seq_must_advance
  BEFORE INSERT ON wal_ops
  FOR EACH ROW
  WHEN NEW.issued_seq <= COALESCE((SELECT MAX(issued_seq) FROM wal_ops), 0)
BEGIN
  SELECT RAISE(ABORT, 'wal_ops.issued_seq must strictly advance MAX(issued_seq)');
END;

CREATE TRIGGER wal_ops_state_transition
  BEFORE UPDATE OF state ON wal_ops
  FOR EACH ROW
  WHEN NEW.state IS NOT OLD.state
   AND NOT (
        (OLD.state = 'ISSUED'   AND NEW.state IN ('PREPARED','REJECTED'))
     OR (OLD.state = 'PREPARED' AND NEW.state IN ('COMMITTED','ABORTED'))
   )
BEGIN
  SELECT RAISE(ABORT, 'wal_ops.state transition not allowed by §5.6 FSM');
END;

CREATE TRIGGER wal_ops_envelope_immutable
  BEFORE UPDATE ON wal_ops
  FOR EACH ROW
  WHEN NEW.operation_id IS NOT OLD.operation_id
    OR NEW.issued_seq   IS NOT OLD.issued_seq
    OR NEW.kind         IS NOT OLD.kind
    OR NEW.envelope     IS NOT OLD.envelope
    OR NEW.issuer       IS NOT OLD.issuer
    OR NEW.principal    IS NOT OLD.principal
    OR NEW.target_hash  IS NOT OLD.target_hash
    OR NEW.scope_json   IS NOT OLD.scope_json
    OR NEW.plan_ref     IS NOT OLD.plan_ref
    OR NEW.expires_at   IS NOT OLD.expires_at
    OR NEW.signature    IS NOT OLD.signature
    OR NEW.issued_at    IS NOT OLD.issued_at
BEGIN
  SELECT RAISE(ABORT, 'wal_ops envelope columns are immutable');
END;

CREATE TRIGGER wal_ops_terminal_immutable
  BEFORE UPDATE ON wal_ops
  FOR EACH ROW
  WHEN OLD.state IN ('COMMITTED', 'ABORTED', 'REJECTED')
BEGIN
  SELECT RAISE(ABORT, 'wal_ops terminal-state rows are fully immutable');
END;

CREATE TRIGGER wal_ops_no_delete
  BEFORE DELETE ON wal_ops
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_ops is append-only; DELETE not permitted');
END;

CREATE TRIGGER wal_steps_no_delete
  BEFORE DELETE ON wal_steps
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_steps is append-only; DELETE not permitted');
END;

CREATE TRIGGER wal_op_deps_no_delete
  BEFORE DELETE ON wal_op_deps
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'wal_op_deps is append-only');
END;

PRAGMA legacy_alter_table = OFF;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (41, '0041_wal_kind_widening', '', strftime('%s','now') * 1000);
