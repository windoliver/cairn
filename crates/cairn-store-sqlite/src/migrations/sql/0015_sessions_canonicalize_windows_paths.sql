-- Migration 0015: canonicalize legacy Windows-shape `project_root`
-- values to backslash form so they match the canonical strings produced
-- by `SessionIdentity::new`.
-- Brief source: §8.1 Session Lifecycle (canonical project_root).
--
-- The current `normalize_project_root` rewrites `C:/repo` → `C:\repo`
-- and `//srv/share` → `\\srv\share` before lookup/insert, but the store
-- keys the lookup query (`project_root IS ?3`) and the partial unique
-- index (`COALESCE(project_root, '')`) on the raw stored string. Legacy
-- rows stored as `C:/repo` (or with mixed slashes) would therefore stop
-- matching post-upgrade callers normalized to `C:\repo`, leaving the
-- old session active but unreachable while resolve_or_create mints a
-- replacement — a silent session split.
--
-- Step 1: deduplicate. After canonicalization, two legacy rows like
-- `C:/repo` and `C:\repo` for the same `(user, agent)` collapse to one
-- canonical key and would violate
-- `sessions_one_active_per_identity_idx`. Mirror migration 0013's
-- ROW_NUMBER pattern over the canonical key, end all but the newest
-- (last_activity_at DESC, session_id DESC) per partition.
--
-- Step 2: rewrite. Update the survivors so their stored
-- `project_root` matches the canonical form. Restrict the rewrite to
-- Windows-shape rows (drive `_:/...` or UNC `//...`) so POSIX paths
-- containing `/` are not corrupted into `\`.

WITH canonical AS (
  SELECT
    s.session_id,
    s.user_id,
    s.agent_id,
    s.last_activity_at,
    -- Canonicalize only Windows-shape rows. POSIX rows pass through.
    CASE
      WHEN s.project_root LIKE '_:/%' OR s.project_root LIKE '_:\%'
        THEN REPLACE(s.project_root, '/', '\')
      WHEN s.project_root LIKE '//%' OR s.project_root LIKE '\\%'
        THEN REPLACE(s.project_root, '/', '\')
      ELSE s.project_root
    END AS canon_root
  FROM sessions s
  WHERE s.ended_at IS NULL
), ranked AS (
  SELECT
    session_id,
    ROW_NUMBER() OVER (
      PARTITION BY user_id, agent_id, COALESCE(canon_root, '')
      ORDER BY last_activity_at DESC, session_id DESC
    ) AS rn
  FROM canonical
)
UPDATE sessions
   SET ended_at = strftime('%s','now') * 1000
 WHERE session_id IN (SELECT session_id FROM ranked WHERE rn > 1);

-- Step 2: rewrite survivors. Cover every Windows-shape row — drive in
-- either spelling (`_:/%`, `_:\%`) and UNC in either spelling (`//%`,
-- `\\%`) — so mixed-slash legacy values like `C:\foo/bar` and
-- `\\srv/share/sub` also collapse to the canonical backslash form
-- `SessionIdentity::new` produces. REPLACE on a string with no `/` is
-- a no-op, so listing already-canonical shapes here is harmless. Limit
-- to active rows so already-ended legacy rows keep their original
-- audit trail.
UPDATE sessions
   SET project_root = REPLACE(project_root, '/', '\')
 WHERE ended_at IS NULL
   AND project_root IS NOT NULL
   AND ( project_root LIKE '_:/%'
      OR project_root LIKE '_:\%'
      OR project_root LIKE '//%'
      OR project_root LIKE '\\%' );

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (15, '0015_sessions_canonicalize_windows_paths', '', strftime('%s','now') * 1000);
