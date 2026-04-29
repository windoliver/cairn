-- Migration 0014: end any active session row whose `project_root` is a
-- relative path.
-- Brief source: §8.1 Session Lifecycle (canonical project_root).
--
-- An earlier draft of the resolver permitted relative `project_root`
-- values. The current `SessionIdentity::new` rejects them on the write
-- path, but `SessionIdentity::from_persisted` deliberately tolerates them
-- on read so an upgrade does not corrupt a vault. The lookup query
-- (`project_root IS ?3`) compares the raw stored string to the caller's
-- *canonical absolute* string; a row stored as `subdir/repo` can never
-- match a caller passing `/abs/cwd/subdir/repo`. Such rows therefore
-- become unreachable through both auto-discover and explicit reopening,
-- which silently splits later writes into a fresh session and breaks the
-- §8.1 fail-closed guarantees around explicit ids.
--
-- We can't safely canonicalize the legacy values: the original CWD that
-- made the relative path meaningful is no longer in scope at migration
-- time, and guessing wrong would cross-link two unrelated vaults. The
-- conservative repair is to mark such rows ended so resolve_or_create
-- mints a fresh session on next contact, rather than leaving zombie rows
-- that look active but can never be resolved.
--
-- Detection covers the three absolute-path forms `Path::is_absolute`
-- accepts on the platforms we ship: POSIX `/...`, Windows drive
-- (`X:\...`), and Windows UNC (`\\server\share`). Anything else stored
-- with a non-NULL `project_root` is treated as relative.

UPDATE sessions
   SET ended_at = strftime('%s','now') * 1000
 WHERE ended_at IS NULL
   AND project_root IS NOT NULL
   AND project_root NOT LIKE '/%'
   AND project_root NOT LIKE '\\%'
   AND project_root NOT LIKE '_:\%';

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (14, '0014_sessions_close_relative_project_root', '', strftime('%s','now') * 1000);
