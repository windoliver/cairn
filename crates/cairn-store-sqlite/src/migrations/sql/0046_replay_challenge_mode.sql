-- Migration 0046: enable challenge-mode replay (issue #52, brief §4.2).
--
-- The 0003 ledger requires `used.sequence INTEGER NOT NULL` and triggers
-- enforce strict sequence advance on every insert. That fits the
-- sequence-mode hot path but blocks the challenge-mode path the brief
-- defines: stateless issuers that skip the per-issuer counter and
-- consume a single-use server-minted nonce instead. To admit both modes
-- inside the same atomic ledger transaction, this migration:
--
-- (a) Rebuilds `used` with `sequence` NULLABLE plus a new `challenge`
--     BLOB column. Rows must populate exactly one of (sequence,
--     challenge) — a CHECK enforces it.
-- (b) Drops + recreates the four ledger triggers from 0003 with
--     NULL-aware predicates so a NULL-sequence row neither asserts
--     strict advance nor advances `issuer_seq.high_water`.
-- (c) Leaves the (issuer, nonce) and (issuer, sequence) UNIQUE
--     constraints untouched — SQLite UNIQUE permits multiple NULLs in
--     the sequence column, which is exactly what challenge mode needs.
--
-- Rebuild rationale: SQLite cannot relax NOT NULL on an existing column
-- (no `ALTER TABLE … ALTER COLUMN`). The standard table-rebuild idiom
-- (`CREATE new`, copy rows, drop original, rename) is required.
-- `wal_ops` retains its FK target via the deferred constraint.

PRAGMA defer_foreign_keys = ON;
PRAGMA legacy_alter_table = ON;

-- Drift guard: the rebuild below DROPs `used` after a rename, so any
-- out-of-band trigger / index attached to it would be silently
-- destroyed and the post-migration fingerprint would still validate.
-- Mirrors 0041's pre-rebuild guard — fail the migration loudly if a
-- replay-ledger object exists outside the explicit allowlist below
-- (issue #52 round-9 review #2).
CREATE TEMP TABLE _mig0046_drift_guard (
  msg TEXT NOT NULL CHECK (msg = 'ok')
);
INSERT INTO _mig0046_drift_guard (msg)
  SELECT 'migration 0046: unexpected schema object on used / issuer_seq / outstanding_challenges: '
         || type || ':' || name
    FROM sqlite_schema
   WHERE (
           tbl_name IN ('used', 'issuer_seq', 'outstanding_challenges')
        OR name IN ('used', 'issuer_seq', 'outstanding_challenges')
         )
     AND name NOT LIKE 'sqlite_autoindex_%'
     AND (type, name) NOT IN (
       VALUES
         ('table', 'used'),
         ('table', 'issuer_seq'),
         ('table', 'outstanding_challenges'),
         ('index', 'outstanding_challenges_exp_idx'),
         ('trigger', 'used_issuer_matches_wal'),
         ('trigger', 'used_sequence_must_advance'),
         ('trigger', 'used_advance_high_water'),
         ('trigger', 'used_immutable'),
         ('trigger', 'used_no_delete'),
         ('trigger', 'issuer_seq_no_delete'),
         ('trigger', 'issuer_seq_insert_must_match_ledger'),
         ('trigger', 'issuer_seq_only_via_ledger')
     );
DROP TABLE _mig0046_drift_guard;

-- Drop the 0003 triggers; they reference NEW.sequence as if it were
-- NOT NULL and would fire incorrectly against a NULL-sequence row.
DROP TRIGGER IF EXISTS used_issuer_matches_wal;
DROP TRIGGER IF EXISTS used_sequence_must_advance;
DROP TRIGGER IF EXISTS used_advance_high_water;
DROP TRIGGER IF EXISTS used_immutable;
DROP TRIGGER IF EXISTS used_no_delete;
DROP TRIGGER IF EXISTS issuer_seq_no_delete;
DROP TRIGGER IF EXISTS issuer_seq_insert_must_match_ledger;
DROP TRIGGER IF EXISTS issuer_seq_only_via_ledger;

-- Rebuild `used` to allow NULL sequence + add `challenge` column.
ALTER TABLE used RENAME TO used_legacy_0046;

CREATE TABLE used (
  operation_id  TEXT NOT NULL PRIMARY KEY,
  nonce         BLOB NOT NULL,
  issuer        TEXT NOT NULL,
  sequence      INTEGER CHECK (sequence IS NULL OR sequence >= 0),
  challenge     BLOB,
  committed_at  INTEGER NOT NULL,
  -- Exactly one mode: either sequence-mode (challenge IS NULL) or
  -- challenge-mode (sequence IS NULL). Mirrors the IDL `oneOf`
  -- between SignedIntent.sequence and SignedIntent.server_challenge.
  CHECK ((sequence IS NULL) <> (challenge IS NULL)),
  UNIQUE (issuer, sequence),
  UNIQUE (issuer, nonce),
  FOREIGN KEY (operation_id) REFERENCES wal_ops(operation_id)
    DEFERRABLE INITIALLY DEFERRED
);

INSERT INTO used (operation_id, nonce, issuer, sequence, challenge, committed_at)
  SELECT operation_id, nonce, issuer, sequence, NULL, committed_at
    FROM used_legacy_0046;

DROP TABLE used_legacy_0046;

-- Cross-table consistency: used.issuer matches wal_ops.issuer.
CREATE TRIGGER used_issuer_matches_wal
  BEFORE INSERT ON used
  FOR EACH ROW
  WHEN NEW.issuer IS NOT (
    SELECT issuer FROM wal_ops WHERE operation_id = NEW.operation_id
  )
BEGIN
  SELECT RAISE(ABORT, 'used.issuer must match wal_ops.issuer for the operation_id');
END;

-- Anti-rewind. NULL sequence (challenge mode) skips the CAS — the
-- challenge nonce is the single-use guarantee for that mode.
CREATE TRIGGER used_sequence_must_advance
  BEFORE INSERT ON used
  FOR EACH ROW
  WHEN NEW.sequence IS NOT NULL
   AND EXISTS (
    SELECT 1 FROM issuer_seq
     WHERE issuer = NEW.issuer
       AND high_water >= NEW.sequence
  )
BEGIN
  SELECT RAISE(ABORT, 'used.sequence must strictly advance issuer_seq.high_water');
END;

-- Atomically advance issuer_seq cache. NULL sequence is a no-op —
-- challenge-mode rows never participate in the per-issuer counter.
CREATE TRIGGER used_advance_high_water
  AFTER INSERT ON used
  FOR EACH ROW
  WHEN NEW.sequence IS NOT NULL
BEGIN
  INSERT INTO issuer_seq (issuer, high_water)
    VALUES (NEW.issuer, NEW.sequence)
    ON CONFLICT(issuer) DO UPDATE
      SET high_water = excluded.high_water
      WHERE excluded.high_water > issuer_seq.high_water;
END;

-- Append-only ledger.
CREATE TRIGGER used_immutable
  BEFORE UPDATE ON used
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'used rows are append-only; UPDATE not permitted');
END;

CREATE TRIGGER used_no_delete
  BEFORE DELETE ON used
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'used rows are append-only; DELETE not permitted');
END;

