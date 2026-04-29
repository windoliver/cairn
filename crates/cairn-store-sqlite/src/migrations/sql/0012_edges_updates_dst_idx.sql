-- Migration 0012: index `updates`-edge dst lookups.
-- Brief sources: §3 (records-in-SQLite), §5.1 (Read path supersession).
--
-- The keyword-search read path runs a correlated
--   NOT EXISTS (SELECT 1 FROM edges WHERE kind = 'updates' AND dst = r.record_id)
-- against every FTS hit to enforce `records_latest` semantics. The base
-- `edges` primary key `(src, dst, kind)` cannot serve a `dst`-leading
-- lookup, so each FTS hit scanned the whole edges table before the
-- LIMIT clause applied. Common terms × large supersession sets made
-- search latency depend on unrelated graph size.
--
-- A partial index keyed on `dst` and gated by `kind = 'updates'` makes
-- the supersession check `O(log E)` per hit and confines the index to
-- the supersession-edge subset (avoiding bloat from `mentions` /
-- `supports` edges that are not consulted by this predicate).

CREATE INDEX edges_updates_dst_idx
  ON edges(dst)
  WHERE kind = 'updates';

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (12, '0012_edges_updates_dst_idx', '', strftime('%s','now') * 1000);
