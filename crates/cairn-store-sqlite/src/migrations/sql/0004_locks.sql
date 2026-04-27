-- Migration 0004: vault locks + reader fence.
-- Brief source: §5.6 + §3.0 (single-writer + reader-fence semantics).

CREATE TABLE locks (
  resource      TEXT NOT NULL PRIMARY KEY,
  mode          TEXT NOT NULL CHECK (mode IN ('NONE','SHARED','EXCLUSIVE')),
  holder_count  INTEGER NOT NULL DEFAULT 0 CHECK (holder_count >= 0),
  updated_at    INTEGER NOT NULL,
  CHECK (
    (mode = 'NONE'      AND holder_count = 0)
    OR (mode = 'SHARED'    AND holder_count >= 1)
    OR (mode = 'EXCLUSIVE' AND holder_count = 1)
  )
);

CREATE TABLE lock_holders (
  resource        TEXT NOT NULL,
  holder_id       TEXT NOT NULL,
  mode_requested  TEXT NOT NULL CHECK (mode_requested IN ('SHARED','EXCLUSIVE')),
  acquired_at     INTEGER NOT NULL,
  expires_at      INTEGER NOT NULL,
  PRIMARY KEY (resource, holder_id),
  FOREIGN KEY (resource) REFERENCES locks(resource)
);

CREATE INDEX lock_holders_expiry_idx ON lock_holders(expires_at);

-- Exclusive holder must be alone.
CREATE TRIGGER lock_holders_exclusive_only_alone
  BEFORE INSERT ON lock_holders
  FOR EACH ROW
  WHEN NEW.mode_requested = 'EXCLUSIVE'
   AND EXISTS (SELECT 1 FROM lock_holders WHERE resource = NEW.resource)
BEGIN
  SELECT RAISE(ABORT, 'EXCLUSIVE holder requires no existing holders for resource');
END;

-- Shared holder cannot coexist with EXCLUSIVE.
CREATE TRIGGER lock_holders_shared_blocked_by_exclusive
  BEFORE INSERT ON lock_holders
  FOR EACH ROW
  WHEN NEW.mode_requested = 'SHARED'
   AND EXISTS (
     SELECT 1 FROM lock_holders
      WHERE resource = NEW.resource AND mode_requested = 'EXCLUSIVE'
   )
BEGIN
  SELECT RAISE(ABORT, 'SHARED holder blocked by existing EXCLUSIVE holder');
END;

-- Identity columns immutable.
CREATE TRIGGER lock_holders_keys_immutable
  BEFORE UPDATE ON lock_holders
  FOR EACH ROW
  WHEN NEW.resource       IS NOT OLD.resource
    OR NEW.holder_id      IS NOT OLD.holder_id
    OR NEW.mode_requested IS NOT OLD.mode_requested
    OR NEW.acquired_at    IS NOT OLD.acquired_at
BEGIN
  SELECT RAISE(ABORT, 'lock_holders identity columns are immutable');
END;

-- Derive locks.(mode, holder_count) from lock_holders after every change.
CREATE TRIGGER lock_holders_count_after_insert
  AFTER INSERT ON lock_holders
  FOR EACH ROW
BEGIN
  INSERT INTO locks (resource, mode, holder_count, updated_at)
    VALUES (
      NEW.resource,
      NEW.mode_requested,
      1,
      strftime('%s','now') * 1000
    )
    ON CONFLICT(resource) DO UPDATE
      SET mode         = NEW.mode_requested,
          holder_count = locks.holder_count + 1,
          updated_at   = strftime('%s','now') * 1000;
END;

CREATE TRIGGER lock_holders_count_after_delete
  AFTER DELETE ON lock_holders
  FOR EACH ROW
BEGIN
  UPDATE locks
     SET holder_count = holder_count - 1,
         mode = CASE
                  WHEN holder_count - 1 = 0 THEN 'NONE'
                  ELSE mode
                END,
         updated_at = strftime('%s','now') * 1000
   WHERE resource = OLD.resource;
END;

-- Daemon incarnation singleton: exactly one row.
CREATE TABLE daemon_incarnation (
  id              INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
  incarnation_id  TEXT    NOT NULL,
  started_at      INTEGER NOT NULL,
  pid             INTEGER NOT NULL
);

-- Reader fence rows track readers that must drain before tombstoning commits.
CREATE TABLE reader_fence (
  resource     TEXT NOT NULL,
  operation_id TEXT NOT NULL,
  state        TEXT NOT NULL CHECK (state IN ('PENDING','CLEARED')),
  created_at   INTEGER NOT NULL,
  cleared_at   INTEGER,
  PRIMARY KEY (resource, operation_id),
  FOREIGN KEY (operation_id) REFERENCES wal_ops(operation_id)
);

CREATE UNIQUE INDEX reader_fence_pending_idx
  ON reader_fence(resource) WHERE state = 'PENDING';

CREATE TRIGGER reader_fence_state_transition
  BEFORE UPDATE OF state ON reader_fence
  FOR EACH ROW
  WHEN NEW.state IS NOT OLD.state
   AND NOT (OLD.state = 'PENDING' AND NEW.state = 'CLEARED')
BEGIN
  SELECT RAISE(ABORT, 'reader_fence.state transition not allowed');
END;

CREATE TRIGGER reader_fence_identity_immutable
  BEFORE UPDATE ON reader_fence
  FOR EACH ROW
  WHEN NEW.resource     IS NOT OLD.resource
    OR NEW.operation_id IS NOT OLD.operation_id
    OR NEW.created_at   IS NOT OLD.created_at
BEGIN
  SELECT RAISE(ABORT, 'reader_fence identity columns are immutable');
END;

-- Direct DELETE only allowed when the linked op is COMMITTED or ABORTED.
CREATE TRIGGER reader_fence_no_direct_delete
  BEFORE DELETE ON reader_fence
  FOR EACH ROW
  WHEN NOT EXISTS (
    SELECT 1 FROM wal_ops
     WHERE operation_id = OLD.operation_id
       AND state IN ('COMMITTED','ABORTED','REJECTED')
  )
BEGIN
  SELECT RAISE(ABORT, 'reader_fence rows can only be deleted after the linked op terminates');
END;

INSERT INTO schema_migrations (migration_id, name, sql_blake3, applied_at)
  VALUES (4, '0004_locks', '', strftime('%s','now') * 1000);
