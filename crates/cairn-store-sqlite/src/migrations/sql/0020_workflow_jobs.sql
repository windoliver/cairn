-- Migration 0020: workflow_jobs durable job table for the tokio orchestrator.
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
  -- `attempts` counts post-start *executions* (incremented on first
  -- heartbeat, fail, or complete). `delivery_count` counts *leases* —
  -- including leases that crashed before the worker began. `attempts`
  -- governs success/permanent-fail logic; `delivery_count` exists so
  -- repeated pre-heartbeat crashes eventually dead-letter instead of
  -- looping forever (cap = max_attempts, same ceiling as executions).
  attempts            INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  delivery_count      INTEGER NOT NULL DEFAULT 0 CHECK (delivery_count >= 0),
  max_attempts        INTEGER NOT NULL CHECK (max_attempts >= 1),
  -- Persisted retry policy (§10 v0.1 row: exponential retry).
  -- Stored per-row so non-default policies survive restart.
  base_backoff_ms     INTEGER NOT NULL CHECK (base_backoff_ms >= 0),
  backoff_multiplier  INTEGER NOT NULL CHECK (backoff_multiplier >= 1),
  max_backoff_ms      INTEGER NOT NULL CHECK (max_backoff_ms >= 0),
  -- Single-writer queue per key (§10 v0.1 row): jobs sharing a queue_key
  -- serialize through the same lease at most once at a time.
  queue_key           TEXT,
  -- Idempotency tag — workflow-supplied step id (e.g. operation_id).
  dedupe_key          TEXT,
  -- Earliest time the job becomes eligible for lease, ms since epoch.
  next_run_at         INTEGER NOT NULL,
  -- Lease identity. `lease_nonce` is a fresh ULID minted on every lease;
  -- it stays stable across heartbeats so a worker's `LeaseToken` remains
  -- valid for `complete`/`fail` after extending the deadline. The reaper
  -- clears all four fields when the row returns to `queued`.
  --
  -- `lease_started` separates "delivered to a worker" from "the worker
  -- actually began executing" — the latter only flips on the first
  -- successful heartbeat. The reaper uses this to decide whether an
  -- expired lease should consume retry budget (started=1) or just go
  -- back to the queue with attempts unchanged (started=0). Without this
  -- split, an infrastructure crash between lease and first-execution
  -- could permanently fail a job after a few retry-storm cycles.
  lease_owner         TEXT,
  lease_nonce         TEXT,
  lease_started       INTEGER,                            -- 0 | 1 | NULL
  lease_expires_at    INTEGER,
  -- Telemetry / replay aids.
  last_error          TEXT,
  enqueued_at         INTEGER NOT NULL,
  updated_at          INTEGER NOT NULL,
  CHECK (
    (state = 'queued'  AND lease_owner IS NULL AND lease_nonce IS NULL
                       AND lease_started IS NULL AND lease_expires_at IS NULL)
    OR (state = 'leased' AND lease_owner IS NOT NULL AND lease_nonce IS NOT NULL
                       AND lease_started IN (0, 1) AND lease_expires_at IS NOT NULL)
    OR (state = 'done'   AND lease_owner IS NULL AND lease_nonce IS NULL
                       AND lease_started IS NULL AND lease_expires_at IS NULL)
    OR (state = 'failed' AND lease_owner IS NULL AND lease_nonce IS NULL
                       AND lease_started IS NULL AND lease_expires_at IS NULL)
  ),
  CHECK (attempts <= max_attempts),
  CHECK (delivery_count >= attempts)
);

-- Lease scan: pick eligible queued rows ordered by next_run_at.
CREATE INDEX workflow_jobs_ready_idx
  ON workflow_jobs(next_run_at)
  WHERE state = 'queued';

-- Per-key head-of-line probe. The keyed lease query has two correlated
-- predicates that walk queued siblings of the candidate row: (a) "is
-- there an older queued sibling for this queue_key?" (FIFO check) and
-- (b) "is any sibling currently leased?" (the leased-only unique index
-- already covers (b)). Without an index keyed on queue_key, (a) becomes
-- a full table scan while the writer lock is held — backlog quickly
-- starves heartbeats. This partial index gives both predicates an
-- O(log n) lookup keyed on (queue_key, enqueued_at).
CREATE INDEX workflow_jobs_queued_queue_key_idx
  ON workflow_jobs(queue_key, enqueued_at)
  WHERE state = 'queued' AND queue_key IS NOT NULL;

-- Reaper scan: leased rows whose lease has expired.
CREATE INDEX workflow_jobs_lease_expiry_idx
  ON workflow_jobs(lease_expires_at)
  WHERE state = 'leased';

-- Single-writer queue: at most one *leased* row per queue_key. The
-- scheduler is allowed to durably queue many jobs sharing a key —
-- they serialize through the lease step, not at enqueue time. This
-- matches the brief §10 v0.1 row's "jobs sharing a queue_key
-- serialize through the same lease at most once at a time" — a real
-- per-key queue, not a per-key mutex that drops follow-up work.
-- queue_key NULL is treated as no queue (multiple jobs allowed).
CREATE UNIQUE INDEX workflow_jobs_queue_key_leased_uniq
  ON workflow_jobs(queue_key)
  WHERE queue_key IS NOT NULL AND state = 'leased';

-- Step-level idempotency: at most one row per (kind, dedupe_key)
-- whose terminal disposition is not `failed`. queued|leased|done
-- all occupy the dedupe slot, so:
--   * a completed `done` operation cannot be silently re-enqueued
--     under the same `operation_id` (the slot stays held — second
--     enqueue surfaces as DuplicateDedupeKey, which is exactly the
--     short-circuit the brief §10 v0.1 row promises).
--   * a `failed` row does not block replay — operators can safely
--     re-drive the same logical operation after a terminal failure
--     without hand-allocating a new key.
-- Without `done` in the predicate, a timed-out caller that retries
-- after the first worker successfully completed could run the same
-- side effect twice; that is the case this index must close.
CREATE UNIQUE INDEX workflow_jobs_dedupe_uniq
  ON workflow_jobs(kind, dedupe_key)
  WHERE dedupe_key IS NOT NULL AND state IN ('queued','leased','done');

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

-- Hard deletes bypass the state machine. Deleting a queued/leased row
-- silently drops durable work; deleting a `done` row frees its
-- (kind, dedupe_key) slot so the same logical operation could be
-- re-enqueued and run its side effects again. Reject all DELETEs at
-- the trigger layer — operators must follow the documented retention
-- path.
--
-- **Retention is deferred.** P0 keeps all `done`/`failed` rows in the
-- hot table; the dedupe index grows monotonically. A follow-up
-- migration introduces a `workflow_dedupe_tombstones` table so the
-- main table can be compacted without reopening the duplicate-side-
-- effect hole this trigger closes. Tracked under issue #89's
-- retention follow-up; do not silently work around this trigger.
CREATE TRIGGER workflow_jobs_no_delete
  BEFORE DELETE ON workflow_jobs
  FOR EACH ROW
BEGIN
  SELECT RAISE(ABORT, 'workflow_jobs rows cannot be deleted directly');
END;

INSERT INTO schema_migrations (migration_id, name, sql_hash, applied_at)
  VALUES (20, '0020_workflow_jobs', '', strftime('%s','now') * 1000);
