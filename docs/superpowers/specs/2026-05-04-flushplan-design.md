# FlushPlan generation, dry-run, and human-review — design

- **Issue:** [#54](https://github.com/windoliver/cairn/issues/54)
- **Parent epic:** [#8](https://github.com/windoliver/cairn/issues/8) WAL, locks, replay, record-level forget
- **Phase:** v0.1 P0
- **Brief sections:** §5.5 Plan, then apply · §5.2 Write path · §5.6 WAL envelope
- **Status:** approved (ready for implementation plan)
- **Date:** 2026-05-04

## 1. Goal

Produce a typed, serializable `FlushPlan` for every write-path mutation before
any bytes hit `MemoryStore`, and expose three modes (`autonomous`, `dry_run`,
`human_review`) through CLI/MCP/SDK so the same plan object can be applied
inline, returned without side effects, or persisted to disk for later
review-and-apply.

A `FlushPlan` is the primary audit artifact for every memory mutation (brief
§5.5). It must be idempotent, replayable, and structurally identical across
the three modes.

## 2. Non-goals

- Workflow scheduling after apply (out-of-scope per issue).
- Wiring the full ingest / forget pipelines through the plan layer — those are
  stubs awaiting #9. This PR delivers the plan layer and surfaces; #9 wires
  the producer side.
- Adding new top-level verbs beyond brief §8's eight. `cairn flush` is an
  admin-style CLI subcommand group, not an MCP/SDK verb.
- Replacing the §5.6 WAL state machine. Apply walks `MemoryStore` directly
  for now; once the WAL adapter lands, `apply` becomes a thin shim that
  enqueues a WAL op carrying the plan.

## 3. Source-of-truth alignment

| Brief section | What it pins | How this design honors it |
|---|---|---|
| §5.5 (plan, then apply) | three modes; `.cairn/flush/<ts>.plan.json` filename; `cairn flush apply <id>` | exact filename + subcommand names |
| §5.6 (WAL envelope) | `operation_id`, `target_hash`, `plan_ref`, `dependencies`, `expires_at` | `FlushPlan` carries the same fields; `plan_ref` resolves to the on-disk path |
| §8 (eight verbs) | `ingest` and `forget` get a `mode` arg | IDL change scoped to the two existing verbs |
| §11 / §4 (CLI is ground truth) | every other surface mirrors CLI | `--dry-run` / `--human-review` flags map 1:1 to a `mode` arg in the IDL request body |

## 4. Architecture

Four layers, top-down.

```
┌────────────────────────────────────────────────────────────────┐
│ cairn-cli      ingest --dry-run / --human-review               │
│                cairn flush list / apply / reject               │
└────────────────────────┬───────────────────────────────────────┘
                         │ thin adapters
┌────────────────────────▼───────────────────────────────────────┐
│ cairn-core::domain::flush_plan                                 │
│   FlushPlan, FlushMode, PlannedMutation, PlanStatus            │
│   serialize / deserialize / idempotency_key / target_hash      │
│   render_diff (markdown)                                       │
│   path resolution: pending/<id>.plan.json, applied/, rejected/ │
│   pure functions only — no I/O                                 │
└────────────────────────┬───────────────────────────────────────┘
                         │ contract calls
┌────────────────────────▼───────────────────────────────────────┐
│ cairn-core::contract::MemoryStore                              │
│   already in-tree (#46)                                        │
└────────────────────────┬───────────────────────────────────────┘
                         │
┌────────────────────────▼───────────────────────────────────────┐
│ cairn-store-sqlite                                             │
└────────────────────────────────────────────────────────────────┘
```

The CLI does the `tokio::fs` writes; `cairn-core` stays pure, returning
`(path, bytes)` tuples that the CLI persists. This keeps the core boundary
intact (`scripts/check-core-boundary.sh` passes).

## 5. Data shapes

### 5.1 Core types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlushMode {
    Autonomous,
    DryRun,
    HumanReview,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlushPlan {
    /// Doubles as the WAL operation_id and the on-disk filename stem.
    /// Monotonic ULID; stable across the plan's lifetime.
    pub operation_id: Ulid,
    pub issued_at: Timestamp,
    pub issuer: ActorId,
    pub principal: Option<ActorId>,
    pub scope: Scope,
    pub mode: FlushMode,
    pub mutations: Vec<PlannedMutation>,
    pub reason: PlanReason,
    /// Capture / extract event ids that motivated this plan.
    pub source_events: Vec<EventRef>,
    /// Pre-state hashes per target — used at apply time to detect drift
    /// (someone else wrote between plan and apply).
    pub target_hashes: BTreeMap<TargetId, Sha256Hash>,
    /// WAL ops this one must apply after (§5.6).
    pub dependencies: Vec<Ulid>,
    /// 5-minute receipt TTL (§5.6). Apply past this is rejected.
    pub expires_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlannedMutation {
    Upsert  { record: MemoryRecord, prior_version: Option<u32> },
    Delete  { target: TargetId, prior_version: u32 },
    Promote { from: TargetId, to_kind: MemoryKind, evidence: Vec<Ulid> },
    Expire  { target: TargetId, reason: ExpirationReason },
    ForgetSession { session: SessionId },
    ForgetRecord  { target: TargetId },
    Evolve  { skill: TargetId, diff_ref: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanReason {
    UserIngest,
    SensorCapture { sensor: String },
    Promote { confidence: f32, evidence_count: u32 },
    Expire   { ttl_expired: bool, salience_below: Option<f32> },
    Forget   { request_id: Ulid },
    Evolve   { previous_version: u32 },
}
```

### 5.2 Persisted wrapper

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedPlan {
    /// On-disk schema version. Always 1 in this PR.
    pub schema_version: u16,
    pub plan: FlushPlan,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlanStatus {
    Pending,
    Applied  { at: Timestamp },
    Rejected { at: Timestamp, reason: String },
}
```

### 5.3 Idempotency

`FlushPlan::idempotency_key()` is `operation_id` — the ULID is the WAL
idempotency key (§5.6). Apply checks for a matching `applied/<id>.plan.json`
or `rejected/<id>.plan.json` before doing any work; both branches return a
no-op success that surfaces the original status.

`FlushPlan::target_hash(target_id)` returns the pre-state SHA-256 stored in
`target_hashes`. At apply time, the live record body is hashed and compared;
mismatch → `FlushPlanError::TargetDrift`, plan stays pending.

## 6. Filesystem layout

```
<vault>/.cairn/flush/
├── pending/
│   ├── 01HQZK...0001.plan.json   # PersistedPlan, status = Pending
│   └── 01HQZK...0001.diff.md     # human-readable diff (skipped with --no-diff)
├── applied/
│   └── 01HQZK...0002.plan.json   # status = Applied { at }
└── rejected/
    └── 01HQZK...0003.plan.json   # status = Rejected { at, reason }
```

- The `<id>` segment is the plan's `operation_id` ULID. Monotonic, sortable,
  collision-free.
- `apply` and `reject` use `tokio::fs::rename` to move between
  `pending/`/`applied/`/`rejected/`. Same-filesystem rename is atomic.
- `flush list` reads `pending/` only by default; `--all` includes
  `applied/` + `rejected/`.
- Directory creation is lazy — first write triggers `create_dir_all`.

## 7. Lifecycle

### 7.1 Mode dispatch

```
ingest <args>                       → produce plan → apply inline (autonomous)
                                      writes mutations through MemoryStore,
                                      emits Response with applied = true.

ingest <args> --dry-run             → produce plan → emit JSON to stdout
                                      writes nothing. Vault untouched.
                                      Exit 0.

ingest <args> --human-review        → produce plan → write
                                      .cairn/flush/pending/<id>.plan.json
                                      + <id>.diff.md (unless --no-diff).
                                      MemoryStore untouched.
                                      Exit 0; stdout = plan summary + path.
```

`forget` mirrors `ingest` exactly. `--dry-run` and `--human-review` are
mutually exclusive (clap `conflicts_with`).

### 7.2 Apply

```
cairn flush apply <id>
  if applied/<id>.plan.json exists → no-op success (idempotent re-apply)
  if rejected/<id>.plan.json exists → error AlreadyTerminal { Rejected }
  read pending/<id>.plan.json
  if status != Pending → error AlreadyTerminal { status }
  if expires_at < now → error Expired

  # Phase 1 — pre-flight drift check across ALL targets, no mutations yet.
  for mutation in plan.mutations:
      verify target_hashes[mutation.target] matches live state
      → error TargetDrift on first mismatch; file untouched

  # Phase 2 — apply.
  for mutation in plan.mutations:
      dispatch to MemoryStore (upsert / delete / etc.)
      → on error, poison the plan: move pending/ → rejected/
        with reason = "apply failed at mutation <i>: <err>"
        and surface the StoreError. Manual recovery only.

  rename pending/ → applied/, set status = Applied { at: now }
  emit Response with applied = true
```

Two-phase apply (drift-check, then mutate) keeps the partial-failure window
small — most plans either pass or fail-closed during phase 1. The
`MemoryStore` trait does not currently expose a transaction / batch
primitive, so a phase-2 failure between mutation N and N+1 does leave the
store with the first N mutations applied. This is documented and made
visible by the poisoned-rejected file (operator sees "apply failed at
mutation 3 of 5" and can reconcile manually). When the WAL state machine
lands (#9 / #55), the entire loop is replaced by a single `wal::issue(plan)`
call that runs all mutations in one local transaction per §5.6 P0
single-transaction model — partial-failure window goes to zero.

### 7.3 Reject

```
cairn flush reject <id> --reason "<r>"
  read pending/<id>.plan.json
  if status != Pending → error
  rename pending/ → rejected/, set status = Rejected { at, reason }
  no MemoryStore calls
```

### 7.4 List

```
cairn flush list [--all]
  read .cairn/flush/pending/*.plan.json (and applied/ + rejected/ with --all)
  print: id  mode  reason  mutations  issued_at  status
  --json emits a JSON array of summaries.
```

## 8. CLI surface

### 8.1 Existing verbs — new flags

```
cairn ingest [args] --dry-run
cairn ingest [args] --human-review [--no-diff]
cairn forget [args] --dry-run
cairn forget [args] --human-review [--no-diff]
```

`--dry-run` and `--human-review` are mutually exclusive. Default (neither
flag) is `autonomous`.

### 8.2 New admin subcommand group

```
cairn flush list [--all] [--json]
cairn flush apply <id> [--json]
cairn flush reject <id> --reason "<r>" [--json]
```

Not in IDL. Lives in `cairn-cli` only. Mirrors the `admin_reindex` /
`admin_model_fetch` pattern.

## 9. IDL changes

### 9.1 `schema/verbs/ingest.json` — add to `Args`

```jsonc
"mode": {
  "type": "string",
  "enum": ["autonomous", "dry_run", "human_review"],
  "default": "autonomous",
  "description": "Plan dispatch mode (brief §5.5)."
}
```

CLI mapping: add two flag entries to `x-cairn-cli.flags`:

```jsonc
{ "name": "dry_run",      "long": "dry-run",      "value_source": "bool",
  "maps_to": { "field": "mode", "value": "dry_run" } },
{ "name": "human_review", "long": "human-review", "value_source": "bool",
  "maps_to": { "field": "mode", "value": "human_review" } }
```

(If `x-cairn-cli` doesn't currently support `maps_to`, the codegen change
is a small extension. Fallback: emit the bool flags in clap, fold to `mode`
in the verb handler.)

### 9.2 `schema/verbs/forget.json`

Same `mode` enum + same two flag mappings.

### 9.3 Response shape

`ingest` / `forget` response gains an optional field:

```jsonc
"plan": {
  "type": "object",
  "description": "FlushPlan, present when mode=dry_run or mode=human_review.",
  "properties": {
    "operation_id":  { "$ref": "../common/primitives.json#/$defs/Ulid" },
    "mode":          { "type": "string", "enum": ["dry_run", "human_review"] },
    "plan_ref":      { "type": "string", "description": "Path under .cairn/flush/pending/ when human_review." }
  }
}
```

The full `FlushPlan` JSON schema isn't reified here — it's a `cairn-core`
type and the generated TypeScript / Python clients in P1 will derive it from
the Rust type via `schemars`. For P0 the Response carries `operation_id` +
`mode` + `plan_ref`; the body of the plan is on disk (human_review) or
inlined as a sub-object (dry_run).

## 10. Error handling

`cairn-core::error::FlushPlanError` (thiserror enum, `#[non_exhaustive]`):

| Variant | When | CLI exit code |
|---|---|---|
| `Serialize(serde_json::Error)` | plan → bytes failed | EX_SOFTWARE=70 |
| `Deserialize(serde_json::Error)` | bytes → plan failed | EX_DATAERR=65 |
| `NotFound { id }` | `flush apply <id>` and no pending file | EX_NOINPUT=66 |
| `AlreadyTerminal { id, status }` | `apply` or `reject` on applied/rejected | EX_DATAERR=65 |
| `Expired { id, expires_at }` | apply past `expires_at` | EX_DATAERR=65 |
| `TargetDrift { target, expected, actual }` | pre-state hash mismatch | EX_TEMPFAIL=75 |
| `StoreFailure(StoreError)` | underlying MemoryStore call failed | propagate |

CLI uses `anyhow` only at the `main` boundary, mapping to the codes above.

## 11. Testing

### 11.1 Unit (cairn-core)

- `FlushPlan` JSON round-trip for each `PlannedMutation` variant.
- `PersistedPlan` round-trip for each `PlanStatus` variant.
- `idempotency_key` returns `operation_id` byte-for-byte.
- `target_hash` lookups for present / absent targets.
- Path helpers: `pending_path`, `applied_path`, `rejected_path` produce the
  documented layout.
- `render_diff` produces stable markdown for each mutation kind.

### 11.2 Property (cairn-core)

- `proptest` round-trip: arbitrary `FlushPlan` → JSON → `FlushPlan` is
  identity. Generators provided in `cairn-test-fixtures`.
- `proptest` filename safety: `operation_id` ULID never produces a path
  segment that escapes `<vault>/.cairn/flush/`.

### 11.3 Snapshot (cairn-core, `insta`)

- One `.snap` per `PlannedMutation` variant — JSON shape locked.
- One `.snap` for the markdown diff per variant.

### 11.4 Integration (cairn-cli/tests)

- `dry_run_writes_nothing`: `cairn ingest --dry-run` against a tempdir vault
  → assert no `.cairn/flush/` exists, stdout is valid plan JSON.
- `human_review_writes_pending`: writes `pending/<id>.plan.json` +
  `<id>.diff.md`, stdout shows path, MemoryStore unchanged.
- `flush_apply_moves_to_applied`: pre-write a pending plan, run `flush apply`,
  assert file moved to `applied/`, MemoryStore reflects mutations.
- `flush_reject_moves_to_rejected`: assert move + reason recorded.
- `flush_list_outputs_summary`: snapshot of human + JSON output.
- `apply_idempotent_on_applied`: re-apply a plan already in `applied/`
  → no-op success, exit 0.
- `apply_rejects_on_rejected`: re-apply a plan in `rejected/` →
  `AlreadyTerminal`, exit EX_DATAERR.
- `apply_fails_on_drift`: mutate the live record between plan + apply →
  `TargetDrift` raised in phase 1, file stays in `pending/`, no mutations
  applied.
- `apply_poisons_plan_on_phase2_failure`: inject a MemoryStore failure
  between mutation 1 and 2 of a 3-mutation plan → file moved to
  `rejected/<id>.plan.json` with `reason` containing
  `"apply failed at mutation 1"`, mutation 0 remains applied to the store.
- `apply_fails_on_expired`: synthesize a plan with `expires_at` in the past
  → `Expired`, file stays in `pending/`.
- `dry_run_and_human_review_conflict`: clap rejects both flags together.

### 11.5 CLI snapshot (cairn-cli, `insta`)

- `cairn ingest --dry-run` JSON output.
- `cairn flush list` human + JSON output.
- `cairn flush apply <id>` success output.
- `cairn flush reject <id>` success output.

## 12. Files touched

```
crates/cairn-core/src/domain/flush_plan/mod.rs            NEW   types + serde
crates/cairn-core/src/domain/flush_plan/store.rs          NEW   path helpers, pure
crates/cairn-core/src/domain/flush_plan/diff.rs           NEW   markdown renderer
crates/cairn-core/src/domain/mod.rs                       EDIT  pub mod flush_plan
crates/cairn-core/src/error/mod.rs                        EDIT  add FlushPlanError
crates/cairn-core/tests/flush_plan_proptest.rs            NEW
crates/cairn-core/src/domain/flush_plan/snapshots/        NEW   insta snapshots
crates/cairn-cli/src/verbs/flush.rs                       NEW   list/apply/reject
crates/cairn-cli/src/verbs/mod.rs                         EDIT  pub mod flush
crates/cairn-cli/src/main.rs                              EDIT  wire flush group
crates/cairn-cli/src/verbs/ingest.rs                      EDIT  --dry-run / --human-review parse
crates/cairn-cli/src/verbs/forget.rs                      EDIT  same
crates/cairn-cli/tests/flush_integration.rs               NEW
crates/cairn-cli/src/snapshots/                           NEW   CLI snapshots
crates/cairn-idl/schema/verbs/ingest.json                 EDIT  + mode arg, flag mappings
crates/cairn-idl/schema/verbs/forget.json                 EDIT  + mode arg, flag mappings
crates/cairn-cli/src/generated/verbs.rs                   REGEN cairn-codegen
crates/cairn-test-fixtures/src/flush_plan.rs              NEW   plan generators
crates/cairn-test-fixtures/src/lib.rs                     EDIT  pub mod flush_plan
docs/design/traceability.md                               EDIT  §5.5 → #54
docs/site/src/reference/generated/                        REGEN cairn-docgen
```

## 13. Verification (CLAUDE.md §8)

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
mdbook build docs/site
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
cargo deny check
cargo audit --deny warnings
cargo machete
```

## 14. Risks and open questions

- **`maps_to` codegen extension.** If `x-cairn-cli.flags` doesn't currently
  support folding two bool flags onto one enum field, we either extend the
  codegen (preferred) or emit the bool flags raw and fold them in
  `verbs/ingest.rs`. Implementation plan picks one after reading
  `cairn-codegen` source.
- **WAL-state-machine integration.** `apply` walks `MemoryStore` directly in
  this PR. When the WAL adapter lands (#9 / #55), `apply` becomes
  `wal::issue(plan).await` and the loop moves into the WAL consumer. This
  PR ships the producer + persistence; the swap is mechanical.
- **Lock acquisition during apply.** Brief §5.6 requires per-`(scope,
  entity_id)` locks before mutation. P0 with the FixtureStore has no lock
  table; SQLite adapter has one but isn't wired through MemoryStore yet.
  This PR documents the gap and routes apply through `MemoryStore` calls
  that will pick up locking transparently when the adapter exposes it.
- **No transaction primitive on `MemoryStore`.** The current trait exposes
  per-row `upsert` / `tombstone` / `put_edge` calls with no batch or
  transaction method. Apply walks them sequentially; a phase-2 mutation
  failure leaves a partial commit visible to readers. Mitigated by the
  phase-1 drift check (§7.2) and by poisoning the plan to `rejected/` with
  the failing mutation index. Replaced by single-transaction WAL apply in
  the next epoch.
- **Diff renderer fidelity.** First pass renders shallow before/after for
  body excerpts (≤ 4 KB) and full enum-variant detail for non-Upsert
  mutations. Rich diffs (semantic markdown diff, syntax highlight) are P1.

## 15. Out of scope

- Workflow scheduling after apply (per issue text).
- Hooking the producer side of FlushPlan into the live ingest pipeline
  (#9 / #46+).
- WAL state-machine implementation (#55).
- MCP-side `flush_apply` / `flush_list` verbs (would require brief
  amendment).
- Plan signing / countersignatures — covered by §5.6 envelope work in #51
  follow-ons.
