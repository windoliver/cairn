# Plugin host, `cairn plugins list`, and `cairn plugins verify`

**Issue:** [#143](https://github.com/windoliver/cairn/issues/143) — *[P0] Implement
plugin registration, capability manifests, and `cairn plugins verify`*.

**Brief sections:** §4.0 Contracts, §4.1 Plugin architecture, §13.3 Commands.

**Status:** Design — drafted 2026-04-25.

---

## 1. Goal

Close the remaining acceptance criteria of issue #143 by:

1. Wiring a live, in-process plugin host into `cairn-cli` so the four
   already-existing bundled adapter crates register themselves through
   `PluginRegistry` at startup of plugin-using commands.
2. Adding two CLI commands: `cairn plugins list` and `cairn plugins verify`.
3. Adding a two-tier conformance suite in `cairn-core::contract::conformance` that the
   `verify` command runs against every registered plugin.
4. Wiring `cairn plugins verify` into CI so a regression in any bundled
   plugin's manifest, identity, version range, or capability self-consistency
   breaks the build.

The existing landed work (PR #174 / commit `f74b0c8`) provides contract
traits, `CONTRACT_VERSION` constants, `PluginRegistry`, `register_plugin!`,
and `PluginManifest`. This design builds on that foundation only — it does
not modify it.

## 2. Scope

### 2.1 In scope

- New `crates/cairn-cli/src/plugins/` module: host, list, verify.
- A `clap`-derived subcommand tree on `cairn-cli` introducing the
  `plugins {list, verify}` subcommands. **`clap` is added as a new
  workspace dependency by this PR** (no current `Cargo.toml` mention).
  The existing scaffold's argv matching is replaced by `clap` only for
  the `plugins` subtree; help and version remain trivial.
- Per-bundled-crate work: `plugin.toml`, `register()` function emitted
  through `register_plugin!`, stub trait impl exposing honest capability
  flags and `CapabilityUnavailable`-returning verb methods. Four crates:
  `cairn-store-sqlite`, `cairn-sensors-local`, `cairn-mcp`,
  `cairn-workflows`.
- New `cairn-core::contract::conformance` module with one submodule per
  contract, two tiers of cases. *(Lives in `cairn-core` rather than
  `cairn-test-fixtures` because `cairn plugins verify` is a production
  code path and CLAUDE.md §3 forbids `cairn-test-fixtures` as a non-dev
  dep. The conformance suite is pure functions — no I/O — so it
  satisfies the `cairn-core` purity rule.)*
- CI integration in `.github/workflows/ci.yml` plus a `cargo`-test wrapper
  in `crates/cairn-cli/tests/plugins_verify.rs`.
- `PluginRegistry` accessor `parsed_manifest(&PluginName)` so `verify` can
  reach the manifest the host parsed at registration time.

### 2.2 Out of scope (deferred to dedicated issues)

- Real `MemoryStore` / `MCPServer` / `WorkflowOrchestrator` /
  `SensorIngress` implementations beyond what tier-1 cases require.
- `LLMProvider` bundled plugin (no crate exists yet).
- `.cairn/config.yaml` loader and active-set selection between competing
  plugins for one contract.
- Verb dispatch calling `register_all()` (lands when the verb layer does).
- Third-party plugin discovery, signing, or sandboxing.

## 3. Architecture

### 3.1 Crate diagram

```
cairn-cli ──┬─ depends-on ──> cairn-core (PluginRegistry, contract traits)
            ├─ depends-on ──> cairn-store-sqlite       (calls register())
            ├─ depends-on ──> cairn-sensors-local      (calls register())
            ├─ depends-on ──> cairn-mcp                (calls register())
            └─ depends-on ──> cairn-workflows          (calls register())

each adapter crate ─ depends-on ─> cairn-core (contract traits, register_plugin!)

cairn-test-fixtures stays dev-only (CLAUDE.md §3) — not on the
production dep graph above.
```

Only `cairn-cli` gains direct deps on the four adapter crates. No adapter
crate depends on another. `cairn-core` retains its zero-workspace-dep
invariant.

### 3.2 Discovery: hard-coded deps

`cairn-cli/src/plugins/host.rs` calls each adapter crate's public
`register(&mut PluginRegistry)` function in alphabetical order:

```rust
pub fn register_all() -> Result<PluginRegistry, PluginError> {
    let mut reg = PluginRegistry::new();
    cairn_mcp::register(&mut reg)?;
    cairn_sensors_local::register(&mut reg)?;
    cairn_store_sqlite::register(&mut reg)?;
    cairn_workflows::register(&mut reg)?;
    Ok(reg)
}
```

No `inventory`, no build-script codegen. Adding a fifth bundled plugin
edits this function and `Cargo.toml`. The brief invariant "registration is
explicit" (§4.1) is satisfied at the source level.

### 3.3 Manifest convention per adapter crate

Each bundled crate gains:

- `crates/<crate>/plugin.toml` — TOML manifest, schema-validated by
  `crates/cairn-idl/schema/plugin/manifest.json`.
- `pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");` — the
  manifest is baked into the binary. No filesystem access at runtime.
- A single `pub fn register(reg: &mut PluginRegistry) -> Result<(),
  PluginError>` emitted by `register_plugin!`. The macro is extended to
  accept the `MANIFEST_TOML` constant and forward the parsed
  `PluginManifest` into the registry alongside the `Arc<dyn Trait>`.

### 3.4 Registry extension

`PluginRegistry` gains a single global `HashMap<PluginName,
PluginManifest>`, populated alongside whichever per-contract `Arc<dyn
Trait>` map the plugin registers into. One global map (rather than one
parallel map per contract) enforces global `PluginName` uniqueness:
even if the per-contract impl maps would tolerate the same name in two
contract slots, the global manifest map rejects the second
`register_*_with_manifest` call with `PluginError::DuplicateName`. This
matches the brief's "stable identifier of a plugin instance" framing
(§4.1) — a `PluginName` identifies one plugin, not one (plugin,
contract) pair. New accessors:

```rust
impl PluginRegistry {
    pub fn parsed_manifest(&self, name: &PluginName) -> Option<&PluginManifest>;
    pub fn parsed_manifests_sorted(&self) -> Vec<(&PluginName, &PluginManifest)>;
}
```

`parsed_manifests_sorted` returns alphabetically-ordered manifests so
`cairn plugins list` / `verify` get stable output without reaching into
the underlying `HashMap`.

The existing seven `register_<contract>()` methods are **kept unchanged**
to preserve the public API just shipped by PR #174. Each gets a sibling
`register_<contract>_with_manifest(name, manifest, plugin)` method that
performs the same registration and additionally stores the parsed
manifest in the global map (failing closed if the name is already
present there). The macro-emitted four-arg form (§6.1) calls the
`_with_manifest` variant; the three-arg form keeps calling the original.

This avoids a churn-y signature change and lets unit tests that don't
care about manifests stay terse, while the bundled-plugin path always
carries a manifest.

### 3.5 CLI structure

`cairn-cli/src/main.rs` stops being a hand-rolled argv matcher. It
adopts `clap` 4.5 — added as a new workspace dependency by this PR
per CLAUDE.md §6.5 — with a minimal command tree:

```
cairn
├── --help / -h           (skips register_all)
├── --version / -V        (skips register_all)
└── plugins
    ├── list  [--json]
    └── verify [--strict] [--json]
```

Future verb subcommands (`ingest`, `search`, …) slot in alongside
`plugins`. They are out of scope here. The existing scaffold's behaviour
of refusing unknown verbs is preserved by `clap`'s usage error (exit 2).

### 3.6 Startup wiring

`register_all()` is invoked **only** by the `plugins` subcommand. `--help`,
`--version`, and unknown commands do not trigger plugin registration.
This matches the AC ("startup fails closed when a plugin contract version
is incompatible") for any command that does work, without bricking
trivial commands.

## 4. Conformance suite (two tiers)

### 4.1 Layout

```
crates/cairn-core/src/contract/conformance/
├── mod.rs                  -- CaseOutcome, CaseStatus, Tier,
│                              run_conformance_for_plugin entry point
├── memory_store.rs
├── workflow_orchestrator.rs
├── sensor_ingress.rs
└── mcp_server.rs
   -- llm_provider.rs deferred until an LLM bundled crate exists.
   -- frontend_adapter.rs / agent_provider.rs deferred (P1/P2).
```

The module is `pub` from `cairn-core::contract`. `cairn-cli` consumes it
directly. `cairn-core`'s no-I/O rule is preserved because every case is
either a pure trait-method call or returns `Pending`.

### 4.2 Public types

```rust
pub struct CaseOutcome {
    pub id: &'static str,
    pub tier: Tier,
    pub status: CaseStatus,
}

pub enum Tier { One, Two }

pub enum CaseStatus {
    Ok,
    Pending { reason: &'static str },
    Failed { message: String },
}
```

### 4.3 Per-contract case set

Tier-1 cases are identical across contracts (defined once and reused with
generic helpers); tier-2 cases are contract-specific function stubs.

| Contract | Tier-1 (always run) | Tier-2 (`Pending` until impl) |
|---|---|---|
| `MemoryStore` | `manifest_matches_host`, `arc_pointer_stable`, `capability_self_consistency_floor` | `put_get_roundtrip`, `fts_query_returns_doc`, `vector_search_when_advertised` |
| `WorkflowOrchestrator` | (same three) | `enqueue_then_complete` |
| `SensorIngress` | (same three) | `emits_envelope_when_poked` |
| `MCPServer` | (same three) | `initialize_and_list_tools` |

Tier-1 specifics:

- `manifest_matches_host` — call `PluginManifest::verify_compatible_with`
  with the plugin's runtime `name()`, contract kind, and host
  `CONTRACT_VERSION`.
- `arc_pointer_stable` — after registration, look up the plugin by name
  twice and assert `Arc::ptr_eq` between the two results. Verifies the
  registry returns a stable pointer to the same plugin instance across
  lookups (the underlying `HashMap::get(name).cloned()` invariant).
- `capability_self_consistency_floor` — every public field of the
  capability struct is readable; the trait's runtime methods do not panic
  when called for a capability the plugin does **not** advertise (they
  must return a typed `CapabilityUnavailable` error). This is the
  minimum floor; per-contract invariants (e.g.,
  `caps.fts == true ⇒ supports_fts() == true`) tighten in the per-impl
  PRs.

Tier-2 stubs are real Rust functions in
`cairn-core::contract::conformance::<contract>` whose bodies return
`CaseStatus::Pending { reason: "real impl pending" }` until a follow-up
PR replaces them. Real tier-2 bodies that need adapter-specific I/O
(e.g., a SQLite round-trip beyond the trait surface) are not surfaced
through this entry point — they live in per-adapter integration tests
and are run by `cargo nextest`, not by `cairn plugins verify`. The
conformance suite is the lowest-common-denominator surface that runs
against any plugin via the trait alone.

### 4.4 Entry point

```rust
pub fn run_conformance_for_plugin(
    registry: &PluginRegistry,
    name: &PluginName,
) -> Vec<CaseOutcome>;
```

This is the function `cairn plugins verify` calls. It dispatches by
contract kind (read from the parsed manifest) to the per-contract case
runner.

## 5. CLI commands

### 5.1 `cairn plugins list`

Default output:

```
NAME                  CONTRACT              VERSION-RANGE      SOURCE
cairn-mcp             MCPServer             [0.1.0, 0.2.0)     bundled:cairn-mcp
cairn-sensors-local   SensorIngress         [0.1.0, 0.2.0)     bundled:cairn-sensors-local
cairn-store-sqlite    MemoryStore           [0.1.0, 0.2.0)     bundled:cairn-store-sqlite
cairn-workflows       WorkflowOrchestrator  [0.1.0, 0.2.0)     bundled:cairn-workflows
```

`--json`:

```json
{
  "plugins": [
    {
      "name": "cairn-mcp",
      "contract": "MCPServer",
      "contract_version_range": {
        "min": "0.1.0",
        "max_exclusive": "0.2.0"
      },
      "source": "bundled:cairn-mcp",
      "capabilities": { "stdio": true, "http": false }
    }
  ]
}
```

`source` is `bundled:<crate-name>` for every plugin in P0. The shape is
forward-compatible with `config:.cairn/config.yaml` once the config
loader lands.

Capabilities are hidden from the human table to keep it narrow but
present in `--json`. A future `cairn plugins describe <name>` command
can surface them in human form; out of scope here.

### 5.2 `cairn plugins verify`

Default output (one block per plugin):

```
cairn-store-sqlite (MemoryStore)
  tier-1 manifest_matches_host                    ok
  tier-1 arc_pointer_stable                       ok
  tier-1 capability_self_consistency_floor        ok
  tier-2 put_get_roundtrip                        pending (real impl pending)
  tier-2 fts_query_returns_doc                    pending (real impl pending)
  tier-2 vector_search_when_advertised            pending (real impl pending)

…

Summary: 4 plugins, 12 ok, 7 pending, 0 failed
```

`--json`:

```json
{
  "plugins": [
    {
      "name": "cairn-store-sqlite",
      "contract": "MemoryStore",
      "cases": [
        { "id": "manifest_matches_host", "tier": 1, "status": "ok" },
        { "id": "put_get_roundtrip", "tier": 2, "status": "pending",
          "reason": "real impl pending" }
      ]
    }
  ],
  "summary": { "ok": 12, "pending": 7, "failed": 0 }
}
```

Exit codes:

| Code | Meaning |
|---|---|
| `0`  | No tier-1 failure (default; `pending` cases allowed). |
| `64` | Clap usage error (`EX_USAGE`). |
| `69` | A tier-1 case failed or `register_all` rejected a plugin (`EX_UNAVAILABLE`). |
| `78` | A bundled `plugin.toml` failed to parse (`EX_CONFIG`). |

`--strict` flips: any `pending` case becomes a tier-2 failure → exit `69`.
This is the mode used by ad-hoc local invocations; CI uses default mode
so the build stays green until per-contract impls land.

## 6. Implementation notes

### 6.1 `register_plugin!` macro extension

The macro currently expands to
`register_plugin!(<Contract>, <Impl>, "<name>")` per
`crates/cairn-core/src/contract/macros.rs`. We extend each arm to accept
an optional fourth argument — the manifest TOML constant:

```rust
// existing form (kept for tests that don't need a manifest):
register_plugin!(MemoryStore, MyStore, "acme-store");

// new manifest-aware form (required for bundled plugins):
register_plugin!(
    MemoryStore,
    MyStore,
    "cairn-store-sqlite",
    MANIFEST_TOML,
);
```

Implementation: add a second arm to `__register_plugin_helper!` that
accepts `$manifest:expr` and emits a `register()` body which:

1. Parses `MANIFEST_TOML` via `PluginManifest::parse_toml`, surfacing
   `PluginError::InvalidManifest` on failure.
2. Calls `PluginManifest::verify_compatible_with(name, contract_kind,
   <contract>::CONTRACT_VERSION)` to fail closed before construction.
3. Calls `reg.<register_method>_with_manifest(name, manifest, Arc::new(<impl>::default()))`.

The four-arg form is used by every bundled plugin in this PR. The
three-arg form remains for unit tests in `cairn-core` that don't need
to exercise the manifest path.

### 6.2 Stub trait implementations

Every bundled crate ships a stub struct implementing its contract trait.
Required methods (`name`, `capabilities`, `supported_contract_versions`)
return real values. Verb-shaped methods that exist on the trait return
`Err(PluginError::CapabilityUnavailable { … })` or the per-trait
equivalent. The stubs exist solely so `register_all()` succeeds and
tier-1 cases pass; per-impl issues replace them with real logic.

Capability flag values for stubs:

- `MemoryStore`: `fts=false, vector=false, graph_edges=false, transactions=false`.
- `WorkflowOrchestrator`: empty workflow set.
- `SensorIngress`: empty sensor set.
- `MCPServer`: `stdio=false, http=false` *(real flags flip to true in
  the MCP impl issue)*.

These are honest defaults — the stubs do not advertise capability they
do not have.

### 6.3 Tests

Unit (in-crate):

- `cairn-cli/src/plugins/host.rs`: `register_all` happy path; failure
  path synthesised by injecting a `BadPlugin` registering before the
  bundled set (test-only feature `test-utils` exposing a helper).
- `cairn-cli/src/plugins/list.rs`: pure formatter unit tests.
- `cairn-cli/src/plugins/verify.rs`: pure exit-code mapping tests.

Integration (`crates/cairn-cli/tests/`):

- `plugins_list_snapshot.rs` — `insta` snapshot of human + JSON output.
- `plugins_verify_snapshot.rs` — `insta` snapshot of human + JSON
  output; asserts default mode exits 0, `--strict` exits 69.
- `plugins_verify.rs` — shells out to the built `cairn` binary,
  parses JSON, asserts `summary.failed == 0`, `summary.ok >= 12` (4
  plugins × 3 tier-1 cases). This is the CI-protective wrapper that
  runs under `cargo nextest` regardless of workflow changes.

Per-bundled-crate (`crates/<crate>/tests/`):

- `manifest_validates.rs` — load `plugin.toml`, run JSON-Schema
  validation via `cairn-idl`, run Rust-level `PluginManifest::parse_toml`,
  call `verify_compatible_with` with the host's `CONTRACT_VERSION`.

### 6.4 Trade-off — sibling method vs signature change

§3.4 picks the **sibling method** form
(`register_<contract>_with_manifest(...)`) over changing the existing
seven `register_<contract>(...)` signatures. Reasons:

1. PR #174 just shipped the existing signatures; changing them within
   the same release cycle churns the public API for no behavioural
   gain.
2. The macro is the bundled-plugin entry point; it emits the
   manifest-aware variant unconditionally, so end-users never call the
   bare form except in unit tests.
3. The sibling form composes: a future `register_with_manifest_and_config`
   can be added without breaking either of the existing two.

Cost: two near-identical methods per contract (14 total). Mitigation:
`_with_manifest` delegates to the bare form internally, so the
duplication is one `self.<contract>_manifests.insert(...)` line per
contract.

## 7. CI integration

### 7.1 `.github/workflows/ci.yml`

After the existing `cargo nextest run` step, add:

```yaml
- name: cairn plugins verify (default)
  run: cargo run -p cairn-cli --locked -- plugins verify
- name: cairn plugins verify (json artifact)
  run: cargo run -p cairn-cli --locked -- plugins verify --json > plugins-verify.json
- uses: actions/upload-artifact@v4
  with:
    name: plugins-verify
    path: plugins-verify.json
```

Default mode keeps CI green while tier-2 cases are still `Pending`. The
JSON artifact is consumed by future dashboards / reviewers.

### 7.2 Cargo-test wrapper

`crates/cairn-cli/tests/plugins_verify.rs` shells out to the built
binary inside a `#[test]`. This means even if a contributor edits
`ci.yml` and removes the verify step, `cargo nextest run --workspace`
still catches a regression.

## 8. Acceptance-criteria mapping

| AC | How this design satisfies it |
|---|---|
| Startup fails closed when a plugin contract version is incompatible | `host::register_all()` propagates `PluginError::UnsupportedContractVersion`; the existing registry guard fires. Synthesized failure path covered by `host.rs` unit test. |
| `cairn plugins list` reports loaded plugins, versions, capabilities, and selected config source | §5.1 — name, contract, version range, source column; capabilities in `--json`. |
| `cairn plugins verify` can run conformance tests against active bundled plugins in CI | §5.2 + §7. CI runs default mode + uploads JSON artifact; cargo-test wrapper guards against workflow drift. |

## 9. Risks and follow-ups

- **Macro complexity.** Extending `register_plugin!` to handle manifests
  inflates the macro surface. If review pushes back, fall back to the
  alternative in §6.4 (separate `register_manifest` call).
- **Adopting clap mid-scaffold.** Bringing in `clap` for one subcommand
  invites scope creep. Mitigation: only the `plugins` subtree is
  defined; verbs remain unwired and exit 2 via the existing scaffold
  path or `clap`'s default error.
- **Tier-2 surface lock-in.** Tier-2 case names defined here become an
  API contract for per-impl PRs. Renaming a case later means renaming a
  function across two crates. Mitigation: the case names map directly
  to brief §5 verb behaviour; they should not need to churn.
- **Stub plugin semantics.** Stubs ship in production binaries until
  per-impl issues land. Risk that a user runs `cairn ingest` and gets a
  `CapabilityUnavailable` error. This is desired behaviour for the
  current scaffold phase but should be documented in `--help` output.

## 10. Definition of done

- [ ] All four bundled crates ship `plugin.toml` + `register()` + stub
      trait impl. Each crate's `manifest_validates.rs` test passes.
- [ ] `PluginRegistry` exposes `parsed_manifest()`; macro emits the
      manifest-aware `register()` form.
- [ ] `cairn plugins list` and `cairn plugins verify` exist, snapshot
      tests committed, exit codes match §5.2.
- [ ] `cairn-core::contract::conformance` ships tier-1 cases (working)
      and tier-2 stubs (`Pending`).
- [ ] CI workflow runs `plugins verify` and uploads JSON artifact;
      cargo-test wrapper passes locally and in CI.
- [ ] PR description cites brief §4.0, §4.1, §13.3 + this spec.
