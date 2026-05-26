# Design — `cairn.admin.v1` extension

**Issue:** [#161](https://github.com/windoliver/cairn/issues/161) — `[P1] Implement cairn.admin.v1 extension with snapshot/restore/replay/connector controls`
**Phase:** v0.2 (Local + Nexus + Frontend)
**Brief sections:** §8 Extensions registry · §8.0.a wire-compat for `status` · §5.6 WAL state machine · §12 Deployment tiers · §14 Privacy & consent · §16 Packaging

## 1. Goal

Land the `cairn.admin.v1` extension end-to-end: six admin verbs (`snapshot`, `restore`, `replay_wal`, `connector_enable`, `connector_disable`, `connector_backfill`) exposed isomorphically through CLI, SDK, and MCP, gated by an operator role and the extension capability handshake. Without this extension operators have no out-of-band way to move a vault between machines, backfill a new connector without triggering full replay, or diagnose a boot-recovery hang in production.

## 2. Non-goals

Each non-goal must be filed as a follow-up issue and linked from this spec before the implementing PR merges.

- Cross-machine restore + per-user salt portability. Brief §14 is silent on salt portability; we refuse cross-machine restore with a typed error and defer the design.
- Hardware-key countersign for admin verbs (brief §8 calls for this; deferred to a v0.3 follow-up that builds on the `AdminContext` shape pinned here).
- Distributed snapshot coordination across federated hubs (owned by #26).
- Admin GUI surfaces (owned by frontend epic #23).
- Incremental / differential snapshots and backup encryption-at-rest (P2 follow-ups).

## 3. Dependencies

All satisfied at spec-write time:

- #55 (WAL state machine, step markers, boot recovery) — closed.
- #104 (Nexus sandbox sidecar lifecycle + config profile) — closed.
- #130 (connector framework + OAuth/webhook contracts) — merged at `3f7c19fb`. `ConnectorRegistry::{enable,disable}` and `Connector::backfill` already exist; we wire verbs on top.
- Backup-registry primitives (`BackupRegistry`, `BackupRegistryEntry`, `materialize_backup_artifact`, `register_backup_artifact`) — already in `crates/cairn-cli/src/verbs/admin_snapshot.rs`. Logic moves down into `cairn-core` as part of this work.

## 4. Architecture

```
cairn-cli ─────────┐
cairn-mcp ─────────┼─→ cairn-sdk ─→ cairn-core::verbs::admin::{snapshot,restore,replay_wal,
cairn-skill ───────┘                                          connector_enable,connector_disable,
                                                              connector_backfill}
                                              │
                                              ├─→ MemoryStore                (cairn-store-sqlite)
                                              ├─→ AdminStateStore (NEW)      (cairn-store-sqlite)
                                              ├─→ BackupRegistry             (existing)
                                              ├─→ ConnectorRegistry          (cairn-connectors-core)
                                              ├─→ WorkflowOrchestrator + emit_progress (NEW method)
                                              └─→ WalReplayer (NEW pure fn)  (cairn-core::wal)
```

### 4.1 New surfaces

- `cairn-core::status::wiring::ADMIN_EXTENSION_WIRED` + `admin_extension_ready()`.
- `cairn-core::status::REMEDIATION` rows for the six capability strings.
- `cairn-core::verbs::admin::{mod.rs, snapshot.rs, restore.rs, replay_wal.rs, connector.rs}` — six pure verb fns over `&dyn Trait` deps.
- `cairn-core::domain::admin::{AdminContext, AdminRole, AdminError}` — role-bit identity guard.
- `cairn-core::contract::admin_state::AdminStateStore` — trait: role lookup + connector enable/disable persistence.
- `cairn-core::contract::consent_log::ConsentLog` — small **new** read-only trait introduced by this refactor: returns forget events since a `frontier_step`. Replaces the path-coupled `replay_current_forgets()` free function currently in `crates/cairn-cli/src/verbs/admin_snapshot.rs:150`, which moves into `cairn-core::verbs::admin::restore` and consumes `&dyn ConsentLog`.
- `cairn-store-sqlite` — two append-only migrations: `admin_roles`, `connector_state` (schemas in §6).
- `cairn-sdk` — six typed wrappers.
- `cairn-mcp::generated::TOOLS` — six new `ToolDecl` entries driven by IDL.
- `cairn-cli` — refactor existing `admin_snapshot.rs` / `admin_restore.rs` into thin dispatch + four new subcommands.
- `cairn-workflows::Scheduler` — implements new `WorkflowOrchestrator::emit_progress` + `subscribe_progress` methods.

### 4.2 Boundary invariants honored

- `cairn-core` stays I/O-free; verbs take trait objects.
- CLI stays a thin wrapper (CLAUDE.md §6.5). Existing snapshot/restore CLI code is *moved down* into core, not duplicated.
- One verb fn per surface (CLI / MCP / SDK / skill all call the same `cairn-core::verbs::admin::*`).
- Capability gating via `wiring::ADMIN_EXTENSION_WIRED` + `AdminStateStore::has_role(identity, Operator)` (CLAUDE.md §4 invariant 6, fail closed).

## 5. Verb contracts

All six live under `cairn-core::verbs::admin::*`. Pure async fns over `&dyn Trait` deps. Every verb takes `AdminContext { actor: IdentityId, requested_role: AdminRole }` as first arg.

```rust
// snapshot.rs — operator role required; read-only on vault, writes artifact + registry entry
pub async fn run(
    ctx: AdminContext,
    req: SnapshotRequest,           // { out_path: PathBuf, label: Option<String> }
    store: &dyn MemoryStore,
    admin: &dyn AdminStateStore,
    registry: &dyn BackupRegistry,
) -> Result<SnapshotResponse, AdminError>;
// → { backup_id, artifact_path, sha256, frontier_step, manifest: SnapshotManifest }

// restore.rs — operator role required; refuses cross-machine
pub async fn run(
    ctx: AdminContext,
    req: RestoreRequest,            // { artifact_path: PathBuf, dry_run: bool }
    store: &dyn MemoryStore,
    admin: &dyn AdminStateStore,
    registry: &dyn BackupRegistry,
    consent: &dyn ConsentLog,
) -> Result<RestoreResponse, AdminError>;
// → { restored_records, tombstones_replayed, frontier_step }

// replay_wal.rs — dry-run under standard identity; --apply requires operator
pub async fn run(
    ctx: AdminContext,
    req: ReplayWalRequest,          // { from_step: StepMarker, apply: bool, sink: ProgressSink }
    store: &dyn MemoryStore,
    admin: &dyn AdminStateStore,
) -> Result<ReplayWalResponse, AdminError>;
// → { steps_visited, steps_applied, escalated: Vec<EscalatedStep> }

// connector.rs — three thin verbs
pub async fn enable(ctx, req: ConnectorTarget, admin, registry: &dyn ConnectorRegistry) -> …
pub async fn disable(ctx, req: ConnectorDisableRequest { name, reason }, admin, registry) -> …
pub async fn backfill(
    ctx,
    req: BackfillRequest,           // { name, from: DateTime<Utc>, to: DateTime<Utc>, rate_limit_per_sec }
    admin,
    registry,
    orch: &dyn WorkflowOrchestrator,
) -> Result<BackfillResponse, AdminError>;
// → { workflow_id, started_at }
```

### 5.1 Identity gating

One helper at the top of every write-modifying verb:

```rust
fn require_role(
    ctx: &AdminContext,
    admin: &dyn AdminStateStore,
    needed: AdminRole,
) -> Result<(), AdminError> {
    if !admin.has_role(&ctx.actor, needed)? {
        return Err(AdminError::NotAuthorized { actor: ctx.actor.clone(), needed });
    }
    Ok(())
}
```

`replay_wal` skips the check when `req.apply == false`; everything else gates on `Operator`.

### 5.2 Capability gating

Same pattern as `cairn.mcp.v1`:

- `wiring::ADMIN_EXTENSION_WIRED` flips when this epic lands.
- `status::advertise()` adds six capability rows iff: `ADMIN_EXTENSION_WIRED && config.admin.enabled && admin_state.has_any_operator()`.
- If any precondition fails the capability row is absent and verbs return `AdminError::CapabilityUnavailable` at the SDK boundary with the failing precondition surfaced in `remediation`.

Capability strings (added to the extension registry table at `crates/cairn-core/src/status/mod.rs:481`):

```
cairn.mcp.v1.extension.admin.snapshot
cairn.mcp.v1.extension.admin.restore
cairn.mcp.v1.extension.admin.replay_wal
cairn.mcp.v1.extension.admin.connector.enable
cairn.mcp.v1.extension.admin.connector.disable
cairn.mcp.v1.extension.admin.connector.backfill
```

## 6. Snapshot format

### 6.1 Artifact

`<label>-<backup_id>.cairn-snap.tar.zst` (zstd level 3 — fast, broadly available). One self-contained file.

```
manifest.json                      # first tar member, for cheap header read
cairn.db                           # via sqlite3_backup_*, consistent point-in-time
wiki/                              # markdown projections (verbatim)
raw/                               # markdown sources (verbatim)
purpose.md                         # vault purpose (if present)
config.snapshot.yaml               # filtered .cairn/config.yaml (secrets stripped)
```

### 6.2 `manifest.json` schema

Canonical JSON with sorted keys → stable sha256:

```json
{
  "schema_version": 1,
  "backup_id": "01H...",
  "created_at": "2026-05-26T17:42:11Z",
  "source_machine_id": "<sha256(machine_uid)>",
  "source_vault_id": "<vault_uuid>",
  "frontier_step": "step:0xab12...",
  "record_count": 12834,
  "tombstone_count": 41,
  "schema_versions": { "store": 12, "wal": 4 },
  "label": "pre-upgrade"
}
```

Two distinct fields:
- `schema_version` (singular) — version of *this manifest format itself*. Starts at `1`.
- `schema_versions` (plural) — per-component migration heads (store / wal / etc.) at snapshot time, used by the precondition gate to refuse forward restore. Values shown are illustrative.

### 6.3 Integrity envelope

`sha256(manifest.json) || sha256(cairn.db) || sha256(tar member tree)` → `artifact.sha256` returned by `snapshot` and recorded in `BackupRegistryEntry`. Round-trip identity check on restore compares all three.

### 6.4 Restore precondition gate

Ordered, fail-closed:

1. `manifest.schema_version` ∈ supported set (currently `{1}`); reject unknown values with `AdminError::SchemaTooNew` (reusing the variant — the manifest header is itself versioned and forward-incompat).
2. `manifest.source_machine_id == local_machine_id` else `AdminError::CrossMachineRestore` (remediation text points at the cross-machine follow-up issue; PR description must include its number before merge per §12).
3. `manifest.source_vault_id == local_vault_id` else `AdminError::VaultIdMismatch`.
4. `manifest.schema_versions` ≤ current schema head (never restore *forward*) else `AdminError::SchemaTooNew`.
5. Integrity envelope verified else `AdminError::IntegrityMismatch`.

Then stage to `.cairn/restore-<backup_id>/` → atomic rename of `cairn.db` → replay tombstones from `consent.log` since `frontier_step` (existing `replay_current_forgets`) → emit `RestoreResponse`.

### 6.5 SQLite migrations (`cairn-store-sqlite/migrations/`)

Both files use the next two sequence numbers after the current migrations head — the implementing PR fills `NNNN` and `NNNN+1` at write time. Per CLAUDE.md §6.11 migrations are append-only; never mutate after merge.

```sql
-- NNNN_admin_roles.sql
CREATE TABLE admin_roles (
  identity_id TEXT PRIMARY KEY,
  role        TEXT NOT NULL CHECK (role IN ('operator')),
  granted_at  TEXT NOT NULL,
  granted_by  TEXT NOT NULL,
  revoked_at  TEXT
);
CREATE INDEX admin_roles_active ON admin_roles(identity_id) WHERE revoked_at IS NULL;

-- NNNN+1_connector_state.sql
CREATE TABLE connector_state (
  connector_name   TEXT PRIMARY KEY,
  enabled          INTEGER NOT NULL DEFAULT 1,
  last_changed_at  TEXT NOT NULL,
  last_changed_by  TEXT NOT NULL,
  reason           TEXT
);
```

Bootstrap: first run with `admin.enabled: true` and no rows seeds the local human identity as `operator` (one-time, logged in `consent.log`).

### 6.6 Connector disable semantics

`disable()` writes the `connector_state` row, then calls `ConnectorRegistry::disable(name)` which already cancels in-flight polls via `CancellationToken`. Scheduler checks `connector_state.enabled` before each tick — guarantees the acceptance criterion of "stops new ingestion within one scheduler tick." Status surfaces every row's `enabled` + `last_changed_*` in `status.connectors[]`.

## 7. Error model

### 7.1 `AdminError` (`cairn-core::domain::admin::error`)

`thiserror`, `#[non_exhaustive]`:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AdminError {
    #[error("admin capability not negotiated: {capability}")]
    CapabilityUnavailable { capability: String, remediation: String },

    #[error("caller {actor} is not authorized for {needed:?}")]
    NotAuthorized { actor: IdentityId, needed: AdminRole },

    #[error("snapshot artifact integrity check failed")]
    IntegrityMismatch { expected: String, actual: String },

    #[error("snapshot is from machine {source} but local is {local} — cross-machine restore not supported in v0.2")]
    CrossMachineRestore { source: String, local: String },

    #[error("snapshot vault id {source} != local vault {local}")]
    VaultIdMismatch { source: String, local: String },

    #[error("snapshot schema_version {source} > local head {local} — refuse forward restore")]
    SchemaTooNew { source: u32, local: u32 },

    #[error("connector {name} not found in registry")]
    UnknownConnector { name: String },

    #[error("WAL step {marker} not found in ledger")]
    UnknownStepMarker { marker: String },

    #[error("WAL replay halted at non-idempotent step {step}; escalated to PURGE_PENDING")]
    ReplayEscalated { step: String },

    #[error(transparent)] Store(#[from] StoreError),
    #[error(transparent)] Wal(#[from] WalError),
    #[error(transparent)] Workflow(#[from] WorkflowError),
}
```

### 7.2 Wire envelope (brief §8.0.b uniform error)

```json
{
  "ok": false,
  "error": {
    "code": "CapabilityUnavailable",
    "capability": "cairn.mcp.v1.extension.admin.snapshot",
    "data": { "remediation": "enable admin: set `admin.enabled: true` in .cairn/config.yaml and grant operator role with `cairn admin grant <identity>`" }
  },
  "policy_trace": [ /* existing */ ]
}
```

### 7.3 CLI exit codes (CLAUDE.md §6.5, `std::process::ExitCode`)

| Code | Meaning |
|---|---|
| 0  | success |
| 64 | `NotAuthorized` |
| 69 | `CapabilityUnavailable` (EX_UNAVAILABLE) |
| 70 | `IntegrityMismatch` / `CrossMachineRestore` / `VaultIdMismatch` / `SchemaTooNew` / `UnknownStepMarker` / `UnknownConnector` (EX_SOFTWARE) |
| 75 | `ReplayEscalated` (EX_TEMPFAIL — operator must intervene) |
| 78 | config errors (EX_CONFIG) |

### 7.4 Capability advertisement (`status::advertise`, brief §8.0.a)

Six rows added to the advertise table. Each gated on `ADMIN_EXTENSION_WIRED && config.admin.enabled && admin_state.has_any_operator()`. If any precondition fails the row is absent from `status.extensions[].capabilities`. `REMEDIATION` table grows six rows — one per capability string.

### 7.5 `status.connectors[]` extension

Each entry carries `{ name, enabled, last_changed_at, last_changed_by, reason }`. Snapshot-tested to be byte-stable (brief §8.0.a wire compat).

### 7.6 Audit log

Every successful write-modifying admin verb appends one line to `.cairn/admin.audit.jsonl` (append-only, never rotated by the binary):

```json
{ "ts": "...", "actor": "hmn:...", "verb": "snapshot", "request_digest": "sha256:...", "response_digest": "sha256:...", "exit": 0 }
```

Read-only verbs (`replay_wal --dry-run`) do not audit.

## 8. Connector verbs + progress emission

### 8.1 `WorkflowOrchestrator` extension

One new event type, two new trait methods:

```rust
#[non_exhaustive]
pub struct ProgressEvent {
    pub workflow_id: WorkflowId,
    pub at: DateTime<Utc>,
    pub kind: ProgressKind,           // Started | Tick | Completed | Failed { code, msg }
    pub processed: u64,
    pub total: Option<u64>,
    pub detail: serde_json::Value,    // connector-specific payload (e.g. last cursor)
}

// Per CLAUDE.md §6.3: native async fn in traits (no `#[async_trait]`) — trait
// is consumed via `&dyn WorkflowOrchestrator`, so the methods that need to be
// object-safe stay non-generic and return concrete futures.
pub trait WorkflowOrchestrator: Send + Sync {
    // … existing …
    async fn emit_progress(&self, event: ProgressEvent) -> Result<(), WorkflowError>;
    async fn subscribe_progress(&self, id: WorkflowId)
        -> Result<broadcast::Receiver<ProgressEvent>, WorkflowError>;
}
```

`cairn-workflows::Scheduler` impl appends every event to `.cairn/metrics.jsonl` (brief §3, structured one-JSON-per-line) **and** fan-outs to an in-process `tokio::sync::broadcast::Sender<ProgressEvent>` keyed by `workflow_id`. Receivers are dropped when the workflow reaches `Completed` or `Failed`.

### 8.2 `connector_backfill` flow

1. Verb checks role + connector exists + connector enabled.
2. Creates `WorkflowId` (ULID), persists a `backfill_jobs(workflow_id, connector_name, from, to, rate, started_at, status)` row via existing scheduler persistence.
3. Spawns workflow via `orch.start_workflow()` — handler lives in `cairn-workflows::handlers::connector_backfill`, calls `Connector::backfill(from, to, rate)` (capability already exists per #130), pumps progress through `emit_progress` every N records or every 5s, whichever first.
4. Verb returns immediately with `{ workflow_id, started_at }`.
5. CLI `cairn admin connector backfill <name> --from ... --to ... [--watch]`:
   - without `--watch`: prints workflow id and exits.
   - with `--watch`: subscribes to broadcast, renders progress to TTY (respecting `IsTerminal` for color/bar), exits with the workflow's terminal status code.
6. Rate limit enforced by handler: `tokio::time::interval` clamps to `rate_limit_per_sec`; cancellation propagates from `ConnectorRegistry::disable()` mid-flight (already wired).

### 8.3 `connector_enable` / `connector_disable` flow

- Both: role check → upsert `connector_state` row → call `ConnectorRegistry::{enable,disable}(name)` → return `{ name, enabled, last_changed_at }`.
- Scheduler reads `connector_state.enabled` at the top of each tick (cheap indexed lookup); per AC#3 "stops new ingestion within one scheduler tick."
- Disable while a backfill workflow is running: backfill handler observes its `CancellationToken` and emits `ProgressEvent { kind: Failed { code: "CancelledByDisable", … } }` before exiting; backfill row marked `cancelled`.

### 8.4 Race semantics

- Enable→disable→enable in rapid succession: `connector_state` is row-level; last write wins; scheduler picks up next tick. No torn state because the row update is a single SQLite transaction and the registry call is idempotent.
- Snapshot taken mid-backfill: snapshot captures the `backfill_jobs` row as-is; restore on the same machine resumes from the persisted cursor (handler is restart-tolerant by contract). Snapshot does NOT wait for in-flight backfills to drain — the WAL frontier is the consistency boundary.

## 9. Testing strategy

Per CLAUDE.md §6.4: `cargo nextest run --workspace`, in-memory SQLite or `tempfile::tempdir()` vault, no DB mocks. TDD per §7.

### 9.1 Unit tests (in each new module)

- `verbs::admin::snapshot` — manifest canonicalization (sorted keys → stable sha256), integrity envelope computation. `proptest` round-trip: random `SnapshotManifest` → JSON → parse → equal.
- `verbs::admin::restore` — precondition gate ordering (schema → machine → vault → integrity), each fails closed with the right `AdminError`. `rstest` table of fixture manifests.
- `domain::admin::error` — wire envelope shape matches §8.0.b; `insta` snapshot of every variant's JSON.
- `status::wiring::admin_extension_ready` — truth table covering all 8 combinations of (wired, config, has_operator).

### 9.2 Integration tests (`crates/cairn-core/tests/admin_*.rs` + per-adapter)

- `admin_snapshot_restore_roundtrip.rs` — seed vault with N records + M tombstones via real `cairn-store-sqlite`, snapshot, drop DB, restore, assert bit-identical record hashes + tombstone count (AC#2).
- `admin_capability_advertise.rs` — flip `ADMIN_EXTENSION_WIRED` + config + admin rows; snapshot-test `status` response with `insta` (AC#1).
- `admin_unauth_reject.rs` — for each write-modifying verb, call with non-operator identity, assert `NotAuthorized` + exit code 64 + no audit row (AC#4 + Implementation Detail bullet 7).
- `admin_unnegotiated_reject.rs` — extension disabled in config; every verb returns `CapabilityUnavailable` with remediation hint matching `REMEDIATION` table.
- `admin_connector_disable_race.rs` — start a fake polling connector, fire `disable` while polls are in-flight, assert (a) no new ingestion events delivered after one scheduler tick, (b) `status.connectors[].enabled == false` (AC#3).
- `admin_connector_backfill_progress.rs` — drive backfill against fixture connector, subscribe to broadcast, assert progress events monotonically increase and final event is `Completed`. Also test `disable` mid-backfill emits `Failed { code: "CancelledByDisable" }`.
- `admin_replay_wal_dry_run.rs` — seed WAL with synthetic step graph, `replay_wal --from <step>` dry-run emits expected step events without any DB mutation (in-memory store snapshotted before/after).
- `admin_replay_wal_apply_escalates.rs` — synthetic non-idempotent step → `--apply` returns `ReplayEscalated` + exit 75 + WAL row marked `PURGE_PENDING`.
- `admin_cross_machine_refused.rs` — manifest carries different `source_machine_id` → `CrossMachineRestore` error with remediation pointing at follow-up issue id.

### 9.3 MCP conformance (`crates/cairn-mcp/tests/admin_tools.rs`)

- `list_tools` exposes six new tools when extension wired; absent otherwise.
- Schema parity: schemars-generated tool input schema matches the Rust request type for each verb (`schemars::schema_for!` snapshot).

### 9.4 Snapshot tests (`insta`)

- Every CLI verb's `--help` output, `--json` output (success + each error path).
- `status` response with extension off / on / on-but-no-operator.
- Each `AdminError` variant's wire envelope.

### 9.5 Bench (`crates/cairn-bench`)

Snapshot of a 100k-record vault should complete within budget — added as one row to the existing bench harness, gated as `coherence run --gate beta`.

### 9.6 Verification commands

Full CLAUDE.md §8 checklist runs unchanged. Codegen re-run for new IDL entries (`cargo run -p cairn-idl --bin cairn-codegen`). Docgen re-run for new CLI subcommands + capability table (`cargo run -p cairn-cli --bin cairn-docgen -- --write`).

## 10. Implementation phasing (for writing-plans hand-off)

The spec is one document; the plan splits the work into landed-PR-sized slices:

1. Wiring + capability advertisement + REMEDIATION + `AdminContext` + `AdminStateStore` trait + the two migrations.
2. Refactor existing CLI `admin_snapshot` / `admin_restore` down into `cairn-core::verbs::admin`; add manifest + integrity envelope + machine-id check.
3. `replay_wal` (dry-run first, then `--apply` with `ReplayEscalated`).
4. Connector verbs (`enable` / `disable`); new scheduler-tick state check.
5. `WorkflowOrchestrator::emit_progress` + `subscribe_progress` + `connector_backfill` handler + CLI `--watch`.
6. MCP tool decls + SDK wrappers (IDL codegen after each new verb).

Each phase is mergeable on its own behind `ADMIN_EXTENSION_WIRED = false` until phase 6 flips it.

## 11. Acceptance criteria mapping

| AC | Verified by |
|---|---|
| AC#1 — six verbs advertised when extension enabled, absent otherwise | `admin_capability_advertise.rs` (§9.2) + truth-table unit (§9.1) |
| AC#2 — snapshot→restore bit-identical, preserves tombstones | `admin_snapshot_restore_roundtrip.rs` (§9.2) |
| AC#3 — disable stops ingestion within one scheduler tick, visible in `status --json` | `admin_connector_disable_race.rs` (§9.2) |
| AC#4 — every write verb fails closed with `CapabilityUnavailable` when not negotiated | `admin_unnegotiated_reject.rs` (§9.2) |

## 12. Follow-up issues to file before merge

- Cross-machine restore + salt portability (blocks v0.3).
- Hardware-key countersign for admin verbs (blocks v0.3 federation).
- Incremental / differential snapshots (P2).
- Backup encryption-at-rest (P2).

Each must exist and be linked from the implementing PR description before review.
