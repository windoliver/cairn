# MCP Conformance + Capability-Rejection Test Suite — Design

- **Issue:** [#67](https://github.com/windoliver/cairn/issues/67) — `[P0] Add MCP conformance and capability-rejection tests`
- **Parent epic:** #10 `[P0] Implement MCP adapter and capability negotiation`
- **Depends on:** #66 (closed) — `[P0] Map MCP requests to typed verb envelopes and responses`
- **Brief sections:** §4.1 Conformance is tested, §8 Contract surfaces, §8.0.a Handshake / status / capability advertisement, §8.0.b Envelope
- **Date:** 2026-05-11

---

## 1. Problem

The MCP adapter (`cairn-mcp`) has 13 test files covering individual verb behaviors,
`initialize` / `status` parity, handshake minting, IDL/codegen surface, and a smoke
suite for the JSON-RPC stdio transport. Coverage is real but **scattered**: there
is no single matrix that walks every P0 verb across `{valid, invalid, capability-disabled}`
and asserts the resulting envelope against a stored canonical artifact.

The wire-fixture library is tiny — four JSON files under `fixtures/v0/envelopes/`.
There is no per-verb canonical baseline that future harness integrations (Claude
Code, Codex, future skill runtime) or future contract versions (`cairn.mcp.v2`)
can diff against.

The brief §8.0.a invariant **(b)** — *"every verb call that corresponds to an
un-advertised capability returns `CapabilityUnavailable` rather than succeeding
or falling back"* — is partially exercised (semantic search rejection in
`handler_rejection.rs`, extension verbs in `init_status_parity.rs`) but is not
mechanically enforced across the full verb-mode cross-product.

Issue #67 closes these gaps with three deliverables:

1. A conformance suite that exercises every P0 MCP tool with valid + invalid envelopes.
2. Tests for disabled capabilities, unsupported extension namespaces, invalid
   search modes, and unsupported forget modes.
3. Protocol fixtures recorded for future semver compatibility checks.

## 2. Goals

- Replay runner + targeted gap fills (both buckets, not one or the other).
- Envelope-canonical fixtures portable across all four surfaces (CLI, MCP, SDK,
  skill) — JSON-RPC framing reconstructed in the runner, not stored as bytes.
- One named fixture per cap-gated path enumerated in §8.0 (named cases →
  readable failures), plus one IDL-driven cross-product loop that backstops the
  brief §8.0.a (b) invariant across every routable verb-mode the dispatcher
  knows about.
- All conformance tests run under the existing
  `cargo nextest run --workspace --locked` step — no new CI workflow.

### Non-goals

- Side-effect verification (e.g., that `ingest` persisted a row). Covered by
  store integration tests in `cairn-store-sqlite`.
- Performance / timing assertions.
- Concurrent dispatch stress. Covered by existing `smoke.rs` /
  `relay_integration.rs`.
- A `cairn mcp verify` user-facing subcommand. Out of scope; tracked as a
  follow-up.
- SDK conformance harness (the `B` option from brainstorming). Defer until a
  second consumer exists.

## 3. Architecture

```
fixtures/v0/mcp/conformance/                     ← canonical artifacts (env JSON)
├── ingest/
│   ├── ok_minimal.request.json
│   ├── ok_minimal.response.json
│   └── _meta.json
├── search/
│   ├── ok_keyword.{request,response}.json
│   ├── err_invalid_mode.{request,response}.json
│   ├── err_semantic_disabled.{request,response}.json
│   └── _meta.json
├── forget/
│   ├── ok_record.{...}
│   ├── err_mode_session_unsupported.{...}
│   ├── err_mode_scope_unsupported.{...}
│   └── _meta.json
├── retrieve/, summarize/, lint/, assemble_hot/, capture_trace/, status/, handshake/
├── _envelope/                                   ← cross-verb cases
│   ├── err_unknown_verb.{request,response}.json
│   └── err_malformed_args.{request,response}.json
└── _extension/
    └── err_aggregate_unadvertised.{request,response}.json

crates/cairn-test-fixtures/src/mcp/             ← new module (dev-only)
└── conformance.rs                              ← include_dir! loader

crates/cairn-mcp/tests/mcp_conformance.rs       ← single test binary
├── conformance_envelope_replay                 ← parameterized over load_all()
├── conformance_jsonrpc_layer                   ← re-frames Ok cases via stdio
├── unadvertised_capability_rejects_for_every_routable_mode
└── mod runner_self_tests                       ← six guard tests
```

Three components, one purpose each:

