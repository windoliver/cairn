# COW upsert and expire WAL apply — design

- **Issue:** [#57](https://github.com/windoliver/cairn/issues/57)
- **Parent epic:** [#8](https://github.com/windoliver/cairn/issues/8) WAL, locks, replay, record-level forget
- **Phase:** v0.1 P0
- **Brief sections:** §3.0 Atomicity model · §5.6 WAL operations · §10 Continuous Learning
- **Status:** approved design, awaiting implementation plan
- **Date:** 2026-05-08

## 1. Goal

Route record upsert and expiration through the §5.6 WAL apply path so every
record mutation is copy-on-write, replay-safe, and hidden from readers until
its activation or retirement point commits.

The work completes the `upsert` and `expire` side-effect bodies deferred by
#55. It must preserve the current `MemoryStore` public behavior while replacing
the direct-write internals with WAL-backed apply.

## 2. Non-goals

- `forget_record` Phase B purge. This remains a sibling task and must later
  purge any body-bearing WAL payloads described here.
- `promote`, `forget_session`, `evolve`, or graph WAL bodies.
- Distributed P1 Nexus durable messaging. P0 remains one SQLite database.
- Reworking the CLI `FlushPlan` lifecycle beyond consuming already-persisted
  plans when a caller provides a `plan_ref`.

## 3. Source-of-truth alignment

| Brief section | Requirement | Design choice |
|---|---|---|
| §3.0 Atomicity model | P0 writes land in one SQLite authority; derived indexes are rebuildable | `records` remains authoritative; FTS/vector/edge steps are retryable projections |
| §5.6 upsert row | Stage version `N+1` inactive, update derived indexes, then activate | `primary.upsert_cow` inserts inactive rows; `primary.activate` flips the active pointer |
| §5.6 expire row | Expiration retires rows without physical purge | `primary.mark_expired` marks the target lineage tombstoned with reason `expire`; no record rows are deleted |
| §5.6 read fence | Readers filter inactive/tombstoned rows | Existing `active = 1 AND tombstoned = 0` predicates remain the visibility gate |
| §10 workflows | Expirer can retry and rebuild indexes | Expire drains derived indexes idempotently and records WAL step completion |

## 4. Architecture

Add a `record_wal` module inside `cairn-store-sqlite`.

```text
MemoryStore::upsert(record)
  -> record_wal::apply_upsert(record)
     -> issue PREPARED wal_ops row
     -> run UPSERT_STEPS through StepRunner
     -> finalize COMMITTED

SqliteMemoryStore::expire(target, reason)
  -> record_wal::apply_expire(target, reason)
     -> issue PREPARED wal_ops row
     -> run EXPIRE_STEPS through StepRunner
     -> finalize COMMITTED

Store::open(...)
  -> init_incarnation(conn)
  -> recover_pending(conn, RecoveryConfig { bodies: RecordWalRegistry })
```

The current direct upsert logic is split into lower-level primitives:

- project a record into an inactive COW row,
- detect idempotent same-body replays,
- activate one target version atomically,
- drain derived rows for a target or record id.

Public API remains stable. `MemoryStore::upsert` still returns
`UpsertOutcome`; `get`, `list`, `versions`, and search keep their current
contracts.

## 5. Durable Mutation Payloads

Recovery must be able to resume a `PREPARED` op after process restart. An
upsert op therefore needs durable access to the planned `MemoryRecord`, and an
expire op needs durable access to the target and reason.

Add a small SQLite table:

```sql
CREATE TABLE wal_payloads (
  operation_id TEXT PRIMARY KEY
    REFERENCES wal_ops(operation_id),
  kind         TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at   INTEGER NOT NULL
);
```

`payload_json` is body-bearing for upserts. It must never be logged. This is
acceptable because `.cairn/cairn.db` is the authoritative private store, and
§5.6 already requires forget flows to purge WAL pre-images. The follow-up
`forget_record` implementation must also purge matching `wal_payloads` rows or
replace them with salted-hash audit stubs before a forget op reaches terminal
commit.

When a caller already has a persisted `FlushPlan`, `wal_ops.plan_ref` points to
that plan and `wal_payloads.payload_json` stores only the minimal mutation
payload needed by recovery. Direct `MemoryStore::upsert` calls synthesize the
same payload in-process.

## 6. Upsert Apply

### 6.1 `snapshot.stage`

Read the active row for `record.target_id`, if any, plus its derived rows. Store
the pre-image in `wal_steps.pre_image` for ord `0`. For a fresh insert, store an
explicit absent marker.

This step is non-idempotent in the static graph. Recovery never re-runs it once
marked `DONE`; later steps read the staged snapshot.

### 6.2 `primary.upsert_cow`

Compute the planned body hash and compare it with the active row:

- no active row: insert version `1` with `active = 0`;
- active row with same body hash: treat as idempotent, update projection/schema
  metadata in place if needed, and mark the op as no-content-change;
- active row with different body hash: insert version `N+1` with `active = 0`
  and leave version `N` active.

The inserted row uses a deterministic record id for retries:

- first version keeps `record.id`;
- superseding versions use a stored generated id in the upsert payload or
  snapshot so retries reuse the same row.

This step never flips the active pointer.

### 6.3 Derived Index Steps

`vector.upsert`, `fts.upsert`, and `edges.upsert` are idempotent.

- FTS is already maintained by triggers on `records`. The WAL step verifies the
  row exists in `records_fts` for the staged row and can repair it with
  delete-then-insert if drift is detected.
- Vector upsert runs only when an embedder is configured. It uses the same
  delete-then-insert behavior as the current embed-on-write path. If embedding
  fails, it queues `pending_embeddings` atomically and the WAL step still
  succeeds because the source record is durable and rebuildable.
- Edge upsert is a no-op for plain record upsert until the extractor writes
  explicit edge payloads. The step still records `DONE` so the graph matches
  §5.6 and recovery has a stable ordinal.

### 6.4 `primary.activate`

Inside one SQLite transaction:

1. assert the staged row exists;
2. set every row for the target `active = 0`;
3. set the staged row `active = 1` unless the op was an idempotent no-op;
4. append the consent journal row if the caller provided consent metadata.

This is the reader-visible linearization point. Before it commits, searches may
hit staged FTS/vector rows, but the join against `records.active = 1` drops
them.

After `StepRunner` has marked every step `DONE`, `record_wal` finalizes the
op to `COMMITTED`. If the process crashes after activation but before that
final transition, boot recovery reloads the all-`DONE` snapshot and finalizes
the op without re-running side effects.

## 7. Expire Apply

Expiration targets a `TargetId`, not a single version row.

### 7.1 `snapshot.stage`

Capture the active row and derived rows for the target. Missing or already
expired targets are idempotent success markers; no hard delete occurs.

### 7.2 `primary.mark_expired`

In one transaction, mark every row in the target lineage:

```sql
UPDATE records
   SET tombstoned = 1,
       tombstone_reason = 'expire',
       updated_at = :now
 WHERE target_id = :target_id;
```

Keeping all historical rows tombstoned prevents superseded versions from
leaking through audit or graph traversals. A future upsert for the same target
can create a later non-tombstoned version, which is the brief's un-expire path.

### 7.3 Derived Drains

`vector.drain`, `fts.drain`, and `edges.drain` are idempotent:

- delete `record_vectors` rows for all record ids in the target lineage;
- delete or verify absence of `records_fts` rows for tombstoned rows;
- remove non-audit edges whose endpoints are now retired, while preserving
  append-only WAL and consent data.

Readers already filter `tombstoned = 0`, so stale derived rows must not surface
even if a drain retry is pending.

## 8. Recovery

`RecordWalRegistry` implements `StepBodyRegistry` for `WalKind::Upsert` and
`WalKind::Expire`. `open.rs` must initialize the daemon incarnation before
boot recovery, then pass this registry to `recover_pending`; recovery step
bodies need a current incarnation so they can reacquire locks and run fenced.

Recovery behavior:

- `ISSUED` without `PREPARED` finalizes to `REJECTED` as today.
- `PREPARED` resumes at the first missing step.
- already `DONE` steps are skipped.
- retry exhaustion marks the op `ABORTED` and runs compensation for pre-activate
  upsert stages or pre-expire snapshots.

Compensation for #57 is limited:

- upsert before activation: tombstone or remove the inactive staged row and
  drain its derived rows;
- expire before `primary.mark_expired`: no-op;
- expire after `primary.mark_expired`: do not auto-unexpire unless the staged
  snapshot proves all rows were changed by this op and the op is still
  `PREPARED`. Otherwise leave tombstones in place and surface a recovery error.

## 9. Locking and Fencing

Every public upsert or expire apply acquires locks using #56's typed lock API:

- entity lock exclusive on `record.target_id` or expire target;
- session lock shared if the record scope carries a session id.

Step bodies run inside `LockHandle::with_fencing` so a stale holder cannot
commit authoritative changes after losing ownership. Recovery runs after
`init_incarnation`, then reacquires fresh locks before resuming bodies.

## 10. Tests

Write tests first.

### 10.1 Upsert conformance

- `upsert_writes_prepared_wal_steps_and_commits`:
  public `store.upsert(record)` creates one `COMMITTED` `upsert` op with all
  `UPSERT_STEPS` marked `DONE`.
- `upsert_stages_inactive_before_activation`:
  an injected failure after `primary.upsert_cow` leaves the new version
  inactive and default reads return the previous version.
- `upsert_supersession_preserves_versions`:
  two different bodies produce versions `1` and `2`, with exactly one active
  non-tombstoned row.
- `upsert_replay_same_operation_is_idempotent`:
  recovery re-runs a partial op without creating duplicate version rows or
  duplicate derived rows.

### 10.2 Expire conformance

- `expire_marks_target_retired_without_deleting_history`:
  `versions(target)` still returns rows, while `get`, `list`, and default
  search exclude them.
- `expire_is_idempotent_for_already_expired_target`:
  repeated expire does not create new record rows or fail.
- `future_upsert_after_expire_creates_visible_later_version`:
  upsert after expire writes version `N+1` and makes that version visible.

### 10.3 Derived index retry

- FTS drain can run twice and leaves no searchable expired record.
- Vector upsert/drain can run twice and leaves at most one vector row for the
  active version.
- Recovery from `PREPARED` with partial derived steps finishes to `COMMITTED`.

## 11. Open Risks

- `wal_payloads` is body-bearing. #58 must treat it as a retention surface for
  forget purge.
- The existing FTS triggers index inactive rows. This is acceptable because all
  read paths join back to `records.active = 1`, but tests must prove no staged
  row leaks through keyword, semantic, hybrid, or graph hydration paths.
- Current graph edge semantics include append-only audit edges. The expire
  drain must only remove reader-visible derived edges and must not mutate WAL
  history or consent journal rows.
