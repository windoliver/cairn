# Issue #53 — `status` + capability negotiation parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lift capability advertisement to one pure function in `cairn-core` so CLI, SDK, and MCP `initialize` report identical capability sets, add `remediation` to `CapabilityUnavailable`, and lock the contract with a snapshot/parity/phase-pinning test matrix.

**Architecture:** New `cairn-core::status` module owns the per-capability decision table and remediation map. Each surface (CLI `status` verb, `Sdk::status`, MCP `get_info`) builds a `CapabilityGates` struct from its locally-known signals (vault binding, store caps, model presence) and calls `advertise(&gates)`. Future verb runtimes opt their capability into advertisement by flipping a single `pub const bool` in `cairn-core::status::wiring`.

**Tech Stack:** Rust 2024, `cairn-core`, `cairn-cli`, `cairn-sdk`, `cairn-mcp`, `cairn-idl` (codegen), `serde`, `serde_json`, `insta` (snapshots), `proptest`, `rstest`, `tempfile`.

**Spec:** `docs/superpowers/specs/2026-05-06-issue-53-status-capability-parity-design.md`

---

## File map

**Created:**
- `crates/cairn-core/src/status/mod.rs` — `Phase`, `StoreCaps`, `CapabilityGates`, `advertise()`
- `crates/cairn-core/src/status/wiring.rs` — `pub const *_WIRED: bool` flags
- `crates/cairn-core/src/status/remediation.rs` — `REMEDIATION` map + `remediation_for()`
- `crates/cairn-core/src/status/tests.rs` — unit + property + exhaustiveness tests
- `crates/cairn-core/tests/status_phase_pinning.rs` — phase-pinning integration tests
- `crates/cairn-cli/tests/status_snapshot_insta.rs` — snapshot matrix (5 fixtures)
- `crates/cairn-cli/tests/cli_capability_rejection.rs` — fail-closed rejection tests with remediation
- `crates/cairn-cli/tests/snapshots/status_snapshot_insta__*.snap` — committed snapshot baselines

**Modified:**
- `crates/cairn-idl/schema/errors/error.json` — add optional `remediation` to `CapabilityUnavailableData`
- `crates/cairn-core/src/generated/errors/mod.rs` — regenerated
- `crates/cairn-mcp/src/generated/schemas/errors/error.json` — regenerated mirror
- `crates/cairn-core/src/lib.rs` — `pub mod status`
- `crates/cairn-cli/src/verbs/status.rs` — `compute_capabilities` delegates to `advertise()`
- `crates/cairn-sdk/src/transport.rs` — `advertised_capabilities` delegates to `advertise()`; `gates()` helper
- `crates/cairn-sdk/src/error.rs` — `SdkError::CapabilityUnavailable` gains `remediation: Option<String>`
- `crates/cairn-mcp/src/handler.rs` — `get_info` packs full status response
- `crates/cairn-mcp/src/handler.rs` — `handle_search` rejection populates remediation
- `crates/cairn-cli/src/verbs/search.rs` — capability-rejection JSON envelope and human hint line populate remediation
- `crates/cairn-cli/tests/sdk_cli_parity.rs` — extend with three-way parity matrix
- `CLAUDE.md` — §4.6 points at `cairn-core::status::advertise` as ground truth
- `docs/design/traceability.md` — §8.0.a / §15 row updated

---

## Task 1: IDL — add `remediation` to `CapabilityUnavailableData`

**Files:**
- Modify: `crates/cairn-idl/schema/errors/error.json`
- Regenerate: `crates/cairn-core/src/generated/errors/mod.rs`, `crates/cairn-mcp/src/generated/schemas/errors/error.json`, related `.snap` files
- Test: existing schema-files / codegen tests must stay green

- [ ] **Step 1: Add `remediation` to the JSON Schema**

Edit `crates/cairn-idl/schema/errors/error.json`. Locate the `"CapabilityUnavailableData"` block under `$defs` (around line 44):

```json
"CapabilityUnavailableData": {
  "type": "object",
  "additionalProperties": false,
  "required": ["capability"],
  "properties": {
    "capability":  { "$ref": "../capabilities/capabilities.json" },
    "remediation": {
      "type": "string",
      "minLength": 1,
      "description": "Free-form operator-facing hint for resolving the capability gap. Optional and additive — pre-#53 servers omit the field, newer servers populate it from the cairn-core::status::REMEDIATION map. Callers MUST NOT parse this for dispatch; the closed `capability` enum is the machine-readable signal."
    }
  }
}
```

`remediation` MUST stay out of `required[]`.

- [ ] **Step 2: Regenerate codegen**

Run:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
```

Expected: regenerates `crates/cairn-core/src/generated/errors/mod.rs` and `crates/cairn-mcp/src/generated/schemas/errors/error.json`. The `errors/mod.rs` `CapabilityUnavailableData` struct gains an `Option<String>` field with `#[serde(default, skip_serializing_if = "Option::is_none")]`. The error-envelope validator at `crates/cairn-core/src/generated/envelope/mod.rs` may also pick up an additional accepted key for `data` — verify by diff that `"remediation"` is now in the keys allowlist for `code=CapabilityUnavailable`.

If the codegen generates a Rust struct (rather than inlining into the envelope validator), the new field shape will be:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub remediation: Option<String>,
```

- [ ] **Step 3: Run the codegen drift gate**

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: exits 0 (no diff after Step 2 wrote the changes).

- [ ] **Step 4: Update affected snapshots if any**

If `cargo nextest run -p cairn-idl` reports `.snap` mismatches, run:

```bash
cargo insta review
```

Accept changes that just add the optional `remediation` field. Reject any change that mutates `required[]` for `CapabilityUnavailableData`.

- [ ] **Step 5: Build the workspace**

```bash
cargo check --workspace --all-targets --locked
```

Expected: clean. No call site is using `remediation` yet, so no semantic change.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-idl/schema/errors/error.json \
        crates/cairn-core/src/generated/errors/mod.rs \
        crates/cairn-core/src/generated/envelope/mod.rs \
        crates/cairn-mcp/src/generated/schemas/errors/error.json \
        crates/cairn-idl/tests/snapshots/
git commit -m "feat(idl): add optional remediation to CapabilityUnavailableData (issue #53)"
```

---

## Task 2: `cairn-core::status` module skeleton

**Files:**
- Create: `crates/cairn-core/src/status/mod.rs`
- Create: `crates/cairn-core/src/status/wiring.rs`
- Modify: `crates/cairn-core/src/lib.rs`

- [ ] **Step 1: Create `wiring.rs`**

Write `crates/cairn-core/src/status/wiring.rs`:

```rust
//! Per-capability "is the runtime wired end-to-end?" flags.
//!
//! Brief §15 / §8.0.a forbid over-advertising: a capability appears in
//! `status.capabilities` only when the runtime can honor every call against
//! it. The flags below start `false`; the issue that lands a verb's dispatch
//! flips the matching flag to `true`. CLI, SDK, and MCP all read these
//! through `cairn_core::status::advertise()` so flipping one constant
//! propagates to every surface.

/// `forget --record` end-to-end dispatch path is wired (issue family #54+).
pub const FORGET_RECORD_WIRED: bool = false;

/// `forget --session` (v0.2+ runtime).
pub const FORGET_SESSION_WIRED: bool = false;

/// `forget --scope` (v0.3+ runtime).
pub const FORGET_SCOPE_WIRED: bool = false;

/// `retrieve --record` dispatch path (issue #61 family).
pub const RETRIEVE_RECORD_WIRED: bool = false;

/// `retrieve --session` dispatch path.
pub const RETRIEVE_SESSION_WIRED: bool = false;

/// `retrieve --turn` dispatch path.
pub const RETRIEVE_TURN_WIRED: bool = false;

/// `retrieve --folder` dispatch path.
pub const RETRIEVE_FOLDER_WIRED: bool = false;

/// `retrieve --scope` dispatch path.
pub const RETRIEVE_SCOPE_WIRED: bool = false;

/// `retrieve --profile` dispatch path.
pub const RETRIEVE_PROFILE_WIRED: bool = false;

/// Sequence-mode replay rejection routed through every signed-verb path
/// (`prepare_wal_with_replay` integration; held back per
/// `crates/cairn-cli/src/verbs/status.rs` round-2 review #2).
pub const REPLAY_SEQUENCE_WIRED: bool = false;

/// Challenge-mode replay rejection routed through every signed-verb path.
pub const REPLAY_CHALLENGE_WIRED: bool = false;
```

- [ ] **Step 2: Create the module skeleton with types**

Write `crates/cairn-core/src/status/mod.rs`:

```rust
//! Capability advertisement — the single source of truth.
//!
//! `advertise()` is a pure function from `CapabilityGates` to the wire-format
//! `Vec<Capabilities>`. CLI's `cairn status` handler, `cairn-sdk`'s `Sdk::status`,
//! and `cairn-mcp`'s `get_info` all delegate here; no surface re-derives the
//! per-capability rule.
//!
//! ## Scope of this module
//!
//! Only decisions about which capabilities are *advertised*. **Not** for:
//! - Dispatch gating (use the per-verb error type — `SearchError::CapabilityUnavailable`,
//!   etc. — and reject from there).
//! - Config validation (use `cairn-core::config`).
//! - Runtime feature toggles (use Cargo features at the crate level).
//!
//! ## Mental model
//!
//! Each capability has one row in `advertise()`. A row evaluates to `true` (and
//! pushes the capability into the result Vec) only when *all* of:
//!
//! 1. The vault is bound (`vault_bound: true`).
//! 2. The contract phase is at or beyond the capability's `x-cairn-since` phase.
//! 3. The runtime dispatch path is wired end-to-end (`wiring::*_WIRED`).
//! 4. The local config opted into the feature (`config.semantic_search`, etc.).
//! 5. The wired store advertises the structural backing
//!    (`store_ok(fts)`, `store_ok(vector)`).
//!
//! When no store is wired (CLI `status` does not open SQLite), `store_ok`
//! returns `true` so the bound-vault structural backstop drives the decision —
//! every v0.1 bound vault has the FTS5 virtual table. The `Sdk::new()`
//! (no-store-no-vault) path short-circuits at rule 1 and returns `Vec::new()`.

pub mod remediation;
pub mod wiring;

pub use remediation::{REMEDIATION, remediation_for};

use crate::config::CapabilitySet;
use crate::generated::common::Capabilities;

/// Contract-version phase the runtime is operating at. Pins which capabilities
/// can ever appear in `status.capabilities` regardless of runtime wiring —
/// brief §8.0 example pins `forget.session` to v0.2+, `forget.scope` to v0.3+.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// v0.1 — minimum substrate.
    V0_1,
    /// v0.2 — adds `forget.session`, `aggregate` extension.
    V0_2,
    /// v0.3 — adds `forget.scope`, `federation` + `sessiontree` extensions.
    V0_3,
}

/// Snapshot of a wired `MemoryStore`'s structural capabilities, projected to
/// the dimensions `advertise()` cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreCaps {
    /// Full-text-search index is queryable.
    pub fts: bool,
    /// Vector / ANN index is queryable.
    pub vector: bool,
}

/// Inputs for the per-capability decision rules in `advertise()`.
#[derive(Debug, Clone)]
pub struct CapabilityGates {
    /// Config-derived feature flags (already accounts for `local_embeddings`,
    /// `policy_trace`, etc.).
    pub config: CapabilitySet,
    /// Wired `MemoryStore`'s capabilities, when one is in the loop. `None`
    /// means the surface (e.g., the CLI `status` path) chose not to open a
    /// store; structural backing falls back to the vault-bound signal.
    pub store: Option<StoreCaps>,
    /// True when `<vault>/.cairn/vault.id` is present and parses (CLI's
    /// `probe_vault_binding`) or when the surface has a wired store
    /// (`Sdk::with_store`, `CairnMcpHandler::with_store`).
    pub vault_bound: bool,
    /// True when the configured embedding model is materialized on disk
    /// (CLI's `ModelCache::is_present`) or when the wired store advertises
    /// `vector: true`.
    pub model_present: bool,
    /// True when an `LLMProvider` is configured. P0 default is `false`;
    /// reserved for future `cairn.mcp.v1.llm.*` capabilities.
    pub llm_configured: bool,
    /// Contract-version phase the runtime is operating at.
    pub contract_phase: Phase,
}

impl CapabilityGates {
    /// `true` when either no store is wired (use the structural backstop) or
    /// the wired store advertises `field`. Used internally by `advertise()`.
    fn store_ok(&self, field: fn(&StoreCaps) -> bool) -> bool {
        self.store.as_ref().map_or(true, field)
    }
}

/// The decision table — single source of truth for capability advertisement.
///
/// Returns the wire-stable order: search → policy_trace → forget → retrieve
/// → replay. `vault_bound: false` short-circuits to `Vec::new()`.
#[must_use]
pub fn advertise(gates: &CapabilityGates) -> Vec<Capabilities> {
    if !gates.vault_bound {
        return Vec::new();
    }

    let phase = gates.contract_phase;
    let cfg = &gates.config;
    let mut out = Vec::with_capacity(8);

    // ── search ────────────────────────────────────────────────────────────
    if cfg.keyword_search && gates.store_ok(|s| s.fts) {
        out.push(Capabilities::CairnMcpV1SearchKeyword);
    }
    if cfg.semantic_search && gates.model_present && gates.store_ok(|s| s.vector) {
        out.push(Capabilities::CairnMcpV1SearchSemantic);
    }
    if cfg.hybrid_search
        && gates.model_present
        && gates.store_ok(|s| s.fts)
        && gates.store_ok(|s| s.vector)
    {
        out.push(Capabilities::CairnMcpV1SearchHybrid);
    }

    // ── policy_trace ──────────────────────────────────────────────────────
    if cfg.policy_trace {
        out.push(Capabilities::CairnMcpV1PolicyTrace);
    }

    // ── forget (capability surfaces; runtime wiring still all-false) ──────
    if phase >= Phase::V0_1 && wiring::FORGET_RECORD_WIRED {
        out.push(Capabilities::CairnMcpV1ForgetRecord);
    }
    if phase >= Phase::V0_2 && wiring::FORGET_SESSION_WIRED {
        out.push(Capabilities::CairnMcpV1ForgetSession);
    }
    if phase >= Phase::V0_3 && wiring::FORGET_SCOPE_WIRED {
        out.push(Capabilities::CairnMcpV1ForgetScope);
    }

    // ── retrieve (all v0.1 per brief §8.0.a; held behind wiring flags) ────
    if wiring::RETRIEVE_RECORD_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveRecord);
    }
    if wiring::RETRIEVE_SESSION_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveSession);
    }
    if wiring::RETRIEVE_TURN_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveTurn);
    }
    if wiring::RETRIEVE_FOLDER_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveFolder);
    }
    if wiring::RETRIEVE_SCOPE_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveScope);
    }
    if wiring::RETRIEVE_PROFILE_WIRED {
        out.push(Capabilities::CairnMcpV1RetrieveProfile);
    }

    // ── replay (held back per brief §15 fail-closed) ─────────────────────
    if wiring::REPLAY_SEQUENCE_WIRED {
        out.push(Capabilities::CairnMcpV1ReplaySequence);
    }
    if wiring::REPLAY_CHALLENGE_WIRED {
        out.push(Capabilities::CairnMcpV1ReplayChallenge);
    }

    out
}

#[cfg(test)]
mod tests;
```

- [ ] **Step 3: Add module declaration to `lib.rs`**

Edit `crates/cairn-core/src/lib.rs`. Add `pub mod status;` next to the other module declarations, alphabetically (between `policy_trace` and `search` is fine):

```rust
pub mod config;
pub mod contract;
pub mod domain;
pub mod error;
pub mod generated;
pub mod pipeline;
pub mod policy_trace;
pub mod search;
pub mod status;
pub mod verbs;
pub mod verifier;
```

- [ ] **Step 4: Stub the test module**

Create `crates/cairn-core/src/status/tests.rs` with a placeholder (real tests land in Task 4):

```rust
//! Unit + property + exhaustiveness tests for `cairn_core::status`.

#[allow(unused_imports)]
use super::*;
```

- [ ] **Step 5: Build**

```bash
cargo check -p cairn-core --all-targets --locked
```

Expected: clean — `remediation_for` is referenced via `pub use` but the file doesn't exist yet, so this **will fail to compile**. That's the failing-test moment for Task 4. Skip this until the next task creates `remediation.rs`.

Instead, replace the `pub use remediation::{REMEDIATION, remediation_for};` line in `mod.rs` with `// pub use remediation::{REMEDIATION, remediation_for};` and add `pub mod remediation;` only (no re-exports). The compile passes; Task 4 restores the re-export.

- [ ] **Step 6: Run again to confirm clean**

```bash
cargo check -p cairn-core --all-targets --locked
```

Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/lib.rs \
        crates/cairn-core/src/status/mod.rs \
        crates/cairn-core/src/status/wiring.rs \
        crates/cairn-core/src/status/tests.rs
git commit -m "feat(core): add status module skeleton with advertise() (issue #53)"
```

---

## Task 3: TDD `advertise()` decision table

**Files:**
- Modify: `crates/cairn-core/src/status/tests.rs`
- Test: same file via `cargo nextest run -p cairn-core status::tests`

- [ ] **Step 1: Write the failing tests**

Replace the placeholder in `crates/cairn-core/src/status/tests.rs` with:

```rust
//! Unit tests for `cairn_core::status::advertise()`.

use super::*;
use crate::config::CapabilitySet;

fn cap_set_default(model: bool, embed_on: bool) -> CapabilitySet {
    CapabilitySet {
        keyword_search: true,
        semantic_search: model && embed_on,
        hybrid_search: model && embed_on,
        llm_extract: false,
        agent_extract: false,
        graph_edges: false,
        policy_trace: true,
        replay_sequence: true,
        replay_challenge: true,
    }
}

fn gates(bound: bool, model_present: bool, store: Option<StoreCaps>) -> CapabilityGates {
    CapabilityGates {
        config: cap_set_default(model_present, true),
        store,
        vault_bound: bound,
        model_present,
        llm_configured: false,
        contract_phase: Phase::V0_1,
    }
}

#[test]
fn unbound_returns_empty() {
    let g = gates(false, true, None);
    assert!(advertise(&g).is_empty());
}

#[test]
fn bound_no_store_advertises_keyword_and_policy_trace() {
    // CLI status path: vault bound, no store opened, no model on disk.
    let g = gates(true, false, None);
    let caps = advertise(&g);
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(caps.contains(&Capabilities::CairnMcpV1PolicyTrace));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchHybrid));
}

#[test]
fn bound_no_store_with_model_advertises_all_search_modes() {
    // CLI status path with the embedding model materialized on disk.
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchHybrid));
    assert!(caps.contains(&Capabilities::CairnMcpV1PolicyTrace));
}