- **Fixture library** — pure data. One `(request, response)` envelope pair per
  case + a `_meta.json` registry per verb directory naming the case kind and
  config overrides. Versioned under `fixtures/v0/` so v0.2 lands beside it
  without breaking the baseline.
- **Loader** (`cairn-test-fixtures::mcp::conformance`) — `include_dir!`-embedded
  at compile time. Exposes `load_all() -> Vec<ConformanceCase>` and
  `load_case(id) -> ConformanceCase`. Dev-only; never a non-dev dep
  (CLAUDE.md §3).
- **Runner** (`crates/cairn-mcp/tests/mcp_conformance.rs`) — instantiates a
  handler with the case's config overrides, dispatches the request envelope,
  canonicalizes the response, diffs against the stored `.response.json`. JSON-RPC
  framing reconstructed inline from the envelope for `conformance_jsonrpc_layer`.

## 4. Components

### 4.1 `ConformanceCase` shape

```rust
// crates/cairn-test-fixtures/src/mcp/conformance.rs

pub struct ConformanceCase {
    pub id: &'static str,            // "search/err_semantic_disabled"
    pub verb: &'static str,          // "search"
    pub kind: CaseKind,
    pub config: ConfigOverrides,
    pub request: serde_json::Value,  // canonical envelope, brief §8.0.b
    pub response: serde_json::Value, // expected envelope, brief §8.0.b
}

pub enum CaseKind {
    Ok,
    InvalidArgs,
    CapabilityRejected,
    ExtensionRejected,
}

pub struct ConfigOverrides {
    pub semantic_enabled: bool,
    pub hybrid_enabled: bool,
    pub forget_session_enabled: bool,        // v0.2+ — false at P0
    pub forget_scope_enabled: bool,          // v0.3+ — false at P0
    pub aggregate_extension_enabled: bool,
    pub admin_extension_enabled: bool,
    // mirror what cairn-core::status::advertise reads
}
```

`ConfigOverrides` fields are typed booleans, not strings. Any drift between
this struct and `cairn-core::status::advertise`'s gate set is caught by the
`config_overrides_match_advertised_capabilities` self-test (§5.5).

### 4.2 Loader

- `pub fn load_all() -> Vec<ConformanceCase>` — embeds
  `fixtures/v0/mcp/conformance/` via `include_dir!`, parses each subdirectory's
  `_meta.json`, pairs `*.request.json` with `*.response.json`. Asserts pairing
  at load time so a missing response is a panic, not a silent skip.
- `pub fn load_case(id: &str) -> ConformanceCase` — single-case accessor for
  targeted debug.
- Returns owned `serde_json::Value`. The runner controls canonicalization;
  the loader does not silently mutate.

### 4.3 Runner — three test functions

```rust
// crates/cairn-mcp/tests/mcp_conformance.rs

#[rstest]
fn conformance_envelope_replay(
    #[values(load_all())] case: ConformanceCase,
) {
    let handler = build_handler_for(&case.config);
    let actual = dispatch_envelope(&handler, &case.request);
    pretty_assertions::assert_eq!(
        canonicalize(&actual),
        canonicalize(&case.response),
        "case {}: envelope mismatch (rerun with CAIRN_BLESS=1 to update)",
        case.id,
    );
}

#[tokio::test]
async fn conformance_jsonrpc_layer() {
    // For each Ok case: re-frame envelope as JSON-RPC tools/call, send through
    // the real stdio transport (using send_frame / recv_frame helpers already
    // present in smoke.rs / init_status_parity.rs), assert outer envelope
    // identical to the direct dispatch path.
}

#[tokio::test]
async fn unadvertised_capability_rejects_for_every_routable_mode() {
    let handler  = build_handler_for(&ConfigOverrides::default_p0());
    let advertised = cairn_core::status::advertise(&handler.gates());
    let all_modes  = idl::all_routable_verb_modes();
    for (verb, mode) in all_modes {
        let cap = capability_id_for(verb, mode);
        if advertised.contains(&cap) { continue; }
        if !dispatcher_routes(verb, mode) { continue; }
        let req  = minimal_envelope(verb, mode);
        let resp = dispatch_envelope(&handler, &req);
        assert_capability_unavailable(&resp, cap, verb, mode);
    }
}
```

`dispatcher_routes` is the "in IDL but not yet wired to a handler" filter — some
verb-modes (`forget.session`, `forget.scope`) aren't routable through the v0.1
handler at all; for those the rejection comes from the parse layer, not the
capability check. The cross-product is explicitly the §8.0.a (b) invariant.

### 4.4 Canonicalization

One pure function: `canonicalize(v: &Value) -> Value`. Sorts object keys
recursively, replaces these non-deterministic fields with stable placeholders
before diff:

