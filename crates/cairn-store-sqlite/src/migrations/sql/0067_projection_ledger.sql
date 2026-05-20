-- Issue #105 — authoritative ledger for rebuildable Nexus projection sidecars.

CREATE TABLE IF NOT EXISTS projection_ledger (
    target TEXT NOT NULL,
    record_id TEXT NOT NULL,
    wal_sequence INTEGER NOT NULL,
    record_hash TEXT NOT NULL,
    source_hash TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL,
    reason TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (target, record_id, record_hash, source_hash)
);

CREATE INDEX IF NOT EXISTS projection_ledger_state_idx
    ON projection_ledger(state);

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
VALUES (67, '0067_projection_ledger', '', strftime('%s','now') * 1000);