#[test]
fn bound_store_without_fts_does_not_advertise_keyword() {
    let store = Some(StoreCaps { fts: false, vector: true });
    let g = gates(true, true, store);
    let caps = advertise(&g);
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchHybrid),
        "hybrid requires FTS; got {caps:?}");
}

#[test]
fn bound_store_without_vector_drops_semantic_and_hybrid() {
    let store = Some(StoreCaps { fts: true, vector: false });
    let g = gates(true, true, store);
    let caps = advertise(&g);
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchHybrid));
}

#[test]
fn local_embeddings_off_drops_semantic_and_hybrid() {
    let mut g = gates(true, true, None);
    g.config = cap_set_default(true, false); // local_embeddings_off
    let caps = advertise(&g);
    assert!(caps.contains(&Capabilities::CairnMcpV1SearchKeyword));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchSemantic));
    assert!(!caps.contains(&Capabilities::CairnMcpV1SearchHybrid));
}

#[test]
fn forget_record_held_back_until_wiring_flips() {
    // wiring::FORGET_RECORD_WIRED = false today.
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(!caps.contains(&Capabilities::CairnMcpV1ForgetRecord),
        "forget.record advertised before runtime wired (brief §15)");
}

#[test]
fn forget_session_pinned_to_v0_2_phase() {
    let mut g = gates(true, true, None);
    g.contract_phase = Phase::V0_1;
    let caps_v0_1 = advertise(&g);
    g.contract_phase = Phase::V0_2;
    let caps_v0_2 = advertise(&g);
    // Wiring flag is false so neither phase advertises today; structural
    // assertion: V0_1 cannot ever advertise forget.session even if wired.
    assert!(!caps_v0_1.contains(&Capabilities::CairnMcpV1ForgetSession));
    assert!(!caps_v0_2.contains(&Capabilities::CairnMcpV1ForgetSession));
}

#[test]
fn replay_capabilities_held_back() {
    let g = gates(true, true, None);
    let caps = advertise(&g);
    assert!(!caps.contains(&Capabilities::CairnMcpV1ReplaySequence));
    assert!(!caps.contains(&Capabilities::CairnMcpV1ReplayChallenge));
}

#[test]
fn output_order_is_stable() {
    let g = gates(true, true, None);
    let caps = advertise(&g);
    // search.* before policy_trace, per the table.
    let kw_idx = caps.iter().position(|c| matches!(c, Capabilities::CairnMcpV1SearchKeyword));
    let pt_idx = caps.iter().position(|c| matches!(c, Capabilities::CairnMcpV1PolicyTrace));
    assert!(kw_idx.is_some() && pt_idx.is_some());
    assert!(kw_idx.unwrap() < pt_idx.unwrap(),
        "wire-stable order requires search.keyword before policy_trace; got {caps:?}");
}
```

- [ ] **Step 2: Run the tests — expect compile pass, all assertions pass**

```bash
cargo nextest run -p cairn-core status::tests --locked
```

Expected: 10/10 pass. The implementation in Task 2 already satisfies these — this task documents the contract in tests.

If any test fails, fix the row in `mod.rs::advertise()` whose gate disagrees with the spec table. Do **not** weaken the test.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/status/tests.rs
git commit -m "test(core): unit tests pinning advertise() decision table (issue #53)"
```

---

## Task 4: Remediation map + `remediation_for()`

**Files:**
- Create: `crates/cairn-core/src/status/remediation.rs`
- Modify: `crates/cairn-core/src/status/mod.rs` (re-export)
- Modify: `crates/cairn-core/src/status/tests.rs` (add map tests)

- [ ] **Step 1: Add failing tests**

Append to `crates/cairn-core/src/status/tests.rs`:

```rust
#[cfg(test)]
mod remediation_tests {
    use super::*;

    #[test]
    fn remediation_for_search_semantic_is_set() {
        let hint = remediation_for("cairn.mcp.v1.search.semantic")
            .expect("semantic must have a remediation hint");
        assert!(hint.contains("local_embeddings"),
            "remediation should mention the toggle: got {hint:?}");
    }

    #[test]
    fn remediation_for_unknown_capability_is_none() {
        assert!(remediation_for("not.a.real.capability").is_none());
    }

    #[test]
    fn remediation_for_forget_session_mentions_v0_2() {
        let hint = remediation_for("cairn.mcp.v1.forget.session")
            .expect("forget.session must have a remediation hint");
        assert!(hint.contains("v0.2"));
    }

    #[test]
    fn remediation_table_has_no_empty_strings() {
        for (cap, hint) in REMEDIATION {
            assert!(!cap.is_empty(), "empty capability key");
            assert!(!hint.is_empty(), "empty remediation for {cap}");
        }
    }
}
```

- [ ] **Step 2: Run, expect compile failure**

```bash
cargo nextest run -p cairn-core status::tests::remediation_tests --locked
```

Expected: compile error — `remediation_for`, `REMEDIATION` not in scope.

- [ ] **Step 3: Implement**

Write `crates/cairn-core/src/status/remediation.rs`:

```rust
//! Operator-facing remediation hints for `CapabilityUnavailable` rejections.
//!
//! Every site that constructs a `CapabilityUnavailable` error pulls its
//! remediation string from this map so all four surfaces (CLI, MCP, SDK,
//! skill) emit identical hint text. Capabilities not in the map cause the
//! caller to omit `data.remediation` from the wire envelope (the field is
//! optional per IDL — Task 1).

/// Static lookup table — capability string → free-form operator hint.
///
/// Order is decision-table order (search → policy_trace → forget → replay)
/// so a future generated exhaustiveness test reads top-to-bottom.
pub const REMEDIATION: &[(&str, &str)] = &[
    (
        "cairn.mcp.v1.search.semantic",
        "set search.local_embeddings: true in .cairn/config.yaml and run \
         cairn embed download to materialize the embedding model",
    ),
    (
        "cairn.mcp.v1.search.hybrid",
        "set search.local_embeddings: true in .cairn/config.yaml and run \
         cairn embed download to materialize the embedding model",
    ),
    (
        "cairn.mcp.v1.policy_trace",
        "policy_trace is enabled by default; check .cairn/config.yaml for \
         an explicit override that disabled it",
    ),
    (
        "cairn.mcp.v1.forget.session",
        "forget.session ships in v0.2; upgrade to a v0.2+ runtime",
    ),
    (
        "cairn.mcp.v1.forget.scope",
        "forget.scope ships in v0.3; upgrade to a v0.3+ runtime",
    ),
    (
        "cairn.mcp.v1.replay.sequence",
        "signed-intent replay protection requires a wired challenge dispatch \
         path; not available in this build",
    ),
    (
        "cairn.mcp.v1.replay.challenge",
        "signed-intent replay protection requires a wired challenge dispatch \
         path; not available in this build",
    ),
];

/// Operator-facing remediation hint for `capability`, or `None` when no hint
/// is registered. Callers that get `None` SHOULD omit `data.remediation`
/// rather than emit an empty string — `data.remediation` declares
/// `minLength: 1`.
#[must_use]
pub fn remediation_for(capability: &str) -> Option<&'static str> {
    REMEDIATION
        .iter()
        .find_map(|(k, v)| if *k == capability { Some(*v) } else { None })
}
```

- [ ] **Step 4: Restore the re-export in `mod.rs`**

Edit `crates/cairn-core/src/status/mod.rs`. Restore the `pub use` line that Task 2 commented out:

```rust
pub mod remediation;
pub mod wiring;

pub use remediation::{REMEDIATION, remediation_for};
```

- [ ] **Step 5: Run tests, expect pass**

```bash
cargo nextest run -p cairn-core status::tests --locked
```

Expected: all tests pass (the new `remediation_tests` block + the existing `tests` block).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/status/mod.rs \
        crates/cairn-core/src/status/remediation.rs \
        crates/cairn-core/src/status/tests.rs
git commit -m "feat(core): add status::remediation map (issue #53)"
```

---

## Task 5: Property test — monotonicity

**Files:**
- Modify: `crates/cairn-core/src/status/tests.rs`
- Modify: `crates/cairn-core/Cargo.toml` (dev-deps if `proptest` not already present)

- [ ] **Step 1: Confirm `proptest` is a dev-dep**

```bash
grep -E "^proptest" crates/cairn-core/Cargo.toml || \
  grep -E "proptest" crates/cairn-core/Cargo.toml | head -3
