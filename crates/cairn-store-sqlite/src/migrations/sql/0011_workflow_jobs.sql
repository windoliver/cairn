-- Migration 0011: workflow_jobs durable job table for the tokio orchestrator.
-- Brief sources: §10 (Continuous Learning truth table — v0.1 row), §19.a item 5
-- (tokio orchestrator), §4 row 3 (WorkflowOrchestrator contract).
--
-- A row tracks one queued/in-flight workflow invocation. The scheduler in
-- `cairn-workflows` polls this table, atomically transitions queued -> leased
-- via UPDATE...RETURNING with a CAS on (state, lease_owner), heartbeats
-- lease_expires_at while running, and terminates with state = done | failed.
-- A reaper sweep moves leased rows whose lease_expires_at < now back to
-- queued, capped by max_attempts.
--
-- Step-level idempotency uses `dedupe_key` (brief §10 v0.1 row: "step-level
-- idempotency via operation_id"). A unique index over (kind, dedupe_key)
-- where dedupe_key IS NOT NULL prevents duplicate enqueue.

CREATE TABLE workflow_jobs (
  job_id              TEXT    NOT NULL PRIMARY KEY,        -- ULID
  kind                TEXT    NOT NULL,                    -- workflow discriminator (e.g. 'dream.light')
  payload             BLOB    NOT NULL,                    -- opaque; deserialized by the workflow
  state               TEXT    NOT NULL CHECK (state IN ('queued','leased','done','failed')),
  attempts            INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  max_attempts        INTEGER NOT NULL CHECK (max_attempts >= 1),
  -- Single-writer queue per key (§10 v0.1 row): jobs sharing a queue_key
  -- serialize through the same lease at most once at a time.
  queue_key           TEXT,
  -- Idempotency tag — workflow-supplied step id (e.g. operation_id).
  dedupe_key          TEXT,
  -- Earliest time the job becomes eligible for lease, ms since epoch.
  next_run_at         INTEGER NOT NULL,
  -- Set when state = 'leased'. NULL otherwise.
  lease_owner         TEXT,
  lease_expires_at    INTEGER,
  -- Telemetry / replay aids.
  last_error          TEXT,
  enqueued_at         INTEGER NOT NULL,
  updated_at          INTEGER NOT NULL,
  CHECK (
    (state = 'queued'  AND lease_owner IS NULL AND lease_expires_at IS NULL)
    OR (state = 'leased' AND lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL)
    OR (state = 'done'   AND lease_owner IS NULL AND lease_expires_at IS NULL)
    OR (state = 'failed' AND lease_owner IS NULL AND lease_expires_at IS NULL)
  ),
  CHECK (attempts <= max_attempts)
);

-- Lease scan: pick eligible queued rows ordered by next_run_at.
CREATE INDEX workflow_jobs_ready_idx
  ON workflow_jobs(next_run_at)
  WHERE state = 'queued';

-- Reaper scan: leased rows whose lease has expired.
CREATE INDEX workflow_jobs_lease_expiry_idx
  ON workflow_jobs(lease_expires_at)
  WHERE state = 'leased';

-- Single-writer queue: at most one queued/leased row per queue_key.
-- queue_key NULL is treated as no queue (multiple jobs allowed).
CREATE UNIQUE INDEX workflow_jobs_queue_key_active_uniq
  ON workflow_jobs(queue_key)
  WHERE queue_key IS NOT NULL AND state IN ('queued','leased');

-- Step-level idempotency: at most one row per (kind, dedupe_key) when set.
CREATE UNIQUE INDEX workflow_jobs_dedupe_uniq
  ON workflow_jobs(kind, dedupe_key)
  WHERE dedupe_key IS NOT NULL;

-- Identity columns immutable after insert.
CREATE TRIGGER workflow_jobs_identity_immutable
  BEFORE UPDATE ON workflow_jobs
  FOR EACH ROW
  WHEN NEW.job_id      IS NOT OLD.job_id
    OR NEW.kind        IS NOT OLD.kind
    OR NEW.enqueued_at IS NOT OLD.enqueued_at
    OR NEW.dedupe_key  IS NOT OLD.dedupe_key
    OR NEW.queue_key   IS NOT OLD.queue_key
BEGIN
  SELECT RAISE(ABORT, 'workflow_jobs identity columns are immutable');
END;

-- Terminal states (done/failed) are absorbing — no further transitions.
CREATE TRIGGER workflow_jobs_terminal_absorbing
  BEFORE UPDATE OF state ON workflow_jobs
  FOR EACH ROW
  WHEN OLD.state IN ('done','failed') AND NEW.state IS NOT OLD.state
BEGIN
  SELECT RAISE(ABORT, 'workflow_jobs terminal-state rows are absorbing');
END;

-- Allowed transitions: queued <-> leased, leased -> done|failed,
-- leased -> queued (lease expiry / explicit retry).
CREATE TRIGGER workflow_jobs_state_transition
  BEFORE UPDATE OF state ON workflow_jobs
  FOR EACH ROW
  WHEN NEW.state IS NOT OLD.state
   AND NOT (OLD.state = 'queued' AND NEW.state = 'leased')
   AND NOT (OLD.state = 'leased' AND NEW.state IN ('queued','done','failed'))
BEGIN
  SELECT RAISE(ABORT, 'workflow_jobs.state transition not allowed');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (11, '0011_workflow_jobs', '', strftime('%s','now') * 1000);
