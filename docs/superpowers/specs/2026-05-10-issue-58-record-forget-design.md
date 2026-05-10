# Issue #58 record-level forget — design

- **Issue:** [#58](https://github.com/windoliver/cairn/issues/58)
- **Parent epic:** [#8](https://github.com/windoliver/cairn/issues/8) WAL, locks, replay, and record-level forget
- **Phase:** v0.1 P0
- **Brief sections:** §5.6 forget_record · §14 Privacy and Consent · §18.c US8
- **Status:** approved design, awaiting implementation plan
- **Date:** 2026-05-10

## 1. Goal

Implement `forget` mode `record` as a WAL-backed, crash-safe two-phase delete
for one target lineage. Phase A tombstones every version of the target and
makes it immediately reader-invisible. Phase B physically purges the target
from the primary vault surfaces that exist in v0.1: `records`, FTS, vectors,
pending embedding queues, record edges, entity episode links, body-bearing WAL
payloads or pre-images, and markdown/profile/cache surfaces that are present in
the local vault.

The CLI, status advertisement, and recovery path must all agree: once this
lands, `cairn forget --record <record_id>` is an advertised v0.1 capability,
while session and scope forget remain unavailable.

## 2. Non-goals

- Session-wide forget fan-out. `forget --session` remains
  `CapabilityUnavailable` until the v0.2 task.
- Scope/folder forget. `forget --scope` remains `CapabilityUnavailable` until
  the v0.3 task.
- Backup registry tombstone replay and source-side `redact_on_forget` fan-out.
  The issue comment explicitly defers those to #160.
- A new storage contract method. The implementation should fit the existing
  SQLite record WAL path and avoid a parallel direct-delete API.
- P1 Nexus durable messaging. P0 remains a single SQLite authority.

## 3. Source-of-truth alignment

| Source | Requirement | Design choice |
|---|---|---|
| §5.6 `forget_record` | Phase A marks every version of a target tombstoned | Resolve the public `record_id` to `target_id`, then update all rows in that lineage |
| §5.6 Phase B | Purge vectors, FTS, edges, primary rows, WAL pre-images, and snapshots | Add `ForgetPayload` and `RecordStepBody` arms for all seven `FORGET_RECORD_STEPS` |
| §5.6 recovery | Recovery resumes from durable `wal_steps` markers | Extend `RecordWalRegistry` to return a body for `WalKind::ForgetRecord` |
| §14 Privacy and Consent | Forget is body-free but auditable | Append a `forget_intent` `consent_journal` event whose subject is a salted target hash, not body text |
| §18.c US8 | Record-level delete ships in v0.1; session delete is v0.2 | Flip only `FORGET_RECORD_WIRED`; keep session/scope rejected |
| #58 issue comment | Primary vault only; backups/sources deferred | `snapshot.purge` only handles present local primary surfaces and records a no-op for absent v0.1 backup/source machinery |

## 4. Current baseline

`origin/main` already includes the prerequisite substrate:

- `cairn_core::wal::FORGET_RECORD_STEPS` with seven stable step names.
- `crates/cairn-store-sqlite/src/record_wal/` for upsert and expire payloads,
  locks, operation issue/finalize helpers, `RecordStepBody`, and
  `RecordWalRegistry`.
- `wal_payloads` for recovery payloads.
- `recover_pending` wired from `open.rs` with `RecordWalRegistry`.
- SQLite record, FTS, vector, pending embedding, edge, entity graph, and
  consent-journal tables.
- `cairn forget` exists but currently returns `CapabilityUnavailable` outside
  the dry-run/human-review placeholder plan path.
- `FORGET_RECORD_WIRED` is still `false`, so status correctly does not
  advertise the capability.

The implementation should extend this shape instead of introducing a second
mutation runner.

## 5. Architecture

Add `forget_record` as the third production body in the existing record WAL
module:

```text
cairn forget --record <record_id>
  -> open bound vault/store
  -> resolve record_id -> target_id + scope + active body hash
  -> issue prepared wal_ops row kind=forget_record
  -> save RecordWalPayload::ForgetRecord
  -> acquire entity lock through record_wal::locks
  -> run FORGET_RECORD_STEPS through StepRunner
  -> finalize wal_ops state=COMMITTED
  -> return ForgetData { deleted_count, tombstones }

Store::open(...)
  -> init_incarnation(conn)
  -> recover_pending(conn, RecoveryConfig { bodies: RecordWalRegistry })
     -> WalKind::ForgetRecord reloads ForgetPayload and resumes steps
```

Files expected to change:

- `crates/cairn-store-sqlite/src/record_wal/payload.rs` adds
  `RecordWalPayload::ForgetRecord(Box<ForgetPayload>)`.
- `crates/cairn-store-sqlite/src/record_wal/steps.rs` adds
  `RecordStepPayload::ForgetRecord` step arms.
- `crates/cairn-store-sqlite/src/record_wal/recovery.rs` maps
  `WalKind::ForgetRecord` to the new body.
- `crates/cairn-store-sqlite/src/record_wal/forget.rs` owns public apply
  orchestration and payload construction.
- `crates/cairn-store-sqlite/src/record_wal/mod.rs` exports the new apply
  helper inside the crate.
- `crates/cairn-cli/src/verbs/forget.rs` becomes an async real dispatch path
  for record mode.
- `crates/cairn-core/src/status/wiring.rs` flips only
  `FORGET_RECORD_WIRED`.
- `crates/cairn-core/src/verbs/lint/` and the SQLite lint bridge gain a
  `purge_pending` report for exhausted or incomplete Phase B forget work.

## 6. Target resolution

The public input is `record_id` because that is the ID users and search results
carry. The storage operation targets a `target_id` lineage because §5.6 requires
forget to tombstone and purge every version of that target.

Resolution happens before issuing the WAL op:

1. Look up `record_id` in `records`, including inactive and tombstoned rows.
2. If no row exists, return a not-found envelope for normal calls.
3. If the row exists, capture `target_id`, `scope`, all version `record_id`s,
   and body hashes needed for target/audit hashing.
4. Build `ForgetPayload` with the `target_id`, original requested `record_id`,
   scope, target-hash/audit salt material, and expected version IDs.

Repeated recovery does not need the original row to still exist; it reloads the
saved payload from `wal_payloads`.

## 7. WAL steps

### 7.1 `primary.mark_tombstone`

Inside the StepRunner transaction:

```sql
UPDATE records
   SET active = 0,
       tombstoned = 1,
       tombstone_reason = 'forget',
       updated_at = :now
 WHERE target_id = :target_id;
```

The same transaction appends a body-free `forget_intent` event to
`consent_journal`. The event subject is a salted hash of the target, not the
body or raw target string. This step is the reader-visible linearization point:
after it commits, `get`, `list`, FTS, semantic search, graph search, and
retrieve paths must not surface the target because they all join/filter on
`tombstoned = 0` or active rows.

### 7.2 `vector.drain`

Delete `record_vectors` and `pending_embeddings` rows for every record id in
the target lineage. This is idempotent: no matching rows means the step is
already complete.

### 7.3 `fts.drain`

Delete `records_fts` rows by source `records.rowid` for the target lineage.
Even though records triggers may remove rows during `primary.purge`, this step
is explicit so Phase B can prove the search index is clean before primary rows
disappear.

### 7.4 `edges.drain`

Delete record-level edges whose `src` or `dst` belongs to the target lineage.
Also delete entity graph episode links that reference those record IDs. Entity
nodes and non-episode entity edges remain unless their own lifecycle rules say
otherwise; the forget contract is about references back to the forgotten record
content.

### 7.5 `primary.purge`

Physically delete every `records` row for the target. SQLite triggers and
foreign keys handle any remaining derived cleanup, but tests assert explicit
absence from all primary surfaces. The step records the deleted count for the
CLI response before deletion. During recovery, if the payload target has no
remaining rows and prior steps are `DONE`, this step is treated as complete.

### 7.6 `wal.purge_pre_images`

Scrub body-bearing WAL retention surfaces for this target:

- `wal_steps.pre_image` rows whose JSON references any forgotten `record_id` or
  target id are replaced with a salted audit stub containing only the
  forgetting operation id, scrub timestamp, and salted target hash.
- `wal_payloads` rows for prior upsert/expire operations that contain the
  forgotten target/body are similarly replaced or removed.
- The current forget payload must remain body-free and recoverable until the
  forget op is terminal.

No raw body text, original target id, or original record id may remain in these
body-bearing WAL surfaces after this step.

### 7.7 `snapshot.purge`

For v0.1 primary vault scope, this step performs idempotent local cleanup only
for snapshot/projection/cache surfaces that exist in the checkout. Backup
registry rewrite and source-file redaction are explicitly skipped because #160
owns them. The step still records `DONE` so the static seven-step graph stays
stable and future backup/source machinery can extend the body without changing
step ordinals.

## 8. CLI and capability behavior

Record mode becomes real:

```text
cairn forget --record <record_id> --json
```

The command returns a normal `forget` response envelope with
`ForgetData.deleted_count`, `tombstones`, and no `plan_ref` unless the caller
asked for `--human-review`.

Dry-run and human-review keep the FlushPlan behavior from #54. They should
produce `PlannedMutation::ForgetRecord { target }` after resolving the input
record to its target. Applying a human-review plan can remain a plan-lifecycle
operation until the existing flush apply path is upgraded; the direct
autonomous `forget --record` path must perform the real WAL mutation.

Session and scope modes continue to return `CapabilityUnavailable`, and status
advertises only `cairn.mcp.v1.forget.record`.

## 9. Error handling

- Missing record for a new autonomous request maps to the wire `NotFound`
  error.
- A duplicate/replayed WAL operation returns the original result when the
  existing replay ledger can identify it; otherwise the operation remains
  idempotent at the SQLite step level.
- Lock acquisition errors use the existing lock error types, including owner,
  operation, timeout, and retry hint.
- Stale fencing fails before step side effects run.
- Schema drift, malformed WAL payloads, or impossible step state combinations
  return `StoreError::Invariant` or `SchemaDrift` and leave the op recoverable
  or aborted according to the StepRunner policy.
- Session/scope requests remain fail-closed with `CapabilityUnavailable`.

## 10. Recovery

`RecordWalRegistry::body_for` should accept `WalKind::ForgetRecord`, load the
`ForgetPayload`, acquire the entity lock for the payload target/scope, and
return `RecordStepBody::new_forget_record`.

Recovery properties:

- A crash before `primary.mark_tombstone` leaves no reader-visible change.
- A crash after `primary.mark_tombstone` keeps the target reader-invisible and
  resumes Phase B from the next incomplete step.
- Already `DONE` drain/purge steps are skipped by the StepRunner.
- Exhausted Phase B retries leave the target reader-invisible and surface a
  `purge_pending` lint finding keyed by `operation_id`, `target_hash`, and the
  failed `step_ord`; the finding must not include raw body, record id, or target
  id.
- Terminal commit occurs only after every `FORGET_RECORD_STEPS` row is `DONE`.

## 11. Tests

Use TDD and write failing tests before implementation.

Store integration tests in
`crates/cairn-store-sqlite/tests/forget_record.rs`:

- `forget_record_tombstones_all_versions`: supersede a target twice, run the
  Phase A body, assert every version is tombstoned and default reads/searches
  miss it.
- `forget_record_purges_primary_and_indexes`: run the full apply path and
  assert no rows remain in `records`, `records_fts`, `record_vectors`,
  `pending_embeddings`, `edges`, or entity episode tables for the target.
- `forget_record_scrubs_wal_payloads`: seed a body-bearing upsert payload and
  `wal_steps.pre_image`, run forget, and assert the unique sentinel body token
  is absent from WAL surfaces.
- `forget_record_recovery_resumes_after_each_done_step`: seed prepared ops with
  different completed prefixes and assert boot recovery finishes the purge.
- `forget_record_replay_is_idempotent`: run the same operation twice and assert
  no duplicate consent events with body-bearing payload and no SQL errors.
- `forget_record_keeps_session_siblings_visible`: insert two targets in the
  same session, forget one, and assert the other still appears in reads/search.

CLI tests in `crates/cairn-cli/tests/forget_record.rs`:

- `forget_record_json_commits`: create a temp vault, insert one record, run
  `cairn forget --record <id> --json`, and assert `deleted_count = 1`.
- `forget_record_removes_from_search_and_retrieve`: after CLI forget, search
  and retrieve cannot surface the record.
- `forget_session_still_unavailable`: assert `forget --session` still returns
  `CapabilityUnavailable`.
- `status_advertises_record_only`: assert status includes record forget and
  does not include session/scope forget.

Leakage regression:

- Insert a body containing a unique sentinel token.
- Run full record forget.
- Directly query every primary-vault table that can hold body/index/edge/WAL
  content and assert the sentinel is absent. Hash-only audit metadata is
  allowed; raw body text, raw target id, and raw record id are not.

## 12. Self-review notes

- The design scopes the issue to primary vault surfaces, matching the #58
  comment and leaving backups/source rewrite to #160.
- It uses the existing record WAL modules from #57 rather than adding a direct
  store delete method.
- It keeps `forget_record` lineages target-based even though the public input
  is record-based.
- It preserves fail-closed capability advertisement for session/scope forget.
- It contains no placeholder requirements; every deferred item is tied to a
  concrete out-of-scope issue, while purge-pending lint is in scope here.
