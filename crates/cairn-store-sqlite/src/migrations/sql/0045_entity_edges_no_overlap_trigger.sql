-- Migration 0045: schema-level overlap protection on entity_edges.
-- Issue #186 round-9 review fix.
--
-- Round-7 added an API guard against bounded-overlap upserts; round-8
-- added the same guard to resolve_contradiction. Both are necessary
-- but not sufficient: raw SQL writes (this migration's own precheck,
-- a future repair script, a misbehaving plugin) can still persist
-- overlapping non-expired rows for the same triple, after which
-- `graph_edges` returns duplicate facts at any as-of read inside the
-- overlap. Defense in depth requires the schema to reject the same
-- patterns the API rejects.
--
-- Two triggers, BEFORE INSERT and BEFORE UPDATE, applied only when
-- NEW.expired_at IS NULL — expired (tombstoned) rows are out of the
-- bitemporal slicing window and may legitimately coexist with live
-- successors. The overlap predicate is identical to the Rust API's
-- Probe B and to resolve_contradiction's overlap probe (NULL-aware
-- open-interval semantics, no sentinel encoding).
--
-- Migration-time precheck: reject installation if existing data
-- already violates the invariant (e.g. an earlier branch persisted
-- pre-round-7 corruption). Use the same INSERT-into-CHECK-constrained
-- TEMP table idiom as 0031 to surface a typed error from top-level SQL.

CREATE TEMP TABLE _mig0045_overlap_precheck (
  msg TEXT NOT NULL CHECK (msg = 'no overlap')
);
INSERT INTO _mig0045_overlap_precheck (msg)
  SELECT 'migration 0045: pre-existing overlap on entity_edges id=' || a.id
         || ' overlaps id=' || b.id
  FROM entity_edges a
  JOIN entity_edges b
    ON a.rowid < b.rowid
   AND a.source_id = b.source_id
   AND a.target_id = b.target_id
   AND a.relation  = b.relation
   AND a.expired_at IS NULL
   AND b.expired_at IS NULL
   AND (b.invalid_at IS NULL OR a.valid_at < b.invalid_at)
   AND (a.invalid_at IS NULL OR a.invalid_at > b.valid_at)
  LIMIT 1;
DROP TABLE _mig0045_overlap_precheck;

CREATE TRIGGER entity_edges_no_overlap_insert
  BEFORE INSERT ON entity_edges
  FOR EACH ROW
  WHEN NEW.expired_at IS NULL
BEGIN
  SELECT RAISE(ABORT,
    'entity_edges overlap: another non-expired row exists for this triple in window')
  WHERE EXISTS (
    SELECT 1 FROM entity_edges
    WHERE id != NEW.id
      AND source_id = NEW.source_id
      AND target_id = NEW.target_id
      AND relation  = NEW.relation
      AND expired_at IS NULL
      AND (NEW.invalid_at IS NULL OR valid_at < NEW.invalid_at)
      AND (invalid_at IS NULL OR invalid_at > NEW.valid_at)
  );
END;

CREATE TRIGGER entity_edges_no_overlap_update
  BEFORE UPDATE ON entity_edges
  FOR EACH ROW
  WHEN NEW.expired_at IS NULL
BEGIN
  SELECT RAISE(ABORT,
    'entity_edges overlap: another non-expired row exists for this triple in window')
  WHERE EXISTS (
    SELECT 1 FROM entity_edges
    WHERE id != NEW.id
      AND source_id = NEW.source_id
      AND target_id = NEW.target_id
      AND relation  = NEW.relation
      AND expired_at IS NULL
      AND (NEW.invalid_at IS NULL OR valid_at < NEW.invalid_at)
      AND (invalid_at IS NULL OR invalid_at > NEW.valid_at)
  );
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (45, '0045_entity_edges_no_overlap_trigger', '', strftime('%s','now') * 1000);