| Field | Replacement |
|---|---|
| `operation_id` | `"<OPERATION_ID>"` |
| `policy_trace[*].timestamp` (if present) | `"<TIMESTAMP>"` |
| `data.server_info.started_at` | `"<STARTED_AT>"` |
| `data.server_info.incarnation` | `"<INCARNATION>"` |
| `data.server_info.build` | `"<BUILD>"` |
| `data.challenge.nonce` (handshake only) | `"<NONCE>"` |
| `data.challenge.expires_at` (handshake only) | `"<EXPIRES_AT>"` |

Everything else is a hard diff. The canonicalization is applied to both
expected and actual so the stored `.response.json` is already in canonical form
on disk — humans read it, machines diff it, no hidden hashing layer.

### 4.5 Handler construction

`build_handler_for(&config) -> CairnMcpHandler` lives in the test file (not the
loader — config knobs are mcp-crate-internal). Wires the same gates that
`cairn-core::status::advertise` reads. Reuses the existing `build_handler_wired()`
helper from `init_status_parity.rs` — if duplication appears across the new
conformance test and the older parity test, extract to a `tests/common/` module.

## 5. Data flow per case

```
on-disk fixture
    │  include_dir!  (compile-time)
    ▼
ConformanceCase { request, response, config, kind }
    │  build_handler_for(config)  ──  reads same gates as advertise()
    ▼
CairnMcpHandler instance
    │  dispatch_envelope(handler, request)
    │      ├── envelope_replay: calls handler dispatch directly
    │      └── jsonrpc_layer:   wraps envelope in JSON-RPC tools/call,
    │                            writes to stdio transport, reads frame back,
    │                            unwraps to envelope
    ▼
actual envelope (serde_json::Value)
    │  canonicalize(actual), canonicalize(expected)
    ▼
pretty_assertions::assert_eq!  — full diff on failure
```

## 6. Named-case manifest

### 6.1 Happy-path (`Ok`) cases — 10 fixtures

| Verb | Case id | Notes |
|---|---|---|
| `status` (prelude) | `status/ok_default` | Loaded twice in the test to assert byte-stable across calls (brief §8.0.a c). |
| `handshake` (prelude) | `handshake/ok_mint` | Nonce + expires_at canonicalized; only shape checked. |
| `ingest` | `ingest/ok_minimal` | One short observation, default visibility. |
| `search` | `search/ok_keyword` | `mode: "keyword"`, no filters. |
| `retrieve` | `retrieve/ok_record` | `target: "record"` — only target advertised at P0 today. |
| `summarize` | `summarize/ok_no_persist` | `persist: false`. |
| `assemble_hot` | `assemble_hot/ok_empty_vault` | Empty prefix returned cleanly. |
| `capture_trace` | `capture_trace/ok_minimal` | One reasoning trajectory. |
| `lint` | `lint/ok_read_only` | `write_report: false`. |
| `forget` | `forget/ok_record` | `mode: "record"`. |

### 6.2 Targeted gap fills — 10 fixtures

| Verb | Case id | Asserts |
|---|---|---|
| `search` | `search/err_invalid_mode` | `mode: "fuzzy"` → `InvalidArgs`, `error.data.field == "mode"` |
| `search` | `search/err_semantic_disabled` | Semantic off → `CapabilityUnavailable`, `error.data.capability == "cairn.mcp.v1.search.semantic"`, `error.data.remediation` present |
| `forget` | `forget/err_mode_session_unsupported` | `mode: "session"` on v0.1 → `CapabilityUnavailable`, capability id matches |
| `forget` | `forget/err_mode_scope_unsupported` | `mode: "scope"` on v0.1 → `CapabilityUnavailable` |
| `retrieve` | `retrieve/err_target_turn_unsupported` | Skipped if `cairn.mcp.v1.retrieve.turn` is currently advertised; included if not. Intent: dispatcher gating on un-advertised target. |
| `lint` | `lint/err_write_no_capability` | `write_report: true` without write capability → `CapabilityUnavailable` (exact code TBD on handler audit; spec says it should be a cap rejection, not a generic auth error) |
| `summarize` | `summarize/err_persist_no_capability` | `persist: true` without write capability → same as lint |
| `_envelope` | `_envelope/err_unknown_verb` | Unknown verb name → MCP `tools/call` error frame with stable code |
| `_envelope` | `_envelope/err_malformed_args` | Missing required field for a known verb → `InvalidArgs` |
| `_extension` | `_extension/err_aggregate_unadvertised` | `agent_summary` when `cairn.aggregate.v1` not enabled → extension-rejected |