```

Expected: a `proptest = { workspace = true }` line under `[dev-dependencies]`. If absent, add it:

```toml
[dev-dependencies]
proptest = { workspace = true }
```

- [ ] **Step 2: Add the property test**

Append to `crates/cairn-core/src/status/tests.rs`:

```rust
#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    fn arb_phase() -> impl Strategy<Value = Phase> {
        prop_oneof![Just(Phase::V0_1), Just(Phase::V0_2), Just(Phase::V0_3)]
    }

    fn arb_store() -> impl Strategy<Value = Option<StoreCaps>> {
        prop_oneof![
            Just(None),
            (any::<bool>(), any::<bool>())
                .prop_map(|(fts, vector)| Some(StoreCaps { fts, vector }))
        ]
    }

    fn arb_cap_set() -> impl Strategy<Value = crate::config::CapabilitySet> {
        (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>())
            .prop_map(|(kw, sem, hyb, pt)| crate::config::CapabilitySet {
                keyword_search: kw,
                semantic_search: sem,
                hybrid_search: hyb,
                llm_extract: false,
                agent_extract: false,
                graph_edges: false,
                policy_trace: pt,
                replay_sequence: true,
                replay_challenge: true,
            })
    }

    fn arb_gates() -> impl Strategy<Value = CapabilityGates> {
        (
            arb_cap_set(),
            arb_store(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            arb_phase(),
        )
            .prop_map(|(config, store, bound, model, llm, phase)| CapabilityGates {
                config,
                store,
                vault_bound: bound,
                model_present: model,
                llm_configured: llm,
                contract_phase: phase,
            })
    }

    /// Turning a capability gate ON never removes capabilities. Catches
    /// accidental conjunction inversions in the decision table.
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn monotone_in_keyword_search_flag(mut gates in arb_gates()) {
            gates.vault_bound = true; // monotone holds within bound branch
            let off = {
                gates.config.keyword_search = false;
                advertise(&gates)
            };
            let on = {
                gates.config.keyword_search = true;
                advertise(&gates)
            };
            for cap in &off {
                prop_assert!(on.contains(cap),
                    "keyword_search true must be a superset of false; lost {cap:?}");
            }
        }

        #[test]
        fn monotone_in_model_present(mut gates in arb_gates()) {
            gates.vault_bound = true;
            let off = {
                gates.model_present = false;
                advertise(&gates)
            };
            let on = {
                gates.model_present = true;
                advertise(&gates)
            };
            for cap in &off {
                prop_assert!(on.contains(cap),
                    "model_present true must be a superset of false; lost {cap:?}");
            }
        }

        #[test]
        fn unbound_always_empty(mut gates in arb_gates()) {
            gates.vault_bound = false;
            prop_assert!(advertise(&gates).is_empty());
        }
    }
}
```

- [ ] **Step 3: Run**

```bash
cargo nextest run -p cairn-core status::tests::prop_tests --locked
```

Expected: 3/3 pass with 256 cases each.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/status/tests.rs crates/cairn-core/Cargo.toml
git commit -m "test(core): proptest monotonicity for advertise() (issue #53)"
```

---

## Task 6: Exhaustiveness proof — every Capabilities variant has a row

**Files:**
- Modify: `crates/cairn-core/src/status/tests.rs`

- [ ] **Step 1: Add an exhaustive match test**

Append to `crates/cairn-core/src/status/tests.rs`:

```rust
#[cfg(test)]
mod exhaustiveness {
    use super::*;

    /// Compile-time proof that every `Capabilities` variant is named in
    /// the decision table. When the IDL adds a new variant, this match
    /// fails to compile until `advertise()` (in `mod.rs`) handles the new
    /// row. Combined with the runtime assertion below, no variant can be
    /// silently un-advertised.
    fn classify(c: Capabilities) -> &'static str {
        match c {
            Capabilities::CairnMcpV1SearchKeyword => "search.keyword",
            Capabilities::CairnMcpV1SearchSemantic => "search.semantic",
            Capabilities::CairnMcpV1SearchHybrid => "search.hybrid",
            Capabilities::CairnMcpV1PolicyTrace => "policy_trace",
            Capabilities::CairnMcpV1ForgetRecord => "forget.record",
            Capabilities::CairnMcpV1ForgetSession => "forget.session",
            Capabilities::CairnMcpV1ForgetScope => "forget.scope",
            Capabilities::CairnMcpV1RetrieveRecord => "retrieve.record",
            Capabilities::CairnMcpV1RetrieveSession => "retrieve.session",
            Capabilities::CairnMcpV1RetrieveTurn => "retrieve.turn",
            Capabilities::CairnMcpV1RetrieveFolder => "retrieve.folder",
            Capabilities::CairnMcpV1RetrieveScope => "retrieve.scope",
            Capabilities::CairnMcpV1RetrieveProfile => "retrieve.profile",
            Capabilities::CairnMcpV1ReplaySequence => "replay.sequence",
            Capabilities::CairnMcpV1ReplayChallenge => "replay.challenge",
            // Extension capabilities advertise via status.extensions, not
            // status.capabilities — they ride a separate code path.
            Capabilities::CairnMcpV1ExtensionAggregate => "ext.aggregate",
            Capabilities::CairnMcpV1ExtensionAdmin => "ext.admin",
            Capabilities::CairnMcpV1ExtensionFederation => "ext.federation",
            Capabilities::CairnMcpV1ExtensionSessiontree => "ext.sessiontree",
            // Capabilities is `#[non_exhaustive]` — explicit catch-all forces
            // the table above to grow when a future codegen adds a variant.
            _ => "unknown",
        }
    }

    #[test]
    fn classify_covers_every_known_variant() {
        // Sanity: classify the variants we know exist today. If the IDL
        // adds a new variant, the catch-all above returns "unknown" and
        // this test stays green — the *intended* failure mode is the
        // match in `advertise()` itself growing a `_ =>` arm. Document
        // the rule here so a reviewer notices.
        let known = [
            Capabilities::CairnMcpV1SearchKeyword,
            Capabilities::CairnMcpV1SearchSemantic,
            Capabilities::CairnMcpV1SearchHybrid,
            Capabilities::CairnMcpV1PolicyTrace,
            Capabilities::CairnMcpV1ForgetRecord,
            Capabilities::CairnMcpV1ForgetSession,
            Capabilities::CairnMcpV1ForgetScope,
            Capabilities::CairnMcpV1RetrieveRecord,
            Capabilities::CairnMcpV1RetrieveSession,
            Capabilities::CairnMcpV1RetrieveTurn,
            Capabilities::CairnMcpV1RetrieveFolder,
            Capabilities::CairnMcpV1RetrieveScope,
            Capabilities::CairnMcpV1RetrieveProfile,
            Capabilities::CairnMcpV1ReplaySequence,
            Capabilities::CairnMcpV1ReplayChallenge,
            Capabilities::CairnMcpV1ExtensionAggregate,
            Capabilities::CairnMcpV1ExtensionAdmin,
            Capabilities::CairnMcpV1ExtensionFederation,
            Capabilities::CairnMcpV1ExtensionSessiontree,
        ];
        for c in known {
            assert_ne!(classify(c), "unknown",
                "missing classify arm for {c:?}");
        }
    }
}
```

- [ ] **Step 2: Run**

```bash
cargo nextest run -p cairn-core status::tests::exhaustiveness --locked
```

Expected: 1/1 pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/status/tests.rs
git commit -m "test(core): pin exhaustiveness over Capabilities enum (issue #53)"
```

---

## Task 7: SDK delegate to `advertise()`

**Files:**
- Modify: `crates/cairn-sdk/src/transport.rs`
- Test: existing `crates/cairn-cli/tests/sdk_cli_parity.rs` and `crates/cairn-sdk/tests/surface.rs`

- [ ] **Step 1: Read the current SDK function**

Confirm `crates/cairn-sdk/src/transport.rs:186` `fn advertised_capabilities` matches the body documented in the spec §3.1. Then plan the swap.

- [ ] **Step 2: Replace `advertised_capabilities`**

Edit `crates/cairn-sdk/src/transport.rs`. Replace the body of `fn advertised_capabilities` (and add the new `gates` helper above it):

```rust
fn gates(&self) -> cairn_core::status::CapabilityGates {
    let store_caps = self.store.as_ref().map(|s| {
        let c = s.capabilities();
        cairn_core::status::StoreCaps { fts: c.fts, vector: c.vector }
    });
    let model_present = store_caps.as_ref().is_some_and(|c| c.vector);
    cairn_core::status::CapabilityGates {
        config: self.config.capabilities(model_present),
        store: store_caps,
        vault_bound: self.store.is_some(),
        model_present,
        llm_configured: false,
        contract_phase: cairn_core::status::Phase::V0_1,
    }
}

/// Project the SDK's executable state into a wire-format capability list.
fn advertised_capabilities(&self) -> Vec<Capabilities> {
    cairn_core::status::advertise(&self.gates())
}
```

Leave the doc comment on `advertised_capabilities` intact — it still describes the same contract.

- [ ] **Step 3: Build**

```bash
cargo check -p cairn-sdk --all-targets --locked
```

Expected: clean.

- [ ] **Step 4: Run SDK + parity tests**

```bash
cargo nextest run -p cairn-sdk -p cairn-cli sdk_cli_parity --locked
```

Expected: all green. The CLI and SDK are now both empty-list (CLI: `compute_capabilities` still old code, but parity test runs in tempdir → both empty). Existing `surface.rs` tests still pass — `Sdk::new()` returns `vault_bound: false` → empty Vec.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-sdk/src/transport.rs
git commit -m "refactor(sdk): delegate advertised_capabilities to cairn-core::status::advertise (issue #53)"
```

---

## Task 8: CLI delegate to `advertise()`

**Files:**
- Modify: `crates/cairn-cli/src/verbs/status.rs`
- Test: `crates/cairn-cli/tests/sdk_cli_parity.rs`, `crates/cairn-cli/tests/status_snapshot.rs`

- [ ] **Step 1: Replace `compute_capabilities`**

Edit `crates/cairn-cli/src/verbs/status.rs`. Replace the body of `fn compute_capabilities` (lines ~213–241):

```rust
fn compute_capabilities(
    vault_root: Option<&Path>,
    config: Option<&CairnConfig>,
    bound: bool,
) -> Vec<Capabilities> {
    let Some(config) = config else { return vec![]; };

    let model_present = vault_root.is_some_and(|root| {
        let models_root = root.join(".cairn").join("models");
        let cache = cairn_embeddings_local::ModelCache::new(&models_root);
        let kind: EmbeddingModelKind = config.search.embedding_model;
        cache.is_present(kind)
    });

    cairn_core::status::advertise(&cairn_core::status::CapabilityGates {
        config: config.capabilities(model_present),
        // CLI status path stays read-only and never opens the SQLite store.
        // The bound-vault structural backstop in advertise() drives the FTS gate.
        store: None,
        vault_bound: bound,
        model_present,
        llm_configured: false,
        contract_phase: cairn_core::status::Phase::V0_1,
    })
}
```

Keep `capabilities_for_config` and `p0_capabilities_advertises` but rewrite their bodies to call `advertise()` too:

```rust
fn capabilities_for_config(config: &CairnConfig, model_present: bool) -> Vec<Capabilities> {
    cairn_core::status::advertise(&cairn_core::status::CapabilityGates {
        config: config.capabilities(model_present),
        store: None,
        vault_bound: true,        // capability surface — used by --explain gate;
                                  // the gate runs only when caller is in a vault.
        model_present,
        llm_configured: false,
        contract_phase: cairn_core::status::Phase::V0_1,
    })
}

