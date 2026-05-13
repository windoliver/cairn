# Issue #53 — `status` + capability negotiation parity across surfaces

- **Issue**: [#53](https://github.com/windoliver/cairn/issues/53)
- **Parent epic**: #7 (identity, signed envelope, status, handshake)
- **Brief sections**: §8.0.a (handshake / status), §8.0 (core verbs), §15 (wire-compat), §19 (sequencing)
- **Phase**: v0.1 — Minimum substrate
- **Date**: 2026-05-06

## 1. Goal

Land the parity *infrastructure* so that any capability advertised by `cairn
status` is reported identically by every surface (CLI, MCP `initialize`, SDK,
skill) and is enforced end-to-end fail-closed via `CapabilityUnavailable`.
Convert the issue's acceptance criteria into live tests + version-pinned rules
so the next verb landing (forget runtime, retrieve runtime, replay dispatch)
automatically picks up correct advertisement without re-deriving the rules.

## 2. Non-goals

- **Advertising new capabilities whose runtime is still stubbed**
  (`forget.record`, `retrieve.*`, `replay.{sequence,challenge}`). Each lands
  with the issue that wires its dispatch end-to-end. Brief §15 forbids
  over-advertising; this matches the existing precedent where `replay.*` is
  held back behind a comment in `crates/cairn-cli/src/verbs/status.rs` until
  the signed-verb dispatch path routes through `prepare_wal_with_replay`.
- **Adding new top-level fields** to `StatusResponse` (no `active_vault`, no
  `plugins[]`, no `sensors[]`, no `workflows[]`, no `handshake_modes[]`). The
  brief §8.0.a wire shape is locked at `{contract, server_info, capabilities,
  extensions, pipeline_dispatch?}`; modes ride inside the capability strings.
  The issue's "Implementation Detail" prose is satisfied by the existing
  capability strings (`cairn.mcp.v1.search.*`, `.forget.*`, `.retrieve.*`,
  `.replay.*`) and the `extensions[]` array (plugins).
- **Skill bundle content changes.** Per brief §8.0.a the skill surface is
  `cairn status --json | jq '.capabilities'` — skill parity is satisfied
  transitively by CLI parity. No SKILL.md edits.
- **Daemon table or per-incarnation state caching.** P0 mints
  `incarnation` / `started_at` per call (see existing `cairn-cli` and
  `cairn-sdk` comments deferring to issue #9). This issue does not change
  that.

## 3. Background — current state

### 3.1 What exists

- `StatusResponse` (generated from `crates/cairn-idl/schema/prelude/status.json`)
  has the brief-locked fields plus the issue #217 `pipeline_dispatch`
  advertisement.
- `Capabilities` enum (generated from
  `crates/cairn-idl/schema/capabilities/capabilities.json`) is closed at
  v0.1 and pins each capability's `x-cairn-since` phase.
- `CairnMcpHandler` (in `crates/cairn-mcp/src/handler.rs`) returns a static
  `ServerInfo` with rmcp's default capabilities for MCP `initialize`. Its
  `tools/list` returns the IDL-generated `TOOLS` array unconditionally and
  routes `tools/call` either to the wired search dispatcher or to a stub.
- CLI's `compute_capabilities`
  (`crates/cairn-cli/src/verbs/status.rs:213`) and SDK's
  `advertised_capabilities` (`crates/cairn-sdk/src/transport.rs:186`) are
  parallel implementations of the same logical decision, gated on different
  inputs (CLI: vault-binding sentinel + filesystem stat for model presence;
  SDK: `MemoryStore::capabilities()` + store FTS/vector flags).
- `Sdk::require_capability` already fail-closes on un-advertised modes,
  emitting `SdkError::CapabilityUnavailable`.
- `crates/cairn-cli/tests/sdk_cli_parity.rs` already deep-equals CLI vs SDK
  status JSON (modulo volatile fields).
- `CapabilityUnavailable` error data carries only `capability`. No
  remediation hint.
- Existing snapshot test (`status_snapshot.rs`) is structural only — no
  insta snapshots; no degraded-config fixtures.

### 3.2 ACs vs current state

| AC | Status today |
|---|---|
| Status reports keyword/semantic/hybrid when local embeddings on | ✓ wired (CLI + SDK) |
| Status reports record-level forget only for v0.1 | ✗ not advertised yet (forget runtime stubbed) |
| MCP tool declarations match runtime status capabilities | ✗ MCP `initialize` is static; no parity test |
| Status snapshot tests for default + degraded configs | ✗ only one structural test |
| Unsupported-mode rejection tests | ◐ partial — search has them; forget/retrieve don't |
| CLI/MCP/SDK parity tests | ◐ CLI/SDK exists; MCP missing |

## 4. Architecture

### 4.1 Single source of truth — `cairn-core::status::advertise`

A new pure function in `cairn-core` is the one place capability decisions
are made. Both CLI and SDK delegate; MCP `initialize` delegates too.

```rust
// crates/cairn-core/src/status/mod.rs

#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase { V0_1, V0_2, V0_3 }

#[derive(Debug, Clone)]
pub struct StoreCaps {
    pub fts: bool,
    pub vector: bool,
}

#[derive(Debug, Clone)]
pub struct CapabilityGates {
    pub config: CapabilitySet,         // existing CairnConfig::capabilities() output
    pub store: Option<StoreCaps>,      // None → no store wired
    pub vault_bound: bool,             // CLI: vault.id sentinel; SDK: store.is_some()
    pub model_present: bool,           // CLI: ModelCache stat; SDK: store_caps.vector
    pub llm_configured: bool,          // P0: false; LLMProvider configured
    pub contract_phase: Phase,         // pins forget.session/scope, replay.*
}

#[must_use]
pub fn advertise(gates: &CapabilityGates) -> Vec<Capabilities> { /* table below */ }

#[must_use]
pub fn remediation_for(capability: &str) -> Option<&'static str> { /* §4.3 */ }
```

### 4.2 Decision table

`advertise()` walks the table top-to-bottom and pushes each capability whose
gate evaluates to `true`. Order in the output Vec matches the table order so
snapshots stay stable. Empty Vec when `vault_bound == false`.

| Capability | Gate |
|---|---|
| `cairn.mcp.v1.search.keyword` | `bound && config.keyword_search && store_ok(fts)` |
| `cairn.mcp.v1.search.semantic` | `bound && config.semantic_search && model_present && store_ok(vector)` |
| `cairn.mcp.v1.search.hybrid` | `bound && config.hybrid_search && model_present && store_ok(fts) && store_ok(vector)` |

Where `store_ok(field)` = `gates.store.as_ref().map_or(true, |s| s.field)` — when
no store is wired (CLI status path), the bound-vault short-circuit is the
structural backstop: any v0.1 bound vault has the FTS virtual table from
the SQLite schema migration, and `model_present` is computed from
`ModelCache` on disk. When a store *is* wired (SDK / MCP), the store's
self-advertised flags veto. The `Sdk::new()` (no store) case is already
short-circuited by `vault_bound = false` → empty Vec.
| `cairn.mcp.v1.policy_trace` | `bound && config.policy_trace` |
| `cairn.mcp.v1.forget.record` | `bound && phase >= V0_1 && wiring::FORGET_RECORD_WIRED` |
| `cairn.mcp.v1.forget.session` | `bound && phase >= V0_2 && wiring::FORGET_SESSION_WIRED` |
| `cairn.mcp.v1.forget.scope` | `bound && phase >= V0_3 && wiring::FORGET_SCOPE_WIRED` |
| `cairn.mcp.v1.retrieve.record` | `bound && wiring::RETRIEVE_RECORD_WIRED` |
| `cairn.mcp.v1.retrieve.session` | `bound && wiring::RETRIEVE_SESSION_WIRED` |
| `cairn.mcp.v1.retrieve.turn` | `bound && wiring::RETRIEVE_TURN_WIRED` |
| `cairn.mcp.v1.retrieve.folder` | `bound && wiring::RETRIEVE_FOLDER_WIRED` |
| `cairn.mcp.v1.retrieve.scope` | `bound && wiring::RETRIEVE_SCOPE_WIRED` |
| `cairn.mcp.v1.retrieve.profile` | `bound && wiring::RETRIEVE_PROFILE_WIRED` |
| `cairn.mcp.v1.replay.sequence` | `bound && wiring::REPLAY_SEQUENCE_WIRED` |
| `cairn.mcp.v1.replay.challenge` | `bound && wiring::REPLAY_CHALLENGE_WIRED` |

Each `*_WIRED` is a `pub const bool` in
`cairn-core::status::wiring` set to `false` in this PR. The issue that
lands the corresponding runtime flips that single constant. CLI, SDK, and
MCP all pick up the change through one delegation — there is no fourth
place to update.

`fts(None) == false`, `vector(None) == false` so a no-store gates struct
(SDK `Sdk::new()`, CLI without a wired store) returns the empty Vec.

`vault_bound == false` short-circuits to empty Vec — no per-row gating
needed below the bound check. This preserves the existing CLI behavior
where a non-vault directory advertises nothing.

### 4.3 Remediation map

`cairn-core::status::REMEDIATION` is a `&[(&str, &str)]` table queried
through `remediation_for(capability_string) -> Option<&'static str>`.

| Capability | Remediation |
|---|---|
| `cairn.mcp.v1.search.semantic` | `set search.local_embeddings: true in .cairn/config.yaml and run cairn embed download` |
| `cairn.mcp.v1.search.hybrid` | (same as semantic) |
| `cairn.mcp.v1.policy_trace` | `policy_trace is enabled by default; check .cairn/config.yaml for an explicit override` |
| `cairn.mcp.v1.forget.session` | `forget.session ships in v0.2; upgrade to a v0.2+ runtime` |
| `cairn.mcp.v1.forget.scope` | `forget.scope ships in v0.3; upgrade to a v0.3+ runtime` |
| `cairn.mcp.v1.replay.sequence` | `signed-intent replay protection requires a wired challenge dispatch path; not available in this build` |
| `cairn.mcp.v1.replay.challenge` | (same) |

Capabilities not in the map → `remediation_for` returns `None` and the
caller omits `data.remediation` from the error envelope.

### 4.4 Surface delegation

- **CLI** (`crates/cairn-cli/src/verbs/status.rs`): `compute_capabilities`
  retains its filesystem probes (vault sentinel via
  `probe_vault_binding`, `ModelCache::is_present`) but the *decisions*
  move to `cairn_core::status::advertise`. The function builds a
  `CapabilityGates` and calls `advertise(&gates)`. Existing TOCTOU /
  fail-closed gates above the call (the `require_bound` arm, the
  `Invalid` sentinel exit) stay where they are — they are CLI-specific
  policy, not capability decisions.
- **SDK** (`crates/cairn-sdk/src/transport.rs`):
  `Sdk::advertised_capabilities` collapses to `let gates = self.gates();
  cairn_core::status::advertise(&gates)`. The `gates()` helper builds
  from `self.store.as_ref().map(|s| s.capabilities())` and `self.config`.
- **MCP** (`crates/cairn-mcp/src/handler.rs`): `get_info` calls
  `advertise()` with gates built from `self.store` + `self.config`. The
  full `StatusResponse` block (capabilities, extensions, server_info,
  pipeline_dispatch) is packed into rmcp's `ServerInfo` extension fields
  so MCP `initialize` carries the same data shape as `cairn status
  --json`.

### 4.5 Wire shape

No change to `StatusResponse`. No change to `capabilities.json`. Only
schema change is the optional `remediation` on
`CapabilityUnavailableData` (§5.1).

## 5. IDL changes

### 5.1 Error schema — `crates/cairn-idl/schema/errors/error.json`

```diff
   "CapabilityUnavailableData": {
     "type": "object",
     "additionalProperties": false,
     "required": ["capability"],
     "properties": {
-      "capability": { "$ref": "../capabilities/capabilities.json" }
+      "capability":  { "$ref": "../capabilities/capabilities.json" },
+      "remediation": { "type": "string", "minLength": 1 }
     }
   }
```

`remediation` is **not** in `required[]` — pre-#53 servers' responses
deserialize cleanly against the new schema (forward compat). Newer
servers populate it for every fail-closed rejection.

`additionalProperties: false` already there → the new key needs to be
declared explicitly. This is a wire-additive change, not a wire-breaking
one (no existing field renamed, no required field added).

### 5.2 Codegen artifacts to regenerate and commit

- `crates/cairn-core/src/generated/errors/mod.rs` — `CapabilityUnavailableData`
  gains an optional `remediation: Option<String>` field with
  `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- `crates/cairn-mcp/src/generated/schemas/errors/error.json` — mirror copy.
- Snapshot updates for any `cargo insta`-tracked codegen output.

## 6. Surface implementations

### 6.1 `cairn-core::status` module

New module under `crates/cairn-core/src/status/`:

```
status/
├── mod.rs              # advertise(), CapabilityGates, Phase, StoreCaps
├── wiring.rs           # pub const *_WIRED: bool flags (all false in this PR)
└── remediation.rs      # REMEDIATION map + remediation_for()
```

**No new dependencies.** Uses existing `cairn-core::config::CapabilitySet`
and `cairn-core::generated::common::Capabilities`. Pure function — no
async, no I/O, no allocation beyond the result Vec.

**Unit tests** (table-driven via `rstest`):

- Each capability row × {vault_bound true/false} × {phase × wiring
  flag set}. ~40 cases.
- Property test (`proptest`): for any `CapabilityGates`, the output
  is monotone in each gate (turning a gate on never removes a
  capability). Cheap invariant; catches accidental conjunction
  inversions.

### 6.2 CLI `verbs/status.rs`

Replace `compute_capabilities`'s body:

```rust
fn compute_capabilities(
    vault_root: Option<&Path>,
    config: Option<&CairnConfig>,
    bound: bool,
) -> Vec<Capabilities> {
    let Some(config) = config else { return vec![]; };
    if !bound { return vec![]; }
    let model_present = vault_root.is_some_and(|root| {
        let cache = ModelCache::new(&root.join(".cairn").join("models"));
        cache.is_present(config.search.embedding_model)
    });
    cairn_core::status::advertise(&CapabilityGates {
        config: config.capabilities(model_present),
        store: None, // CLI status path does not open the store
        vault_bound: bound,
        model_present,
        llm_configured: false, // P0
        contract_phase: Phase::V0_1,
    })
}
```

The TOCTOU / `require_bound` / `Invalid` sentinel gates above stay
unchanged. `p0_capabilities_advertises` keeps its current signature but
delegates to `advertise()` with the default config.

**Note on `store: None` from CLI.** The CLI `status` path deliberately
does not open the SQLite store (status must be cheap and read-only).
Without a store the FTS/vector gates fail-closed → CLI never advertises
search modes from the no-store path. This matches existing CLI behavior
(`compute_capabilities` already advertises only what the bound vault +
filesystem-detected model permit). When a future issue wires the CLI
status path to the store, it sets `store: Some(...)` and the gates
unify with the SDK path automatically.

### 6.3 SDK `transport.rs`

Replace `advertised_capabilities`:

```rust
fn advertised_capabilities(&self) -> Vec<Capabilities> {
    cairn_core::status::advertise(&self.gates())
}

fn gates(&self) -> CapabilityGates {
    let store_caps = self.store.as_ref().map(|s| {
        let c = s.capabilities();
        StoreCaps { fts: c.fts, vector: c.vector }
    });
    let model_present = store_caps.as_ref().is_some_and(|c| c.vector);
    CapabilityGates {
        config: self.config.capabilities(model_present),
        store: store_caps,
        vault_bound: self.store.is_some(),
        model_present,
        llm_configured: false,
        contract_phase: Phase::V0_1,
    }
}
```

`require_capability` keeps its existing shape but constructs
`SdkError::CapabilityUnavailable { capability, remediation, ... }` —
remediation pulled from `cairn_core::status::remediation_for(cap)`.

### 6.4 MCP handler

`CairnMcpHandler::get_info` is rewritten to produce the full status
block. rmcp's `ServerInfo` carries arbitrary serializable extension
fields; pack the `StatusResponse` JSON under a Cairn-namespaced key
(e.g. `cairn.status`). Clients reading MCP `initialize` see the same
JSON as `cairn status --json` (modulo volatile fields).

```rust
fn get_info(&self) -> ServerInfo {
    let gates = build_gates(self.store.as_ref(), &self.config);
    let status = StatusResponse {
        contract: "cairn.mcp.v1".to_owned(),
        server_info: StatusResponseServerInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            build: build_profile(),
            started_at: now_rfc3339_seconds(),
            incarnation: new_operation_id(),
        },
        capabilities: cairn_core::status::advertise(&gates),
        extensions: vec![],
        pipeline_dispatch: Some(pipeline_dispatch_advertisement(&DefaultRegistry)),
    };
    ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new("cairn", env!("CARGO_PKG_VERSION")))
        .with_extensions(serde_json::json!({ "cairn.status": status }))
}
```

`tools/list` is unchanged — verb roots are mandatory surfaces per
`capabilities.json`'s `x-cairn-mandatory-surfaces`. Mode-level filtering
happens at `tools/call` via `CapabilityUnavailable`.

`tools/call` rejection paths populate `remediation` via
`cairn_core::status::remediation_for(cap)` — see §6.5.

### 6.5 Remediation propagation

Every site that constructs `CapabilityUnavailable` is updated to call
`remediation_for(cap)` and populate `data.remediation`. Audit list:

- `crates/cairn-sdk/src/transport.rs:require_capability` (search-mode
  gate, retrieve-target gate, forget-target gate).
- `crates/cairn-sdk/src/transport.rs:search` (dispatcher rejection
  arm).
- `crates/cairn-core/src/verbs/search/...` — wherever
  `SearchError::CapabilityUnavailable` is produced (the dispatcher
  rejects unsupported modes there).
- `crates/cairn-mcp/src/handler.rs:handle_search` (`CallToolResult`
  text now mirrors envelope shape with remediation).
- `crates/cairn-cli/src/verbs/search.rs` (CLI's `--explain` rejection
  path; `--mode semantic` rejection path) — populate the JSON envelope
  on `--json`, append a remediation line on human output.
- Any future `forget` / `retrieve` `CapabilityUnavailable` site adopts
  the same lookup automatically by importing `remediation_for`.

CLI human output prints remediation as a hint line:

```
cairn search: capability unavailable — cairn.mcp.v1.search.semantic
  hint: set search.local_embeddings: true in .cairn/config.yaml and run cairn embed download