Total: 20 fixture pairs (≈40 files) + 12 `_meta.json` registry files (one per
verb-group directory: `status`, `handshake`, `ingest`, `search`, `retrieve`,
`summarize`, `assemble_hot`, `capture_trace`, `lint`, `forget`, `_envelope`,
`_extension`).

### 6.3 `_meta.json` shape

```jsonc
// fixtures/v0/mcp/conformance/search/_meta.json
{
  "cases": {
    "ok_keyword": {
      "kind": "ok",
      "config": { "semantic_enabled": true, "hybrid_enabled": true }
    },
    "err_invalid_mode": {
      "kind": "invalid_args",
      "config": { "semantic_enabled": true, "hybrid_enabled": true }
    },
    "err_semantic_disabled": {
      "kind": "capability_rejected",
      "config": { "semantic_enabled": false, "hybrid_enabled": false }
    }
  }
}
```

Loader asserts every `case_id` named in `_meta.json` has a matching
`.request.json` + `.response.json`, and every on-disk pair is named in
`_meta.json`. Orphans are panics, not warnings.

### 6.4 Authoring rule

Every fixture file is canonical on disk (sorted keys, volatile fields replaced
with the placeholders from §4.4). The `fixtures_on_disk_are_canonical` self-test
(§7.2) panics with a `CAIRN_BLESS=1` hint when a contributor checks in a
non-canonical fixture.

## 7. Self-tests for the runner

Six guard tests under `mod runner_self_tests` in the same test file. They
protect against a buggy runner that returns green on everything.

### 7.1 Canonicalization is total and idempotent

```rust
#[test] fn canonicalize_sorts_keys_recursively() { ... }
#[test] fn canonicalize_replaces_every_volatile_field() { ... }
#[test] fn canonicalize_is_idempotent() {
    for case in load_all() {
        let a = canonicalize(&case.response);
        let b = canonicalize(&a);
        assert_eq!(a, b);
    }
}
```

### 7.2 Fixtures on disk are canonical

```rust
#[test] fn fixtures_on_disk_are_canonical() {
    for case in load_all() {
        let raw = case.response.clone();
        assert_eq!(
            raw, canonicalize(&raw),
            "fixture {} is not canonical; run CAIRN_BLESS=1 cargo nextest …",
            case.id,
        );
    }
}
```

### 7.3 Negative meta-test — runner can fail

```rust
#[test] fn runner_actually_diffs() {
    let mut case = load_case("search/ok_keyword");
    case.response["data"]["hits"][0]["score"] = serde_json::json!(0.0);
    let actual = dispatch_envelope(&build_handler_for(&case.config), &case.request);
    let result = std::panic::catch_unwind(|| {
        pretty_assertions::assert_eq!(
            canonicalize(&actual),
            canonicalize(&case.response),
        );
    });
    assert!(result.is_err(), "runner failed to detect a forced mismatch");
}
```

This is the only place `catch_unwind` is used. Proves the assertion path is
live; a refactor that accidentally short-circuits `assert_eq!` ships red.

### 7.4 `_meta.json` is well-formed and complete

```rust
#[test] fn meta_registry_covers_every_fixture_directory() {
    // Every subdir's _meta.json names a kind + config for every case_id on disk.
    // No orphans, no phantom entries.
}
```

### 7.5 `ConfigOverrides` matches `cairn-core::status::advertise`

```rust
#[test] fn config_overrides_match_advertised_capabilities() {
    // For each case: apply config to a fresh advertise() call, assert the
    // resulting capability set is exactly what the test handler will gate on.
}
```

### 7.6 Cross-product backstop is non-empty

```rust
#[test] fn cross_product_backstop_is_non_empty() {
    let pairs = unadvertised_routable_modes();
    assert!(
        !pairs.is_empty(),
        "every verb-mode is advertised — backstop is testing nothing",
    );
}
```

Future-proofs against the day someone enables every capability by default.
The cross-product test would silently become a no-op; this guard makes that
loud.

## 8. Error handling and failure reporting

| Class | Meaning | Output |
|---|---|---|
| Envelope mismatch | Handler returned a structurally-different envelope than `.response.json` | Full pretty-printed JSON diff (`pretty_assertions::assert_eq!`), case id, paths to both files, `CAIRN_BLESS=1` re-bless hint |
| Wrong rejection code | Got rejection but `error.code` / `error.data.capability` doesn't match expected | Compact diff scoped to the `error.*` subtree |
| Loader / canonicalization fault | Fixture missing pair, malformed JSON, non-canonical on disk | Panic at load time with the offending file path |

