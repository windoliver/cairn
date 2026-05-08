-- Migration 0022: sqlite-vec ANN table + embedding backfill queue.
-- Requires the sqlite-vec extension registered at connection open (see open.rs).
-- Brief §3.0: record_vectors is the local ANN index; pending_embeddings
-- is the idempotent backfill queue for failed or deferred embeddings.

CREATE VIRTUAL TABLE record_vectors USING vec0(
  record_id TEXT PRIMARY KEY,
  embedding float[384],
  -- Shadow column: which model produced this embedding.
  -- Semantic search skips rows where model ≠ active model (stale from swap).
  +model TEXT NOT NULL
);

-- Backfill / failure queue. Idempotent on record_id.
-- ON DELETE CASCADE keeps purge clean when the source record is physically deleted.
CREATE TABLE pending_embeddings (
  record_id        TEXT PRIMARY KEY
                     REFERENCES records(record_id) ON DELETE CASCADE,
  reason           TEXT NOT NULL
                     CHECK(reason IN ('embed_failed','opt_in_backfill','model_swap')),
  attempt_count    INTEGER NOT NULL DEFAULT 0,
  last_attempt_at  INTEGER,
  last_error       TEXT,
  enqueued_at      INTEGER NOT NULL
);

CREATE INDEX pending_embeddings_enqueued_idx
  ON pending_embeddings(enqueued_at, attempt_count);

-- Cascade vector cleanup when a record is physically deleted (Phase B purge).
CREATE TRIGGER records_vector_cleanup
  AFTER DELETE ON records
  BEGIN
    DELETE FROM record_vectors     WHERE record_id = OLD.record_id;
    DELETE FROM pending_embeddings WHERE record_id = OLD.record_id;
  END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (22, '0022_record_vectors', '', strftime('%s','now') * 1000);
