-- Migration 0017: lift pre-0014 single-snapshot markers into the
-- per-version snapshot format introduced by migration 0014.
--
-- Migration 0010 added `scope_snapshot`/`taxonomy_snapshot` columns
-- holding the visibility snapshot of the *latest* version at purge
-- time. Migration 0014 introduced `version_snapshots` (a JSON array
-- of `{scope, taxonomy}` per pre-purge version) so that any
-- principal who could read at least one version sees the marker.
-- The read path falls back to the single-snapshot pair when
-- `version_snapshots IS NULL`, which means pre-0014 markers retain
-- the buggy "latest-only" semantics: a principal who had read access
-- to a superseded version but not the latest can no longer see the
-- marker.
--
-- For pre-0014 markers we genuinely lost the per-version data —
-- `purge_target` deletes the original rows from `records` before any
-- snapshot exists for non-latest versions. The best repair we have
-- is to lift the single-snapshot pair into a one-element
-- `version_snapshots` array, so the read path uniformly evaluates
-- the new format and old markers are no longer treated specially.
-- The "latest-only" visibility semantics persist for those rows
-- because the data needed to widen visibility is unrecoverable, but
-- code paths converge on a single behavior.
UPDATE record_purges
SET version_snapshots = json_array(
    json_object(
        'scope', json(scope_snapshot),
        'taxonomy', json(taxonomy_snapshot)
    )
)
WHERE version_snapshots IS NULL
  AND scope_snapshot IS NOT NULL
  AND taxonomy_snapshot IS NOT NULL;