-- issuer_seq DELETE permitted only when there are no `used` rows for the issuer
-- (i.e., orphan cleanup). Challenge-only rows do not block this — they
-- have NULL sequence and therefore did not contribute to issuer_seq.
CREATE TRIGGER issuer_seq_no_delete
  BEFORE DELETE ON issuer_seq
  FOR EACH ROW
  WHEN EXISTS (
    SELECT 1 FROM used
     WHERE issuer = OLD.issuer
       AND sequence IS NOT NULL
  )
BEGIN
  SELECT RAISE(ABORT, 'cannot delete issuer_seq while `used` has sequence-mode rows for this issuer');
END;

-- Direct INSERT only when matching used row exists.
CREATE TRIGGER issuer_seq_insert_must_match_ledger
  BEFORE INSERT ON issuer_seq
  FOR EACH ROW
  WHEN NOT EXISTS (
    SELECT 1 FROM used
      WHERE issuer = NEW.issuer
        AND sequence = NEW.high_water
  )
BEGIN
  SELECT RAISE(ABORT, 'issuer_seq INSERT must correspond to a sequence-mode row in `used`');
END;

-- UPDATE must align with MAX(used.sequence) for the issuer, ignoring
-- challenge-mode rows whose sequence column is NULL.
CREATE TRIGGER issuer_seq_only_via_ledger
  BEFORE UPDATE ON issuer_seq
  FOR EACH ROW
  WHEN NEW.high_water IS NOT (
    SELECT MAX(sequence) FROM used
     WHERE issuer = NEW.issuer
       AND sequence IS NOT NULL
  )
BEGIN
  SELECT RAISE(ABORT, 'issuer_seq.high_water must equal MAX(used.sequence) for the issuer');
END;

-- Restore PRAGMA legacy_alter_table to its default before the
-- migration runner returns. This pragma is connection-scoped (NOT
-- transaction-scoped), so any later migration in the same `to_latest`
-- run, or any later schema operation on this connection, would
-- otherwise inherit the relaxed rename semantics — the exact drift
-- this localised pragma was meant to bound. Mirrors 0041's restore.
-- (Issue #52 round-8 review #2.)
PRAGMA legacy_alter_table = OFF;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (46, '0046_replay_challenge_mode', '', strftime('%s','now') * 1000);