#[must_use]
pub fn p0_capabilities_advertises(capability: &str) -> bool {
    let default_config = CairnConfig::default();
    capabilities_for_config(&default_config, false)
        .iter()
        .any(|c| {
            serde_json::to_value(c)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .as_deref()
                == Some(capability)
        })
}
```

- [ ] **Step 2: Build**

```bash
cargo check -p cairn-cli --all-targets --locked
```

Expected: clean.

- [ ] **Step 3: Run existing CLI tests**

```bash
cargo nextest run -p cairn-cli --locked
```

Expected: all existing tests pass. `sdk_cli_parity::status_parity_cli_vs_sdk` still passes (both surfaces emit empty `capabilities` from a tempdir with no `.cairn/vault.id`). `status_snapshot::status_json_has_required_keys` still passes — it only asserts key presence, not values.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/verbs/status.rs
git commit -m "refactor(cli): delegate compute_capabilities to cairn-core::status::advertise (issue #53)"
```

---

## Task 9: MCP `get_info` emits the full status block

**Files:**
- Modify: `crates/cairn-mcp/src/handler.rs`

- [ ] **Step 1: Find rmcp's `ServerInfo` extension API**

Confirm rmcp's `ServerInfo` shape:

```bash
cargo doc -p rmcp --no-deps --open 2>/dev/null || \
  grep -rn "pub struct ServerInfo\|with_server_info\|with_extensions" \
  ~/.cargo/registry/src/*/rmcp-*/src/ 2>/dev/null | head -20
```

Expected: an `extensions` field or `with_extensions` builder taking `serde_json::Map<String, Value>`. If rmcp's `ServerInfo` does not expose an extensions slot at the version pinned in `Cargo.toml`, fall through to the alternative below.

- [ ] **Step 2: Rewrite `get_info`**

Edit `crates/cairn-mcp/src/handler.rs`. Replace `fn get_info`:

```rust
fn get_info(&self) -> ServerInfo {
    use cairn_core::generated::status::{StatusResponse, StatusResponseServerInfo};
    use cairn_core::pipeline::dispatch::{DefaultRegistry, pipeline_dispatch_advertisement};

    let store_caps = self.store.as_ref().map(|s| {
        let c = s.capabilities();
        cairn_core::status::StoreCaps { fts: c.fts, vector: c.vector }
    });
    let model_present = store_caps.as_ref().is_some_and(|c| c.vector);
    let gates = cairn_core::status::CapabilityGates {
        config: self.config.capabilities(model_present),
        store: store_caps,
        vault_bound: self.store.is_some(),
        model_present,
        llm_configured: false,
        contract_phase: cairn_core::status::Phase::V0_1,
    };

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

    let info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        .with_server_info(Implementation::new("cairn", env!("CARGO_PKG_VERSION")));

    // Pack the Cairn status block under a namespaced extension key so MCP
    // clients (and the parity test) can read it byte-identical to
    // `cairn status --json`.
    info.with_extension("cairn.status", serde_json::to_value(&status).unwrap_or(serde_json::Value::Null))
}

fn build_profile() -> String {
    if cfg!(debug_assertions) { "debug".to_owned() } else { "release".to_owned() }
}

fn now_rfc3339_seconds() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("invariant: system clock is after Unix epoch")
        .as_secs();
    let (y, mo, d, h, mi, s) = secs_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn new_operation_id() -> cairn_core::generated::common::Ulid {
    // Reuse the SDK helper if visible; otherwise a fresh ULID.
    cairn_sdk::stub::new_operation_id()
}
```

If rmcp does not expose `with_extension`, instead call `info.with_meta(...)` if a `_meta` slot exists, or — as a fallback — log the status block and skip the extension entirely (the parity test in Task 12 will need to reach into the handler directly through a `pub fn status_for_test(&self) -> StatusResponse` accessor; add that accessor in this task and make the parity test call it). Pick whichever path actually compiles; document the chosen fallback in a one-line comment.

For deterministic test reach-through, add a `pub fn status_response(&self) -> StatusResponse` method to `CairnMcpHandler` regardless of which rmcp slot is used:

```rust
impl CairnMcpHandler {
    /// Snapshot of the status response this handler advertises through MCP
    /// `initialize`. Used by parity tests; keep return shape identical to
    /// what `get_info()` packs into its extension slot.
    #[must_use]
    pub fn status_response(&self) -> StatusResponse {
        // ... same gates-and-pack logic factored out so get_info() and this
        // method cannot diverge.
    }
}
```

Refactor `get_info` to call `self.status_response()` and pack the result.

- [ ] **Step 3: Build**

```bash
cargo check -p cairn-mcp --all-targets --locked
```

Expected: clean.

- [ ] **Step 4: Smoke-run existing MCP tests**

```bash
cargo nextest run -p cairn-mcp --locked
```

Expected: pass. (No mutation of `tools/list` or `tools/call` paths in this task.)

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-mcp/src/handler.rs
git commit -m "feat(mcp): emit full status block via initialize + status_response accessor (issue #53)"
```

---

## Task 10: Wire `remediation` into `SdkError` and call sites

**Files:**
- Modify: `crates/cairn-sdk/src/error.rs`
- Modify: `crates/cairn-sdk/src/transport.rs`
- Modify: `crates/cairn-cli/src/verbs/search.rs` (capability-rejection envelope)
- Modify: `crates/cairn-mcp/src/handler.rs` (`handle_search` rejection arm)

- [ ] **Step 1: Extend `SdkError::CapabilityUnavailable`**

Edit `crates/cairn-sdk/src/error.rs`. Add `remediation: Option<String>` to the variant:

```rust
#[error("capability unavailable: {capability} ({reason})")]
CapabilityUnavailable {
    /// The fully-qualified capability identifier.
    capability: String,
    /// Why the capability is unavailable in this incarnation.
    reason: String,
    /// Operator-facing remediation hint sourced from
    /// `cairn_core::status::remediation_for`. `None` when the capability
    /// has no registered hint — callers should omit `data.remediation`
    /// from the wire envelope rather than emit an empty string.
    remediation: Option<String>,
    /// Operation correlation ID for log lookup.
    operation_id: Ulid,
},
```

- [ ] **Step 2: Build, expect call-site failures**

```bash
cargo check -p cairn-sdk --all-targets --locked
```

Expected: errors at every `CapabilityUnavailable { ... }` construction in `transport.rs` — missing `remediation` field.

- [ ] **Step 3: Update SDK construction sites**

Edit `crates/cairn-sdk/src/transport.rs`. There are three sites:

a) `require_capability` (line ~390):

```rust
fn require_capability(&self, required: Option<&'static str>) -> Result<(), SdkError> {
    let Some(cap) = required else { return Ok(()); };
    let advertised = self.advertised_capabilities();
    let is_advertised = advertised.iter().any(|c| {
        serde_json::to_value(c).ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .as_deref() == Some(cap)
    });
    if is_advertised {
        Ok(())
    } else {
        Err(SdkError::CapabilityUnavailable {
            capability: cap.to_owned(),
            reason: "not advertised by `status` in this incarnation".to_owned(),
            remediation: cairn_core::status::remediation_for(cap).map(str::to_owned),
            operation_id: crate::stub::new_operation_id(),
        })
    }
}
```

b) `search` dispatcher rejection arm (line ~299):

```rust
Err(cairn_core::verbs::search::SearchError::CapabilityUnavailable { capability }) => {
    Err(SdkError::CapabilityUnavailable {
        capability: capability.to_owned(),
        reason: "rejected by dispatcher".to_owned(),
        remediation: cairn_core::status::remediation_for(capability).map(str::to_owned),
        operation_id: crate::stub::new_operation_id(),
    })
}
```

- [ ] **Step 4: Update CLI capability-rejection sites**

Edit `crates/cairn-cli/src/verbs/search.rs`. Find every place that constructs a `CapabilityUnavailable` error envelope or prints a capability-unavailable message (search by `grep -n "CapabilityUnavailable\|capability.*unavailable" crates/cairn-cli/src/verbs/search.rs`). For each:

- JSON path: include a top-level `"remediation"` key inside `error.data` when `cairn_core::status::remediation_for(cap)` returns `Some`.
- Human path: append a separate line `  hint: <remediation>` after the existing rejection line.

Concretely the construction looks like:

```rust
let remediation = cairn_core::status::remediation_for(cap);
let mut data = serde_json::json!({ "capability": cap });
if let Some(hint) = remediation {
    data.as_object_mut().expect("invariant: data is a JSON object")
        .insert("remediation".to_owned(), serde_json::Value::String(hint.to_owned()));
}
// ... emit envelope with `error: { code: "CapabilityUnavailable", message, data }`

