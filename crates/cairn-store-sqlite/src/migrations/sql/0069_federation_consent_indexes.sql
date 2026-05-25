-- Federation consent lookup indexes (issue #123).
-- Covers the four ConsentLookup queries added in T12:
-- find_federation_accept, is_link_revoked, find_share_link, find_revocation.
--
-- Existing indexes:
--   consent_journal_kind_idx      ON consent_journal(kind, decided_at)  — single-column kind
--   consent_journal_subject_scope_idx ON consent_journal(subject, scope, decided_at) — no kind
--   workflow_jobs_dedupe_uniq     ON workflow_jobs(kind, dedupe_key) WHERE ... — partial, active states only
--
-- New indexes:
--   consent_journal_federation_kind_subject_idx  — composite (kind, subject) filtered to federation
--     kinds; covers find_federation_accept / is_link_revoked / find_share_link / find_revocation.
--   workflow_jobs_dedupe_key_idx  — non-partial lookup on dedupe_key for JOIN in find_share_link
--     and find_revocation (which need to locate jobs in any state, including failed/cancelled).

-- Composite index on (kind, subject) for federation event lookups.
-- Covers: find_federation_accept(kind='federation_accept', subject='share_link:...'),
--         is_link_revoked(kind='federation_revoke', subject='share_link:...'),
--         find_share_link(kind='federation_grant', subject='share_link:...'),
--         find_revocation(kind='federation_revoke', subject='share_link:...').
CREATE INDEX IF NOT EXISTS consent_journal_federation_kind_subject_idx
  ON consent_journal(kind, subject)
  WHERE kind IN ('federation_grant', 'federation_accept', 'federation_revoke');

-- Index on workflow_jobs.dedupe_key for the JOIN in find_share_link / find_revocation.
-- The existing workflow_jobs_dedupe_uniq covers only active states (queued/leased/done);
-- these queries need to locate completed or failed jobs too, so an unrestricted index is needed.
CREATE INDEX IF NOT EXISTS workflow_jobs_dedupe_key_idx
  ON workflow_jobs(dedupe_key)
  WHERE dedupe_key IS NOT NULL;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (69, '0069_federation_consent_indexes', '', strftime('%s','now') * 1000);
