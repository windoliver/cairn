-- Migration 0019: strip Windows verbatim-path prefixes and case-fold
-- Windows-shape `project_root` values to match the runtime canonical
-- form `SessionIdentity::new` produces.
-- Brief source: §8.1 Session Lifecycle (canonical project_root).
--
-- Two issues this migration repairs against vaults that already
-- applied 0011..0015:
--
-- 1. Verbatim prefixes (`\\?\C:\repo`, `\\?\UNC\srv\share`) are now
--    stripped at runtime so they key the same identity as their plain
--    forms (`C:\repo`, `\\srv\share`). 0015 didn't replicate this;
--    legacy verbatim rows would stay raw on disk while new callers
--    normalize to the plain form, splitting sessions and confusing
--    explicit-id reopen with a `SessionIdentityMismatch`.
-- 2. Windows file systems (NTFS, SMB shares) are case-insensitive, so
--    the runtime now ASCII-lowercases Windows-shape paths. Legacy
--    rows persisted with mixed case (`C:\Repo`, `\\Srv\Share`) would
--    likewise stop matching post-upgrade callers. SQLite's `LOWER()`
--    folds ASCII only — same behavior as Rust's
--    `to_ascii_lowercase()`, so the two stay in sync without invoking
--    Unicode case mapping.
--
-- Like 0013/0015, this is a two-step migration:
--
-- Step 1 (dedup): partition active rows on the canonical form and
-- end all but the newest per partition. After verbatim-stripping +
-- lowercasing, two legacy rows like `\\?\C:\Repo` and `c:\repo` for
-- the same `(user, agent)` collapse to one canonical key and would
-- violate `sessions_one_active_per_identity_idx`. ROW_NUMBER pattern
-- mirrors 0013/0015.
--
-- Step 2 (rewrite): rewrite every Windows-shape row — active or
-- ended — to the canonical form. Ended rows must be rewritten so
-- `resolve_explicit_session` (which checks identity equality before
-- the ended-state check) returns `SessionEnded` on legacy rows
-- instead of `SessionIdentityMismatch`. Same reasoning as 0015.
--
-- The canonical form combines, in order:
--   a) strip verbatim prefix (`\\?\UNC\X` → `\\X`, `\\?\X:Y` → `X:Y`)
--   b) REPLACE('/', '\') to collapse slash spellings
--   c) RTRIM(_, '\') to drop trailing separators (preserve drive root
--      `X:\` exactly as in 0015)
--   d) LOWER(_) to case-fold
--
-- POSIX rows pass through untouched: their `/` characters are real
-- separators that must not become `\`, trailing `\` is a filename
-- character there, and POSIX is case-sensitive.

WITH stripped AS (
  -- Step a: strip verbatim prefixes. `\\?\UNC\` is 8 chars (substr
  -- start = 9); `\\?\` is 4 chars (substr start = 5).
  SELECT
    s.session_id,
    s.user_id,
    s.agent_id,
    s.last_activity_at,
    s.ended_at,
    CASE
      WHEN s.project_root IS NULL THEN NULL
      WHEN s.project_root LIKE '\\?\UNC\%' THEN '\\' || substr(s.project_root, 9)
      WHEN s.project_root LIKE '\\?\_:\%'
        OR s.project_root LIKE '\\?\_:/%' THEN substr(s.project_root, 5)
      ELSE s.project_root
    END AS pr_stripped
  FROM sessions s
), canonical AS (
  -- Steps b–d: only on Windows-shape rows. After verbatim stripping,
  -- the row may have become a plain Windows shape (e.g.
  -- `\\?\UNC\srv\share` → `\\srv\share`), which is exactly what we
  -- want.
  SELECT
    session_id,
    user_id,
    agent_id,
    last_activity_at,
    ended_at,
    CASE
      WHEN pr_stripped IS NULL THEN NULL
      WHEN pr_stripped LIKE '_:/%'
        OR pr_stripped LIKE '_:\%'
        OR pr_stripped LIKE '\\%' THEN
          LOWER(
            CASE
              WHEN length(REPLACE(pr_stripped, '/', '\')) = 3
                   AND REPLACE(pr_stripped, '/', '\') LIKE '_:\'
                THEN REPLACE(pr_stripped, '/', '\')
              ELSE RTRIM(REPLACE(pr_stripped, '/', '\'), '\')
            END
          )
      ELSE pr_stripped
    END AS canon_root
  FROM stripped
), ranked AS (
  SELECT
    session_id,
    ROW_NUMBER() OVER (
      PARTITION BY user_id, agent_id, COALESCE(canon_root, '')
      ORDER BY last_activity_at DESC, session_id DESC
    ) AS rn
  FROM canonical
  WHERE ended_at IS NULL
)
UPDATE sessions
   SET ended_at = strftime('%s','now') * 1000
 WHERE session_id IN (SELECT session_id FROM ranked WHERE rn > 1);

-- Step 2: rewrite all Windows-shape rows (active OR ended) to the
-- canonical form. Same CASE structure as the dedup CTE; we re-derive
-- inline rather than UPDATE FROM because SQLite's UPDATE-FROM
-- semantics across CTEs are version-fragile.
UPDATE sessions
   SET project_root = (
     -- Strip verbatim, then slash-collapse + trim + lowercase. Drive
     -- root (`X:\`, length 3) must NOT have its trailing `\` trimmed
     -- — `X:` would be drive-relative on Windows, which the runtime
     -- classifier rejects. Same preservation applies after stripping
     -- a verbatim drive prefix: `\\?\C:\` → `C:\` → keep as `c:\`.
     CASE
       WHEN project_root LIKE '\\?\UNC\%' THEN
         LOWER(RTRIM(REPLACE('\\' || substr(project_root, 9), '/', '\'), '\'))
       WHEN project_root LIKE '\\?\_:\%' OR project_root LIKE '\\?\_:/%' THEN
         LOWER(
           CASE
             WHEN length(REPLACE(substr(project_root, 5), '/', '\')) = 3
                  AND REPLACE(substr(project_root, 5), '/', '\') LIKE '_:\'
               THEN REPLACE(substr(project_root, 5), '/', '\')
             ELSE RTRIM(REPLACE(substr(project_root, 5), '/', '\'), '\')
           END
         )
       WHEN length(REPLACE(project_root, '/', '\')) = 3
            AND REPLACE(project_root, '/', '\') LIKE '_:\'
         THEN LOWER(REPLACE(project_root, '/', '\'))
       ELSE LOWER(RTRIM(REPLACE(project_root, '/', '\'), '\'))
     END
   )
 WHERE project_root IS NOT NULL
   AND ( project_root LIKE '_:/%'
      OR project_root LIKE '_:\%'
      OR project_root LIKE '\\%' );

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (19, '0019_sessions_strip_verbatim_and_case_fold', '', strftime('%s','now') * 1000);