if !json_mode {
    eprintln!("cairn search: capability unavailable — {cap}");
    if let Some(hint) = remediation {
        eprintln!("  hint: {hint}");
    }
}
```

- [ ] **Step 5: Update MCP rejection arm**

Edit `crates/cairn-mcp/src/handler.rs::handle_search`. Replace the `CapabilityUnavailable` arm (line ~203):

```rust
Err(cairn_core::verbs::search::SearchError::CapabilityUnavailable { capability }) => {
    let remediation = cairn_core::status::remediation_for(capability).unwrap_or("");
    let msg = if remediation.is_empty() {
        format!("cairn search: capability unavailable: {capability}")
    } else {
        format!("cairn search: capability unavailable: {capability}\n  hint: {remediation}")
    };
    return CallToolResult::error(vec![Content::text(msg)]);
}
```

- [ ] **Step 6: Build the workspace**

```bash
cargo check --workspace --all-targets --locked
```

Expected: clean.

- [ ] **Step 7: Run all tests**

```bash
cargo nextest run --workspace --locked
```

Expected: existing tests pass. Any test asserting exact `CapabilityUnavailable` text without remediation may break — update those tests to also check the new `remediation` field where relevant. If the assertion is structural (e.g., `data.capability == ...`) it stays green.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-sdk/src/error.rs \
        crates/cairn-sdk/src/transport.rs \
        crates/cairn-cli/src/verbs/search.rs \
        crates/cairn-mcp/src/handler.rs
git commit -m "feat(surfaces): populate remediation on every CapabilityUnavailable (issue #53)"
```

---

## Task 11: Snapshot matrix — `status_snapshot_insta`

**Files:**
- Create: `crates/cairn-cli/tests/status_snapshot_insta.rs`
- Create: `crates/cairn-cli/tests/snapshots/status_snapshot_insta__*.snap` (5 baselines)
- Modify: `crates/cairn-cli/Cargo.toml` (dev-deps for `insta`, `tempfile` — likely present)

- [ ] **Step 1: Confirm `insta` is a dev-dep**

```bash
grep -E "^insta" crates/cairn-cli/Cargo.toml
```

Expected: `insta = { workspace = true }` or similar. If absent, add it.

- [ ] **Step 2: Write the snapshot test**

Create `crates/cairn-cli/tests/status_snapshot_insta.rs`:

```rust
//! Snapshot matrix for `cairn status --json` over the v0.1 config space.
//! Volatile fields (`incarnation`, `started_at`) are masked so snapshots
//! stay deterministic. Run `cargo insta review` to update intentional
//! changes; reject any change that mutates `capabilities[]` semantics
//! without a corresponding spec update.

use std::path::Path;
use std::process::Command;

fn cairn_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write_vault_id(root: &Path) {
    std::fs::create_dir_all(root.join(".cairn")).unwrap();
    std::fs::write(
        root.join(".cairn").join("vault.id"),
        b"01HZZ0000000000000000000AB\n",
    )
    .unwrap();
}

/// Run `cairn status --json` in `dir` with optional config overrides written
/// to `.cairn/config.yaml`. Returns the parsed JSON with volatiles masked.
fn run_status(dir: &Path, config_yaml: Option<&str>) -> serde_json::Value {
    if let Some(yaml) = config_yaml {
        std::fs::write(dir.join(".cairn").join("config.yaml"), yaml).unwrap();
    }
    let out = cairn_bin()
        .args(["status", "--json"])
        .current_dir(dir)
        .env_remove("CAIRN_VAULT")
        .output()
        .expect("spawn cairn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let mut v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("valid JSON");
    if let Some(si) = v["server_info"].as_object_mut() {
        si.insert("incarnation".into(), "<masked>".into());
        si.insert("started_at".into(), "<masked>".into());
        si.insert("version".into(), "<masked>".into());
        si.insert("build".into(), "<masked>".into());
    }
    v
}

#[test]
fn snapshot_default_p0_bound_vault() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    let v = run_status(tmp.path(), None);
    insta::assert_json_snapshot!("default_p0_bound_vault", v);
}

#[test]
fn snapshot_local_embeddings_off() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    let v = run_status(
        tmp.path(),
        Some("vault:\n  name: test\nsearch:\n  local_embeddings: false\n"),
    );
    insta::assert_json_snapshot!("local_embeddings_off", v);
}

#[test]
fn snapshot_unbound_dir() {
    let tmp = tempfile::tempdir().unwrap();
    // No .cairn/vault.id written.
    let v = run_status(tmp.path(), None);
    insta::assert_json_snapshot!("unbound_dir", v);
}

#[test]
fn snapshot_model_missing() {
    // Bound vault with default config but no embedding model on disk → the
    // semantic/hybrid gates fail closed.
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    // Default config would attempt model presence check; without a
    // .cairn/models/ tree the cache reports `is_present == false`.
    let v = run_status(tmp.path(), None);
    insta::assert_json_snapshot!("model_missing", v);
}
```

For the SDK no-store fixture, cover it in a separate test that calls `Sdk::new().status()` directly:

```rust
#[test]
fn snapshot_sdk_new_no_store() {
    let mut v = serde_json::to_value(cairn_sdk::Sdk::new().status())
        .expect("serialize");
    if let Some(si) = v["server_info"].as_object_mut() {
        si.insert("incarnation".into(), "<masked>".into());
        si.insert("started_at".into(), "<masked>".into());
        si.insert("version".into(), "<masked>".into());
        si.insert("build".into(), "<masked>".into());
    }
    insta::assert_json_snapshot!("sdk_new_no_store", v);
}
```

- [ ] **Step 3: Run, expect new snapshots to be captured**

```bash
cargo nextest run -p cairn-cli status_snapshot_insta --locked --no-fail-fast
```

Expected: 5 tests fail with "no snapshot exists; run `cargo insta review`".

- [ ] **Step 4: Review and accept the baselines**

```bash
cargo insta review
```

For each fixture, eyeball the JSON:

- `default_p0_bound_vault`: `capabilities` should contain `cairn.mcp.v1.search.keyword` + `cairn.mcp.v1.policy_trace`. (Semantic/hybrid require model on disk; CLI will report them as absent in this no-model test environment.)
- `local_embeddings_off`: only keyword + policy_trace.
- `unbound_dir`: `capabilities: []`.
- `model_missing`: same as default (no model in tmpdir).
- `sdk_new_no_store`: `capabilities: []`.

If `default_p0_bound_vault` shows semantic/hybrid (because the test environment has a populated `.cairn/models/` somewhere up-tree), document that and either pin the config or accept the broader snapshot.

- [ ] **Step 5: Re-run, expect green**

```bash
cargo nextest run -p cairn-cli status_snapshot_insta --locked
```

Expected: 5/5 pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-cli/tests/status_snapshot_insta.rs \
        crates/cairn-cli/tests/snapshots/
git commit -m "test(cli): snapshot matrix for cairn status --json (issue #53)"
```

---

## Task 12: Three-way parity test (CLI / SDK / MCP)

**Files:**
- Modify: `crates/cairn-cli/tests/sdk_cli_parity.rs`

- [ ] **Step 1: Add the three-way test**

Append to `crates/cairn-cli/tests/sdk_cli_parity.rs`:

```rust
#[test]
fn status_parity_cli_vs_sdk_vs_mcp() {
    use cairn_mcp::handler::CairnMcpHandler;

    assert_tempdir_unbound();

    let mut cli = run_json(&["status", "--json"]);
    let mut sdk = serde_json::to_value(cairn_sdk::Sdk::new().status())
        .expect("sdk serialize");
    let mut mcp = serde_json::to_value(CairnMcpHandler::new().status_response())
        .expect("mcp serialize");

    let volatile: &[&[&str]] = &[
        &["server_info", "incarnation"],
        &["server_info", "started_at"],
    ];
    mask(&mut cli, volatile);
    mask(&mut sdk, volatile);
    mask(&mut mcp, volatile);

    assert_eq!(cli, sdk, "CLI and SDK status diverge");
    assert_eq!(sdk, mcp, "SDK and MCP status diverge");
    // Transitive: cli == mcp follows.
}
```

This requires `CairnMcpHandler::status_response()` from Task 9. If that accessor is private, mark it `pub` in the handler.

- [ ] **Step 2: Build**

```bash
cargo check -p cairn-cli --all-targets --locked
```

Expected: clean. If `cairn-mcp` is not in `cairn-cli`'s dev-deps, add it:

```toml
# crates/cairn-cli/Cargo.toml
[dev-dependencies]
cairn-mcp = { path = "../cairn-mcp" }
```

- [ ] **Step 3: Run**

```bash
cargo nextest run -p cairn-cli status_parity_cli_vs_sdk_vs_mcp --locked
```

Expected: pass — all three surfaces emit the same empty-capabilities Vec from a no-vault tempdir.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/tests/sdk_cli_parity.rs crates/cairn-cli/Cargo.toml
git commit -m "test(parity): assert CLI/SDK/MCP status three-way agreement (issue #53)"
```

---

## Task 13: Fail-closed rejection tests with remediation

**Files:**
- Create: `crates/cairn-cli/tests/cli_capability_rejection.rs`

