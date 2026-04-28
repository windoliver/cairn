-- Migration 0009: add record_json column to records for full MemoryRecord
-- round-trip. The individual columns (body, provenance, etc.) are retained
-- for SQL filtering; record_json is the canonical round-trip form for reads.
ALTER TABLE records ADD COLUMN record_json TEXT;

-- Best-effort backfill from existing column data. Pre-0009 rows lacked
-- columns for `id`, `signature`, `tags`, and `extra_frontmatter`, so the
-- reconstructed JSON uses placeholder values; downstream
-- `MemoryRecord::deserialize` will reject those rows. The reset is
-- documented in the brief migration notes (no shipped DBs predate 0009).
UPDATE records
SET record_json = json_object(
        'id', record_id,
        'kind', json_extract(taxonomy, '$.kind'),
        'class', json_extract(taxonomy, '$.class'),
        'visibility', json_extract(taxonomy, '$.visibility'),
        'scope', json(scope),
        'body', body,
        'provenance', json(provenance),
        'updated_at', created_at,
        'evidence', json(evidence),
        'salience', salience,
        'confidence', confidence,
        'actor_chain', json(actor_chain),
        'signature', 'ed25519:legacy-pre-0009',
        'tags', json('[]'),
        'extra_frontmatter', json('{}')
    )
WHERE record_json IS NULL;