```

JSON output includes it inside the envelope's `error.data.remediation`.

## 7. Tests

### 7.1 Snapshot matrix — `crates/cairn-cli/tests/status_snapshot_insta.rs`

Insta snapshots, volatiles masked
(`server_info.incarnation`, `server_info.started_at`).

| Fixture | Configuration | Asserted shape |
|---|---|---|
| `default_p0` | bound vault, default config, model on disk | search.{keyword,semantic,hybrid} + policy_trace |
| `local_embeddings_off` | bound vault, `search.local_embeddings: false` | search.keyword + policy_trace |
| `model_missing` | bound vault, default config, no model file | search.keyword + policy_trace |
| `unbound_dir` | tempdir without `.cairn/vault.id` | empty |
| `no_store_sdk` | `Sdk::new()` | empty |

Snapshots committed under `crates/cairn-cli/tests/snapshots/`.

### 7.2 Cross-surface parity — extends `sdk_cli_parity.rs`

New test `mcp_initialize_parity_three_way`:

- Build same `(store, config)` pair into a `CairnMcpHandler` and an
  `Sdk::with_store`.
- Spawn `cairn` binary with same vault.
- Capture each surface's status JSON, mask volatiles, deep-equal.
- Repeat for each fixture in §7.1 — every config matrix has
  three-surface byte-identity.

The existing two-way `status_parity_cli_vs_sdk` is preserved.

### 7.3 Fail-closed rejection — `crates/cairn-cli/tests/cli_capability_rejection.rs`

- `cairn search --mode semantic` against a vault with
  `local_embeddings: false` → exit 69, JSON envelope:
  `status: "rejected"`, `error.code: "CapabilityUnavailable"`,
  `error.data.capability:
  "cairn.mcp.v1.search.semantic"`, `error.data.remediation` non-empty
  and matches the table.
- `cairn search --explain` with policy_trace gated off (synthetic
  config) → same shape, capability `cairn.mcp.v1.policy_trace`.
- SDK equivalent in `crates/cairn-sdk/tests/surface.rs`.
- MCP equivalent: instantiate `CairnMcpHandler::with_store` with a
  store whose `capabilities().vector == false`, drive a `tools/call`
  for `search` with `mode: "semantic"`, assert `CallToolResult`
  carries the same envelope.

### 7.4 Future-advertisement assertions — `crates/cairn-core/tests/status_phase_pinning.rs`

- Build `CapabilityGates { contract_phase: V0_1, ... }` with every
  `*_WIRED` flag flipped on; assert `forget.session` / `forget.scope`
  / `replay.*` are absent (phase still pinned them off).
- Build with `phase: V0_2` + `FORGET_SESSION_WIRED: true`; assert
  `forget.session` appears.
- Confirms version-pinned rules survive future refactors.

### 7.5 Property test — monotonicity

In `crates/cairn-core/src/status/tests.rs`: `proptest` that for any two
gates `a, b` where `a` dominates `b` field-by-field (every bool ≥, phase
≥), `advertise(b) ⊆ advertise(a)`. Catches accidental conjunction
inversions in §4.2's table.

### 7.6 Remediation existence test

For every capability advertised at v0.1 with all wiring flags on
(`search.{keyword,semantic,hybrid}`, `policy_trace`), verify
`remediation_for(cap_string)` returns `Some(_)` so users always get a
hint when one of those is rejected. This pins the remediation map to
the advertise table.

## 8. Implementation slice ordering

Each step compiles, passes existing tests, and lands as its own commit.
PR is a single branch.

1. **IDL — error schema.** Add optional `remediation` to
   `CapabilityUnavailableData`. Run `cargo run -p cairn-idl --bin
   cairn-codegen`. Commit generated `errors/mod.rs` +
   `cairn-mcp/src/generated/schemas/errors/error.json`.
2. **`cairn-core::status` module.** Create `mod.rs`, `wiring.rs`,
   `remediation.rs`. All `*_WIRED` constants `false`. Unit tests +
   property test land here.
3. **CLI delegation.** Replace `compute_capabilities` body. Existing
   CLI tests stay green.
4. **SDK delegation.** Replace `advertised_capabilities` body. Existing
   `sdk_cli_parity` test stays green.
5. **MCP `initialize` parity.** Extend `get_info` to emit the full
   status block. Add `mcp_initialize_parity_three_way` parity test
   matrix.
6. **Remediation wiring.** Update every `CapabilityUnavailable`
   construction site (audit list in §6.5).
7. **Snapshot matrix.** Add `status_snapshot_insta.rs`. Run
   `cargo insta review`. Commit baselines.
8. **Phase-pinning + remediation tests.** Add
   `status_phase_pinning.rs`, remediation-existence test.
9. **Docs.** Update CLAUDE.md §4.6 to point at `cairn-core::status::advertise`
   as ground truth. Update `docs/design/traceability.md` row for §8.0.a /
   §15 mapping #53 to the new module.

## 9. Verification

Per CLAUDE.md §8 verification checklist:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check  # CLI flags / capability docs may shift
```

