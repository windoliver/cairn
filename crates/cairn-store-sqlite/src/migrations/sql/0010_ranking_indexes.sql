-- Migration 0010: indexes for ranking-input lookups.
-- Brief source: §5.1 (Rank & Filter — recency, confidence, salience).

CREATE INDEX records_confidence_idx
  ON records(confidence)
  WHERE active = 1 AND tombstoned = 0;

CREATE INDEX records_updated_at_idx
  ON records(updated_at)
  WHERE active = 1 AND tombstoned = 0;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (10, '0010_ranking_indexes', '', strftime('%s','now') * 1000);