**No silent skips.** A `*.request.json` without a paired `*.response.json` is a
panic. A `_meta.json` entry referencing a missing case is a panic. A capability
name in `ConfigOverrides` that doesn't match a known gate is a compile error
(typed booleans, not strings).

### 8.1 Re-bless workflow

```bash
CAIRN_BLESS=1 cargo nextest run -p cairn-mcp --test mcp_conformance
```

The runner sees `CAIRN_BLESS=1` and writes the *canonicalized actual* back to
`.response.json`. Never touches `.request.json` — inputs are intentional.
Discovered cases (cross-product backstop) ignore the env var since they have
no on-disk artifact.

### 8.2 Cross-product backstop reporting

When the generated loop fails, each iteration's `assert!` carries the verb and
mode in its message. Single-test grouping for now; if the cross-product grows
past ~20 pairs, switch to `#[rstest::rstest]` so each pair gets its own nextest
entry.

## 9. CI integration

Folded into the existing `cargo nextest run --workspace --locked` step. No new
workflow. Failure surfaces with the test name `mcp_conformance::*`.

The supply-chain workflow already runs `cargo deny check` / `cargo audit` /
`cargo machete`; the new `include_dir` and `pretty_assertions` deps (if not
already in the workspace) will be vetted there.

## 10. Invariants

Brief §2 invariants touched:

- **#6 Fail closed on capability** — partially strengthened. A weak
  cross-product assertion (`unadvertised_capability_does_not_succeed`) runs in
  CI and verifies no un-advertised, dispatch-routable verb-mode returns
  `status == "committed"`. The strict form (`unadvertised_capability_rejects_strict_form`,
  `#[ignore]`'d) will additionally assert `error.code == "CapabilityUnavailable"`
  and `error.data.capability` once handler wiring lands.
- **#3 CLI is ground truth** — unchanged. Conformance asserts MCP matches the
  envelope contract; CLI parity tests in other crates continue to be the
  source of truth for verb semantics.
- **#4 Seven contracts** — unchanged. No new core API, no new IDL, no new trait.

Brief §8.0.a (b) — *every un-advertised cap MUST reject* — is partially tested:
a weak assertion (response status != committed) runs in CI; the strict form
(error.code == CapabilityUnavailable) is `#[ignore]`'d pending handler wiring.

## 11. Deliverables for the PR

1. `fixtures/v0/mcp/conformance/` — 20 fixture pairs + 12 `_meta.json` files.
2. `crates/cairn-test-fixtures/src/mcp/conformance.rs` — new module, ≈120 LOC.
   Add `include_dir` to the crate's dev-only dep surface (the helper is dev-only).
3. `crates/cairn-mcp/tests/mcp_conformance.rs` — runner + cross-product + six
   self-tests, ≈500 LOC.
4. `crates/cairn-test-fixtures/Cargo.toml` — add `include_dir` workspace dep
   (already has `serde_json`).
5. `Cargo.toml` (workspace) — add `include_dir` + `pretty_assertions` to
   `[workspace.dependencies]` if not present.
6. This design doc, committed at the same path.

Verification per CLAUDE.md §8:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run -p cairn-mcp --test mcp_conformance --locked
cargo nextest run --workspace --locked
./scripts/check-core-boundary.sh
cargo deny check
cargo machete
```

## 12. Risks and open questions

- **`lint`/`summarize` write-capability rejection wording.** The exact `error.code`
  for "tried to use `write_report: true` without write capability" needs a quick
  handler audit before fixtures are blessed — current handler may emit
  `Unauthorized` rather than `CapabilityUnavailable`. Plan: audit during
  implementation, adjust fixture or open a follow-up for handler semantics if
  the existing behavior diverges from brief §8.0.b expectations.
- **`include_dir` macro and cargo target tracking.** Confirm in implementation
  that `cargo nextest` re-runs the test crate when a fixture file changes.
  `include_dir!` does emit `cargo:rerun-if-changed` for the embedded tree, so
  this should work; verify on the first PR cycle.
- **JSON-RPC re-framing in `conformance_jsonrpc_layer`.** Requires the existing
  `send_frame`/`recv_frame` helpers from `smoke.rs` to be shareable. Extract to
  `tests/common/mod.rs` if duplication appears across `smoke.rs`,
  `init_status_parity.rs`, `handler_rejection.rs`, and the new test.

## 13. Out of scope (explicit)

- Consumer-specific Claude Code acceptance tests (issue #67 explicitly names
  these as out of scope).
- SDK-level conformance harness — defer until a second consumer exists.
- `cairn mcp verify` user-facing subcommand — separate design.
- Side-effect verification (covered by store tests).
- Performance benchmarks.