PR description cites brief §8.0.a and §15, lists the invariants
touched (no over-advertising, fail-closed enforcement, single source
of truth in `cairn-core`), and pastes verification output.

## 10. Risks & mitigations

| Risk | Mitigation |
|---|---|
| MCP `initialize` extension key collides with an rmcp internal key | Namespace under `cairn.status` per the brief's contract namespace. The rmcp `ServerInfo` `extensions` field is JSON; conflicts surface as deserialization errors at the MCP↔harness boundary, caught by the parity test. |
| `cairn-core::status` becomes an attractive nuisance for unrelated runtime decisions | Keep the module surface tight: only `advertise`, `remediation_for`, `CapabilityGates`, `Phase`, `StoreCaps`, `wiring::*`. Document at module top: "decisions about which capabilities are *advertised*. Not for: dispatch gating (use the per-verb error type), config validation (use `cairn-core::config`), runtime feature toggles (use feature flags)." |
| Future capability added to the IDL but not to the table → silent under-advertising | Capabilities enum is `#[non_exhaustive]`; `advertise()` matches over the closed enum and returns the `Vec` in deterministic order. Add an exhaustiveness proof: the test in §7.6 iterates `Capabilities::all()` (a generated method) and asserts every variant is mentioned in the decision table. Forces the table to be updated when the enum grows. |
| `pipeline_dispatch` advertisement diverges between MCP and CLI | Both call `pipeline_dispatch_advertisement(&DefaultRegistry)` from `cairn-core::pipeline::dispatch` — same source, no divergence. |
| Remediation strings drift over time as runtime details change | Remediation table is committed source code; PRs that change runtime behavior must update the table. The test in §7.6 forces the table to stay in sync with the advertised set. |

