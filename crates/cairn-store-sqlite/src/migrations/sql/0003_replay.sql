-- Migration 0003: replay-attack ledger.
-- Brief source: §4.2.

CREATE TABLE used (
  operation_id  TEXT NOT NULL PRIMARY KEY,
  nonce         BLOB NOT NULL,
  issuer        TEXT NOT NULL,
  sequence      INTEGER NOT NULL CHECK (sequence >= 0),
  committed_at  INTEGER NOT NULL,
  UNIQUE (issuer, sequence),
  UNIQUE (issuer, nonce),
  FOREIGN KEY (operation_id) REFERENCES wal_ops(operation_id)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE issuer_seq (
  issuer      TEXT NOT NULL PRIMARY KEY,
  high_water  INTEGER NOT NULL CHECK (high_water >= 0)
);

CREATE TABLE outstanding_challenges (
  issuer      TEXT NOT NULL,
  challenge   BLOB NOT NULL,
  expires_at  INTEGER NOT NULL,
  PRIMARY KEY (issuer, challenge)
);
CREATE INDEX outstanding_challenges_exp_idx ON outstanding_challenges(expires_at);

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

-- Anti-rewind.
CREATE TRIGGER used_sequence_must_advance
  BEFORE INSERT ON used
  FOR EACH ROW
  WHEN EXISTS (
    SELECT 1 FROM issuer_seq
     WHERE issuer = NEW.issuer
       AND high_water >= NEW.sequence
  )
BEGIN
  SELECT RAISE(ABORT, 'used.sequence must strictly advance issuer_seq.high_water');
END;

-- Atomically advance issuer_seq cache.
CREATE TRIGGER used_advance_high_water
  AFTER INSERT ON used
  FOR EACH ROW
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
-- (i.e., orphan cleanup).
CREATE TRIGGER issuer_seq_no_delete
  BEFORE DELETE ON issuer_seq
  FOR EACH ROW
  WHEN EXISTS (SELECT 1 FROM used WHERE issuer = OLD.issuer)
BEGIN
  SELECT RAISE(ABORT, 'cannot delete issuer_seq while `used` has rows for this issuer');
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
  SELECT RAISE(ABORT, 'issuer_seq INSERT must correspond to a row in `used`');
END;

-- UPDATE must align with MAX(used.sequence) for the issuer.
CREATE TRIGGER issuer_seq_only_via_ledger
  BEFORE UPDATE ON issuer_seq
  FOR EACH ROW
  WHEN NEW.high_water IS NOT (
    SELECT MAX(sequence) FROM used WHERE issuer = NEW.issuer
  )
BEGIN
  SELECT RAISE(ABORT, 'issuer_seq.high_water must equal MAX(used.sequence) for the issuer');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (3, '0003_replay', '', strftime('%s','now') * 1000);