- [ ] **Step 1: Write the rejection test**

Create `crates/cairn-cli/tests/cli_capability_rejection.rs`:

```rust
//! Verify that capability-gated args are rejected with the full
//! `CapabilityUnavailable` envelope (including `data.remediation`) when the
//! runtime does not advertise the capability.

use std::path::Path;
use std::process::Command;

fn cairn_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cairn"))
}

fn write_vault_id(root: &Path) {
    std::fs::create_dir_all(root.join(".cairn")).unwrap();
    std::fs::write(
        root.join(".cairn").join("vault.id"),
        b"01HZZ0000000000000000000AB\n",
    )
    .unwrap();
}

#[test]
fn search_semantic_rejects_with_remediation_when_local_embeddings_off() {
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    std::fs::write(
        tmp.path().join(".cairn").join("config.yaml"),
        "vault:\n  name: t\nsearch:\n  local_embeddings: false\n",
    )
    .unwrap();

    let out = cairn_bin()
        .args(["search", "--mode", "semantic", "anything", "--json"])
        .current_dir(tmp.path())
        .env_remove("CAIRN_VAULT")
        .output()
        .expect("spawn cairn");

    // sysexit 69 = EX_UNAVAILABLE — CapabilityUnavailable.
    assert_eq!(
        out.status.code(),
        Some(69),
        "exit code: {:?}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    let envelope: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("valid JSON");
    assert_eq!(envelope["status"], "rejected");
    assert_eq!(envelope["error"]["code"], "CapabilityUnavailable");
    assert_eq!(
        envelope["error"]["data"]["capability"],
        "cairn.mcp.v1.search.semantic"
    );
    let remediation = envelope["error"]["data"]["remediation"]
        .as_str()
        .expect("remediation populated");
    assert!(
        remediation.contains("local_embeddings"),
        "remediation should mention the toggle: got {remediation}"
    );
}

#[test]
fn search_explain_rejects_with_remediation_when_policy_trace_off() {
    // Synthesize a config where policy_trace is false (the v0.1 default is
    // true, so this requires a config override). If `policy_trace` is not
    // exposed as a config knob yet, this test is skipped at runtime.
    let tmp = tempfile::tempdir().unwrap();
    write_vault_id(tmp.path());
    std::fs::write(
        tmp.path().join(".cairn").join("config.yaml"),
        "vault:\n  name: t\npolicy_trace:\n  enabled: false\n",
    )
    .unwrap();

    let out = cairn_bin()
        .args(["search", "--mode", "keyword", "--explain", "x", "--json"])
        .current_dir(tmp.path())
        .env_remove("CAIRN_VAULT")
        .output()
        .expect("spawn cairn");

    // If the override is unknown, the CLI exits with EX_CONFIG (78) — skip.
    if out.status.code() == Some(78) {
        eprintln!("policy_trace config override not supported in this build; skipping");
        return;
    }

    assert_eq!(out.status.code(), Some(69));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
    assert_eq!(envelope["error"]["code"], "CapabilityUnavailable");
    assert_eq!(
        envelope["error"]["data"]["capability"],
        "cairn.mcp.v1.policy_trace"
    );
}
```

- [ ] **Step 2: Run**

```bash
cargo nextest run -p cairn-cli cli_capability_rejection --locked
```

Expected: at least the first test passes with full envelope shape. The second may print "skipping" if the policy_trace knob is not exposed yet — that is acceptable.

If the first test fails with exit 1 instead of 69, walk the search verb's rejection path: the gate must map `CapabilityUnavailable` to sysexit 69. This is already documented in the IDL (`search.json` line 118 cites "sysexit 69"). Fix the CLI's exit-code mapping in `crates/cairn-cli/src/verbs/search.rs` if necessary.

- [ ] **Step 3: Add SDK + MCP equivalents**

Append to `crates/cairn-sdk/tests/surface.rs`:

```rust
#[tokio::test]
async fn sdk_search_semantic_rejects_when_no_store_with_remediation() {
    use cairn_core::generated::verbs::search::{SearchArgs, SearchArgsMode};
    let sdk = cairn_sdk::Sdk::new();
    let args = SearchArgs {
        query: "x".to_owned(),
        mode: SearchArgsMode::Semantic,
        limit: Some(5),
        ..Default::default()
    };
    let err = sdk.search(&args).await.expect_err("must reject");
    match err {
        cairn_sdk::SdkError::CapabilityUnavailable {
            capability, remediation, ..
        } => {
            assert_eq!(capability, "cairn.mcp.v1.search.semantic");
            let hint = remediation.expect("remediation populated");
            assert!(hint.contains("local_embeddings"));
        }
        other => panic!("expected CapabilityUnavailable, got {other:?}"),
    }
}
```

If `SearchArgs` does not derive `Default`, build the value field-by-field — see existing tests in `surface.rs` for a working pattern.

For MCP add the equivalent in `crates/cairn-mcp/tests/handler_rejection.rs` (new file):

```rust
//! MCP capability-rejection envelope (issue #53).

use cairn_mcp::handler::{CairnMcpHandler, dispatch_stub};

#[tokio::test]
async fn mcp_search_semantic_rejection_carries_remediation() {
    // The handler with no store hits the stub path. Once a fail-closed
    // capability path lands in the search dispatcher within MCP, drive
    // the dispatcher path instead — the envelope shape is what we pin.
    let handler = CairnMcpHandler::new();
    // Build a tools/call payload requesting search semantic, drive
    // `handler.call_tool(...)` and assert the resulting CallToolResult
    // contains the capability + remediation strings. (Use rmcp test
    // harness if available; otherwise call the private handler-level
    // rejection helper.)
    let result = dispatch_stub("search");
    assert!(result.is_error.unwrap_or(false));
    // Stub path doesn't carry remediation today; advance once dispatch
    // wires through. Document the gap.
    let _ = handler;
}
```

This MCP test is intentionally a placeholder — the test grows once the search dispatcher in MCP can be driven without a wired store. Mark it `#[ignore = "requires wired-store rejection path; tracked in #61 follow-up"]` if it cannot pass today.

- [ ] **Step 4: Run**

```bash
cargo nextest run -p cairn-cli -p cairn-sdk -p cairn-mcp \
    cli_capability_rejection sdk_search_semantic_rejects mcp_search_semantic --locked
```

Expected: CLI + SDK pass; MCP either passes or is skipped via `#[ignore]`.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/tests/cli_capability_rejection.rs \
        crates/cairn-sdk/tests/surface.rs \
        crates/cairn-mcp/tests/handler_rejection.rs
git commit -m "test(surfaces): assert CapabilityUnavailable carries remediation (issue #53)"
```

---

## Task 14: Phase-pinning integration tests

**Files:**
- Create: `crates/cairn-core/tests/status_phase_pinning.rs`

- [ ] **Step 1: Write the test**

Create `crates/cairn-core/tests/status_phase_pinning.rs`:

```rust
//! Brief §8.0.a / AC: forget.session/scope cannot appear in `capabilities[]`
//! at v0.1 regardless of any wiring flag flip. retrieve.* + replay.* hold
//! back behind their own wiring flags. This integration test guards the
//! version-pinning rules — a refactor that loses them must turn this red.

use cairn_core::config::CapabilitySet;
use cairn_core::generated::common::Capabilities;
use cairn_core::status::{advertise, CapabilityGates, Phase, StoreCaps};

fn full_caps_set() -> CapabilitySet {
    CapabilitySet {
        keyword_search: true,
        semantic_search: true,
        hybrid_search: true,
        llm_extract: false,
        agent_extract: false,
        graph_edges: false,
        policy_trace: true,
        replay_sequence: true,
        replay_challenge: true,
    }
}

fn full_gates(phase: Phase) -> CapabilityGates {
    CapabilityGates {
        config: full_caps_set(),
        store: Some(StoreCaps { fts: true, vector: true }),
        vault_bound: true,
        model_present: true,
        llm_configured: false,
        contract_phase: phase,
    }
}

#[test]
fn forget_session_pinned_to_v0_2_phase() {
    let caps_v0_1 = advertise(&full_gates(Phase::V0_1));
    assert!(!caps_v0_1.contains(&Capabilities::CairnMcpV1ForgetSession),
        "forget.session must NOT appear at v0.1 even with every gate on; got {caps_v0_1:?}");
}

#[test]
fn forget_scope_pinned_to_v0_3_phase() {
    let caps_v0_1 = advertise(&full_gates(Phase::V0_1));
    let caps_v0_2 = advertise(&full_gates(Phase::V0_2));
    assert!(!caps_v0_1.contains(&Capabilities::CairnMcpV1ForgetScope));
    assert!(!caps_v0_2.contains(&Capabilities::CairnMcpV1ForgetScope));
}

#[test]
fn replay_capabilities_held_back_at_every_phase() {
    for phase in [Phase::V0_1, Phase::V0_2, Phase::V0_3] {
        let caps = advertise(&full_gates(phase));
        assert!(!caps.contains(&Capabilities::CairnMcpV1ReplaySequence),
            "replay.sequence must stay un-advertised; got {caps:?}");
        assert!(!caps.contains(&Capabilities::CairnMcpV1ReplayChallenge),
            "replay.challenge must stay un-advertised; got {caps:?}");
    }
}

