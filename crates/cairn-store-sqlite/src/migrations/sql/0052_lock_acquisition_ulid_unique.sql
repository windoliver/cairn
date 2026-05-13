-- Migration 0052: harden the acquisition_ulid identity invariant.
--
-- The fence/release predicates added in #56 round 7 key on
-- acquisition_ulid as the per-acquisition unique identity, but
-- migration 0051 only added a non-UNIQUE index. A misbehaving
-- third-party tool, or a future refactor that bypasses acquire(),
-- could then insert duplicate ULIDs and let one handle validate or
-- delete another holder's row.
--
-- This migration:
--   1. Drops any leftover pre-v3 sentinel rows (init_incarnation
--      should have GC'd them already by the time this runs).
--   2. Drops the non-unique 0051 index.
--   3. Recreates it as a UNIQUE index.
--   4. Adds a trigger that rejects new inserts whose acquisition_ulid
--      is the pre-v3 sentinel prefix — defense-in-depth against any
--      caller that bypasses acquire()'s ULID minting.

-- Step 1: any rows still tagged as pre-v3 are by definition unowned by
-- the current incarnation (init_incarnation deletes by owner_incarnation
-- mismatch on every Store::open). Drop them so the UNIQUE index in
-- step 3 cannot trip on duplicate sentinel-prefixed values.
DELETE FROM lock_holders WHERE acquisition_ulid LIKE '__pre_v3_%';

-- Step 2 + 3: replace the non-unique 0051 index with a UNIQUE one.
DROP INDEX IF EXISTS lock_holders_acquisition_ulid_idx;
CREATE UNIQUE INDEX lock_holders_acquisition_ulid_idx
  ON lock_holders(acquisition_ulid);

-- Step 4: defense-in-depth — never accept a sentinel-prefixed ULID as
-- a fresh insert. acquire() always mints a real ULID; this guards
-- against direct SQL or future code regression.
CREATE TRIGGER lock_holders_reject_sentinel_ulid
  BEFORE INSERT ON lock_holders
  FOR EACH ROW
  WHEN NEW.acquisition_ulid LIKE '__pre_v3_%'
       OR NEW.acquisition_ulid = '__pre_v3__'
       OR NEW.acquisition_ulid = ''
BEGIN
  SELECT RAISE(ABORT, 'lock_holders.acquisition_ulid must be a real ULID, not the pre-v3 sentinel or empty');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (52, '0052_lock_acquisition_ulid_unique', '', strftime('%s','now') * 1000);
