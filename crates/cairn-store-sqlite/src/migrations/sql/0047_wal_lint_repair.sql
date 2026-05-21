-- Migration 0047: extend wal_ops.kind CHECK to include 'lint_repair'.
-- Brief source: §5.6 (WAL FSM); issue #254 (lint --fix-markdown).
--
-- SQLite does not support ALTER TABLE ... DROP/MODIFY CONSTRAINT, so we
-- must change the stored CREATE TABLE statement to broaden the kind CHECK.
-- The classic workaround — rebuild the parent under a new name — risks
-- stranding child foreign keys (wal_op_deps, wal_steps, reader_fence all
-- reference wal_ops(operation_id)). `legacy_alter_table = ON` is supposed
-- to suppress FK retargeting across `ALTER TABLE ... RENAME`, but in our
-- migration runner's transaction context the FK targets in child schemas
-- are still rewritten, leaving them pointing at the renamed parent.
--
-- The robust path documented by SQLite for exactly this scenario is to
-- enable PRAGMA writable_schema, surgically rewrite the parent's
-- CREATE TABLE in sqlite_master, then run integrity_check. All triggers,
-- indexes, and child FKs survive untouched because the table object's
-- internal identity (the rootpage and column layout) is unchanged — only
-- the textual CHECK widens.
--
-- See https://sqlite.org/pragma.html#pragma_writable_schema and
-- https://sqlite.org/lang_altertable.html (§"Otheralter").

PRAGMA writable_schema = ON;

UPDATE sqlite_master
   SET sql =
       'CREATE TABLE wal_ops (' ||
       '  operation_id   TEXT NOT NULL PRIMARY KEY,' ||
       '  issued_seq     INTEGER NOT NULL UNIQUE,' ||
       '  kind           TEXT NOT NULL CHECK (kind IN (' ||
       '                   ''upsert'',''delete'',''promote'',''expire'',' ||
       '                   ''forget_session'',''forget_record'',''evolve'',' ||
       '                   ''graph_upsert_entity'',''graph_upsert_edge'',' ||
       '                   ''graph_contradict'',''graph_tombstone'',''graph_link_episode'',' ||
       '                   ''lint_repair'')),' ||
       '  state          TEXT NOT NULL CHECK (state IN (' ||
       '                   ''ISSUED'',''PREPARED'',''COMMITTED'',''ABORTED'',''REJECTED'')),' ||
       '  envelope       TEXT NOT NULL,' ||
       '  issuer         TEXT NOT NULL,' ||
       '  principal      TEXT,' ||
       '  target_hash    TEXT NOT NULL,' ||
       '  scope_json     TEXT NOT NULL,' ||
       '  plan_ref       TEXT,' ||
       '  expires_at     INTEGER NOT NULL,' ||
       '  signature      TEXT NOT NULL,' ||
       '  issued_at      INTEGER NOT NULL,' ||
       '  updated_at     INTEGER NOT NULL,' ||
       '  reason         TEXT' ||
       ')'
 WHERE type = 'table' AND name = 'wal_ops';

-- writable_schema = RESET turns the pragma off AND forces SQLite to
-- re-parse the in-memory schema cache so subsequent statements (and
-- subsequent connections) observe the broadened CHECK. Without RESET,
-- the connection that ran the migration would keep using the original
-- narrow CHECK in memory.
PRAGMA writable_schema = RESET;

-- Enforced post-condition: assert the rewritten DDL parses AND mentions
-- 'lint_repair'. Plain `PRAGMA integrity_check` only emits diagnostics —
-- its output isn't surfaced as an error, so a damaged rewrite would
-- still commit. Insert a probe row into a TEMP table whose CHECK
-- constraint requires `ok = 1`; if either condition fails, the INSERT's
-- CHECK violation aborts the migration before `schema_migrations` is
-- stamped.
--
-- Two checks: (a) the row exists at all (no `wal_ops` entry → empty
-- SELECT → INSERT inserts zero rows → no abort, hence the COUNT(*)
-- check first), and (b) the rewritten SQL contains `'lint_repair'`.
CREATE TEMP TABLE _migration_0047_check (
  ok INTEGER NOT NULL CHECK (ok = 1)
);
INSERT INTO _migration_0047_check
  SELECT 1
  WHERE (
    SELECT COUNT(*) FROM sqlite_master
     WHERE type = 'table' AND name = 'wal_ops'
  ) = 1;
INSERT INTO _migration_0047_check
  SELECT CASE
    WHEN sql LIKE '%''lint_repair''%' THEN 1
    ELSE 0
  END
  FROM sqlite_master
 WHERE type = 'table' AND name = 'wal_ops';
-- (a) Was the rewritten DDL string-check satisfied? The two INSERTs
-- above must have inserted exactly two rows (one per assertion).
INSERT INTO _migration_0047_check
  SELECT CASE
    WHEN (SELECT COUNT(*) FROM _migration_0047_check) = 2 THEN 1
    ELSE 0
  END;
DROP TABLE _migration_0047_check;

-- Executable post-condition: the read-only DDL check above proves the
-- stored CREATE TABLE text mentions 'lint_repair', but it does NOT
-- prove the live SQLite query planner re-parsed the schema and the
-- new CHECK constraint actually accepts the new kind. The only way
-- to verify that is to attempt a real INSERT. Wrap it in a
-- SAVEPOINT so the probe row never persists, and use a sentinel
-- `operation_id` that's wide enough to be obviously not real.
-- `issued_seq` has a strict-advance trigger (`NEW.issued_seq >
-- MAX(issued_seq)`), so the probe must use a value larger than any
-- existing row. `i64::MAX` is the sentinel; rolled back below before
-- any real run can collide with it.
SAVEPOINT _migration_0047_probe;
INSERT INTO wal_ops(
  operation_id, issued_seq, kind, state, envelope,
  issuer, target_hash, scope_json, expires_at,
  signature, issued_at, updated_at
) VALUES (
  '__migration_0047_probe__', 9223372036854775807, 'lint_repair', 'ISSUED', '{}',
  'migration', '0', '{}', 0,
  '', 0, 0
);
ROLLBACK TO SAVEPOINT _migration_0047_probe;
RELEASE SAVEPOINT _migration_0047_probe;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (47, '0047_wal_lint_repair', '', strftime('%s','now') * 1000);