## 11. Out of scope (P1+ follow-ups)

- Cloud provider health and SRE telemetry (per issue #53 "Out of Scope").
- Per-incarnation status caching backed by the daemon table (#9).
- Extension-namespace advertisement (`cairn.aggregate.v1`,
  `cairn.federation.v1`, `cairn.sessiontree.v1`) — already wired via
  the schema's `allOf` extension/capability bindings; this issue does
  not touch that.
- Live `tools/list` filtering of mode discriminants — would require
  per-incarnation IDL schema mutation; brief §15 byte-identity rules
  out runtime schema drift.

## 12. Traceability

| Brief section | Coverage in this issue |
|---|---|
| §8.0.a — handshake / status preludes | `advertise()` is the single producer; CLI/SDK/MCP delegate. Wire-compat (a)/(b)/(c) enforced by §7.2 parity matrix. |
| §15 — wire-compat | Snapshot matrix (§7.1) + parity matrix (§7.2) make CI fail on drift. Phase-pinning (§7.4) makes future advertisement opt-in. |
| §19 — sequencing | `Phase` enum + `wiring` constants encode the v0.1 → v0.2 → v0.3 sequencing. |
| §4.6 (CLAUDE.md) — fail-closed | `require_capability` keeps its shape; remediation makes operator UX better; tests in §7.3 lock the contract. |
