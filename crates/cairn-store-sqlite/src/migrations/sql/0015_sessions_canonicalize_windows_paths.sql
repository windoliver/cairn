-- Migration 0015: canonicalize legacy Windows-shape `project_root`
-- values to the backslash + trimmed-trailing-separator form
-- `SessionIdentity::new` produces.
-- Brief source: §8.1 Session Lifecycle (canonical project_root).
--
-- `normalize_project_root` collapses every Windows-shape variant to one
-- string: `C:\repo`, `C:/repo`, `C:\repo\`, `C:/repo/`, and the
-- mixed-slash forms in between all become `C:\repo`. The store keys
-- the lookup query (`project_root IS ?3`) and the partial unique index
-- (`COALESCE(project_root, '')`) on the raw stored string, so any
-- legacy row that doesn't already match the runtime canonical form
-- becomes unreachable post-upgrade — a silent session split that
-- defeats the §8.1 single-active-session invariant and leaves explicit
-- ids effectively unrecoverable.
--
-- Two-step migration:
--
-- Step 1 (dedup): partition active rows on the canonical form and
-- end all but the newest per partition. Mirrors migration 0013's
-- ROW_NUMBER pattern. Without this, the rewrite step would violate
-- `sessions_one_active_per_identity_idx` whenever two legacy
-- spellings of the same path coexist.
--
-- Step 2 (rewrite): rewrite every Windows-shape row — active or
-- ended — to the canonical form. Ended rows must be rewritten too so
-- `resolve_explicit_session` can still match them: that path checks
-- identity equality before the ended-state check, so an ended row
-- still carrying the legacy raw string would surface as
-- `SessionIdentityMismatch` (a security-class error) instead of the
-- `SessionEnded` the caller expects. Both error variants exist so
-- callers can distinguish "your id is foreign" from "your id is over"
-- — collapsing them silently would defeat that distinction.
--
-- The canonical form combines: REPLACE('/', '\') to collapse slash
-- spellings, then RTRIM(_, '\') to drop trailing separators — except
-- for the drive-root case (`C:\` is exactly 3 chars and trimming it
-- to `C:` would make it drive-relative). UNC roots like `\\` are not
-- representable as a meaningful project root on their own, so the
-- runtime never produces them and the migration doesn't try to
-- preserve them.

WITH canonical AS (
  SELECT
    s.session_id,
    s.user_id,
    s.agent_id,
    s.last_activity_at,
    -- Canonicalize only Windows-shape rows. POSIX rows pass through
    -- (their `/` characters are real path separators that must not
    -- become `\`, and trailing `\` on POSIX is a filename character
    -- that must not be trimmed).
    CASE
      WHEN s.project_root IS NULL THEN NULL
      WHEN s.project_root LIKE '_:/%'
        OR s.project_root LIKE '_:\%'
        OR s.project_root LIKE '//%'
        OR s.project_root LIKE '\\%' THEN
          CASE
            -- Drive root `X:\` (after slash-collapse, length 3) is
            -- already canonical and must not have its trailing `\`
            -- trimmed.
            WHEN length(REPLACE(s.project_root, '/', '\')) = 3
                 AND REPLACE(s.project_root, '/', '\') LIKE '_:\'
              THEN REPLACE(s.project_root, '/', '\')
            ELSE RTRIM(REPLACE(s.project_root, '/', '\'), '\')
          END
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

-- Step 2: rewrite every Windows-shape row (active OR ended) to the
-- canonical form. Same two-step CASE: slash-collapse, then trim
-- trailing `\` except when the result is the bare drive root.
UPDATE sessions
   SET project_root = (
     CASE
       WHEN length(REPLACE(project_root, '/', '\')) = 3
            AND REPLACE(project_root, '/', '\') LIKE '_:\'
         THEN REPLACE(project_root, '/', '\')
       ELSE RTRIM(REPLACE(project_root, '/', '\'), '\')
     END
   )
 WHERE project_root IS NOT NULL
   AND ( project_root LIKE '_:/%'
      OR project_root LIKE '_:\%'
      OR project_root LIKE '//%'
      OR project_root LIKE '\\%' );

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (15, '0015_sessions_canonicalize_windows_paths', '', strftime('%s','now') * 1000);
