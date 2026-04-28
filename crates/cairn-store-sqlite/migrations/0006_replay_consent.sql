CREATE TABLE replay_ledger (
  nonce      TEXT NOT NULL PRIMARY KEY,
  issuer     TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  expires_at TEXT NOT NULL,
  seen_at    TEXT NOT NULL
) STRICT;

CREATE TABLE issuer_seq (
  issuer     TEXT NOT NULL PRIMARY KEY,
  last_seq   INTEGER NOT NULL,
  updated_at TEXT NOT NULL
) STRICT;

CREATE TABLE challenges (
  challenge_id TEXT NOT NULL PRIMARY KEY,
  issued_at    TEXT NOT NULL,
  expires_at   TEXT NOT NULL,
  consumed_at  TEXT
) STRICT;

CREATE TABLE consent_journal (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  op_id       TEXT NOT NULL,
  kind        TEXT NOT NULL,
  target_id   TEXT,
  actor       TEXT NOT NULL,
  payload     TEXT NOT NULL,
  at          TEXT NOT NULL
);

CREATE INDEX consent_journal_op_idx ON consent_journal(op_id);
CREATE INDEX consent_journal_target_idx ON consent_journal(target_id);