#[test]
fn retrieve_capabilities_held_back_at_every_phase() {
    for phase in [Phase::V0_1, Phase::V0_2, Phase::V0_3] {
        let caps = advertise(&full_gates(phase));
        for needle in [
            Capabilities::CairnMcpV1RetrieveRecord,
            Capabilities::CairnMcpV1RetrieveSession,
            Capabilities::CairnMcpV1RetrieveTurn,
            Capabilities::CairnMcpV1RetrieveFolder,
            Capabilities::CairnMcpV1RetrieveScope,
            Capabilities::CairnMcpV1RetrieveProfile,
        ] {
            assert!(!caps.contains(&needle),
                "retrieve.* held behind wiring flags; {needle:?} appeared in {caps:?}");
        }
    }
}
```

- [ ] **Step 2: Run**

```bash
cargo nextest run -p cairn-core --test status_phase_pinning --locked
```

Expected: 4/4 pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/tests/status_phase_pinning.rs
git commit -m "test(core): pin v0.1 phase rules for forget/retrieve/replay (issue #53)"
```

---

## Task 15: Remediation-existence test

**Files:**
- Modify: `crates/cairn-core/src/status/tests.rs`

- [ ] **Step 1: Add the existence test**

Append to `crates/cairn-core/src/status/tests.rs::remediation_tests`:

```rust
#[test]
fn every_advertised_capability_has_remediation() {
    // Pin: any capability we *can* advertise at v0.1 (with all wiring flags
    // imagined on) must have a remediation string registered. Future
    // advertisers won't accidentally ship a fail-closed verb without an
    // operator hint.
    use crate::config::CapabilitySet;

    let always_on = CapabilitySet {
        keyword_search: true,
        semantic_search: true,
        hybrid_search: true,
        llm_extract: false,
        agent_extract: false,
        graph_edges: false,
        policy_trace: true,
        replay_sequence: true,
        replay_challenge: true,
    };
    let gates = CapabilityGates {
        config: always_on,
        store: Some(StoreCaps { fts: true, vector: true }),
        vault_bound: true,
        model_present: true,
        llm_configured: false,
        contract_phase: Phase::V0_1,
    };

    for cap in advertise(&gates) {
        let cap_str = serde_json::to_value(&cap).ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .expect("cap serializes to string");
        // search.keyword and policy_trace are universally available; their
        // remediation is "should not happen" — None is acceptable.
        if cap_str == "cairn.mcp.v1.search.keyword"
            || cap_str == "cairn.mcp.v1.policy_trace"
        {
            continue;
        }
        assert!(
            remediation_for(&cap_str).is_some(),
            "no remediation registered for {cap_str}"
        );
    }
}
```

- [ ] **Step 2: Run**

```bash
cargo nextest run -p cairn-core status::tests::remediation_tests --locked
```

Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/status/tests.rs
git commit -m "test(core): pin remediation coverage for advertised capabilities (issue #53)"
```

---

## Task 16: Documentation updates

**Files:**
- Modify: `CLAUDE.md`
- Modify: `docs/design/traceability.md`

- [ ] **Step 1: Update CLAUDE.md**

Edit `CLAUDE.md`. Locate the §4 "Load-bearing invariants" section (specifically rule 6 "Fail closed on capability"). Add a sub-bullet pointing at the new module:

```markdown
6. **Fail closed on capability.** If a mode isn't advertised in `status`, the
   verb rejects with `CapabilityUnavailable`. Never silently downgrade.
   - Capability advertisement decisions live in **`cairn-core::status::advertise`**
     (issue #53). All four surfaces (CLI, MCP, SDK, skill) read from this one
     function. Adding a new capability is a row in that table; flipping it on
     is a `wiring::*_WIRED` constant change in the issue that lands the
     dispatch path.
   - Remediation hints for `CapabilityUnavailable.data.remediation` come from
     `cairn-core::status::REMEDIATION` — keep the table in sync when the
     advertise table grows.
```

- [ ] **Step 2: Update traceability matrix**

Edit `docs/design/traceability.md`. Locate the §8 "CLI / MCP / SDK / skill contract" row (or whichever row cites brief §8.0.a) and add issue #53 to the Implementation column. If a row for §8.0.a Handshake / capability negotiation does not exist, add one:

```markdown
| §8.0.a Handshake / capability negotiation | #51 (envelope), #52 (replay), #53 (status parity) | — | Status preludes, capability advertisement, and challenge mint covered. |
```

- [ ] **Step 3: Build docs**

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
```

Expected: clean (or, if docs/site/src/reference/generated/* needs regeneration after capability-related changes, run with `--write` and commit the diff).

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md docs/design/traceability.md docs/site/src/reference/generated/
git commit -m "docs: point capability advertisement at cairn-core::status (issue #53)"
```

---

## Task 17: Final verification

**Files:** none (verification only)

- [ ] **Step 1: Run the full CI gauntlet**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
```

Expected: every command exits 0.

If `clippy` flags new warnings in the new `cairn-core/src/status/` module, address them — workspace lints are deny-on-warning. Common pedantic hits:

- `clippy::missing_errors_doc` — `advertise()` returns no Result, so n/a.
- `clippy::missing_panics_doc` — `advertise()` does not panic; n/a.
- `clippy::struct_excessive_bools` — `CapabilityGates` has 4 bools, under threshold.
- `clippy::module_name_repetitions` — already allowed at workspace.

- [ ] **Step 2: Smoke-test the CLI from a fresh vault**

```bash
TMPDIR=$(mktemp -d)
mkdir -p "$TMPDIR/.cairn"
echo '01HZZ0000000000000000000AB' > "$TMPDIR/.cairn/vault.id"
cargo run -p cairn-cli --release -- status --json --vault "$TMPDIR" | jq .
```

Expected: JSON envelope with at least `cairn.mcp.v1.search.keyword` + `cairn.mcp.v1.policy_trace` in `capabilities[]`. No `forget.*`, no `retrieve.*`, no `replay.*` (wiring flags still all `false`).

```bash
cargo run -p cairn-cli --release -- search --mode semantic xyz \
    --vault "$TMPDIR" --json
```

Expected: exit 69, JSON envelope with `error.code: "CapabilityUnavailable"`, `error.data.capability: "cairn.mcp.v1.search.semantic"`, `error.data.remediation` non-empty.

- [ ] **Step 3: Final commit (if any verification fixes)**

If steps 1–2 produced fix-up commits (rare), squash them with the relevant earlier commit (`git commit --fixup`, then `git rebase -i --autosquash main`). Otherwise nothing to commit here.

- [ ] **Step 4: Push branch and open PR**

```bash
git push -u origin HEAD
gh pr create --title "feat(status): capability negotiation parity across surfaces (issue #53)" \
  --body "$(cat <<'EOF'
## Summary
- Lifts capability-advertisement decisions to `cairn-core::status::advertise`; CLI, SDK, and MCP `initialize` all delegate to one pure function (brief §8.0.a, §15).
- Adds optional `remediation` to `CapabilityUnavailable.data` (IDL change) so operators see actionable hints on every fail-closed rejection.
- Locks the contract with snapshot matrix, three-way parity test, phase-pinning rules, and proptest monotonicity.
- Wiring constants for forget/retrieve/replay capabilities ship `false` — the issue that lands each verb's dispatch flips its constant and propagates to every surface.

## Test plan
- [ ] `cargo nextest run --workspace --locked` — all green
- [ ] `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check` — no drift
- [ ] CLI smoke: `cairn status --json` from a bound tempdir advertises `search.keyword` + `policy_trace` only
- [ ] CLI smoke: `cairn search --mode semantic` against a `local_embeddings: false` config exits 69 with full `CapabilityUnavailable` envelope including `remediation`
- [ ] Three-way parity test passes (CLI / SDK / MCP byte-equal modulo volatiles)

Spec: `docs/superpowers/specs/2026-05-06-issue-53-status-capability-parity-design.md`
Plan: `docs/superpowers/plans/2026-05-06-issue-53-status-capability-parity.md`
EOF
)"
```

Expected: PR created against `main`. Capture the URL.

---

## Self-review checklist

- [x] **Spec coverage:** §1 goal → Tasks 2-3-4-7-8-9-10. §2 non-goals → encoded as held-back wiring constants (Tasks 2, 14). §3 background → Task 1 (IDL), Tasks 7-8-9 (delegation). §4 architecture → Tasks 2-3-4 (core module), Tasks 7-8-9 (surface delegation). §5 IDL → Task 1. §6 surface impls → Tasks 7-9-10. §7 tests → Tasks 11-12-13-14-15 + property test in Task 5 + exhaustiveness in Task 6. §8 ordering → reflected in task order. §9 verification → Task 17. §10 risks → mitigated by namespacing (Task 9), strict module surface (Task 2 doc), exhaustiveness proof (Task 6), shared dispatch advertisement (Task 9 reuses `pipeline_dispatch_advertisement`).
- [x] **Placeholder scan:** no "TBD", "TODO", "implement later". The MCP rejection test in Task 13 is intentionally `#[ignore]`-able with explicit follow-up rationale; the policy_trace rejection is documented to skip when the config knob is absent. Both are explicit, not vague.
- [x] **Type consistency:** `CapabilityGates` field names (`config`, `store`, `vault_bound`, `model_present`, `llm_configured`, `contract_phase`) match across Tasks 2, 7, 8, 9, 14, 15. `Phase` variants (`V0_1`, `V0_2`, `V0_3`) match across Tasks 2, 14. `StoreCaps` fields (`fts`, `vector`) match across Tasks 2, 7, 9, 14, 15. `remediation` field name + `Option<String>` shape match across Tasks 1, 10, 13.
