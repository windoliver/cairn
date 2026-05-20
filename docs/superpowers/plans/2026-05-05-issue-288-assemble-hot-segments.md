# `assemble_hot` cache-breakpoint segments — Implementation Plan (issue #288)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the IDL-generated `AssembleHotData` to carry recipe-step `HotSegment` markers, validate them at the wire trust boundary via `#[serde(try_from)]`, and wire `cairn assemble_hot` end-to-end with a stub-body assembler so harness wrappers can attach provider-specific prompt-cache breakpoints.

**Architecture:** A pure helper (`build_segments`) in `cairn-core` produces canonical segments by construction; a layered validator (`validate_base` + `validate_segments` + recipe-aware `validate_with_recipe`) runs inside a `try_from` `Deserialize` so every decode path enforces invariants. A stub `assemble_hot(config)` walks `HotMemoryConfig.recipe`, calls a `load_step_body` placeholder returning `""`, and returns a fully validated `AssembleHotData`. The CLI / SDK return that value; real source loading is the missing-half of #193.

**Tech Stack:** Rust 1.95.0 (edition 2024), `serde`, `serde_json`, `sha2 = "0.10"` (new direct dep), `thiserror`, `proptest`, `insta`, `rstest`. JSON Schema 2020-12 for IDL. `cargo nextest` for the test runner.

**Spec:** [`docs/superpowers/specs/2026-05-05-issue-288-assemble-hot-segments-design.md`](../specs/2026-05-05-issue-288-assemble-hot-segments-design.md).

**Pre-flight reading (do this first, do not skip):**

1. Re-read the spec top to bottom — every contract decision below is grounded in it.
2. Re-read `CLAUDE.md` §6.2 (errors), §6.3 (async), §6.10 (API design), §8 (verification checklist).
3. Skim `crates/cairn-idl/src/codegen/emit_sdk.rs` around line 1786 (`emit_struct_field`) and line 2872 (Raw-mirror pattern) — the `x-cairn-validate` codegen extension in Task 2 reuses that infrastructure.
4. Skim `crates/cairn-core/src/generated/envelope/mod.rs` lines 280–320 to see how `ResponseData::AssembleHot` is decoded — your `try_from` validation runs on that path automatically.

---

## File Map

**New files (`cairn-core`):**

- `crates/cairn-core/src/verbs/assemble_hot/mod.rs` — module root, public re-exports, `From<config::HotMemoryRecipeStep>` conversion.
- `crates/cairn-core/src/verbs/assemble_hot/segments.rs` — `build_segments`, `default_stability`, `MAX_SEGMENTS`, validators, error enum.
- `crates/cairn-core/src/verbs/assemble_hot/raw.rs` — `AssembleHotDataRaw` mirror + `TryFrom<Raw> for AssembleHotData` and `From<AssembleHotData> for Raw`.
- `crates/cairn-core/src/verbs/assemble_hot/assembler.rs` — stub `assemble_hot(config)` and `load_step_body(step)`.
- `crates/cairn-core/tests/assemble_hot_envelope.rs` — trust-boundary integration tests.
- `crates/cairn-core/tests/assemble_hot_snapshots.rs` — `insta` snapshot of canonical JSON.

**New files (`cairn-cli`):**

- `crates/cairn-cli/tests/cli_assemble_hot.rs` — end-to-end CLI `--json` snapshot.

**Modified files:**

- `crates/cairn-idl/schema/verbs/assemble_hot.json` — add `segments`, `HotSegment` def, recipe/stability enums, `x-cairn-validate`, `maxItems`.
- `crates/cairn-idl/src/codegen/emit_sdk.rs` — recognize `x-cairn-validate: true` and emit `#[serde(try_from, into)]` + Raw mirror.
- `crates/cairn-core/Cargo.toml` — add `sha2` workspace dep.
- `crates/cairn-core/src/verbs/mod.rs` — add `pub mod assemble_hot;`.
- `crates/cairn-cli/src/verbs/assemble_hot.rs` — replace `unimplemented_response` with real call.
- `crates/cairn-sdk/src/transport.rs` — replace `unimplemented_response` with real call.
- `crates/cairn-sdk/tests/surface.rs` — extend with assemble_hot round-trip case.

**Auto-regenerated** (by `cargo run -p cairn-idl --bin cairn-codegen`):

- `crates/cairn-core/src/generated/verbs/assemble_hot.rs`
- `crates/cairn-core/src/generated/envelope/mod.rs` (untouched logically; may pick up minor whitespace)
- `crates/cairn-mcp/src/generated/schemas/verbs/assemble_hot.json`
- `crates/cairn-mcp/src/generated/...` (skill-pack snapshots)
- `crates/cairn-cli/src/generated/...`
- `crates/cairn-sdk/src/generated/...`

---

## Task 1: IDL schema — add segments/HotSegment/enums

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/assemble_hot.json`

- [ ] **Step 1: Edit the schema**

Replace the `Data` definition (and add `HotSegment`, `HotRecipeStep`, `SegmentStability` to `$defs`) with:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://cairn.dev/schema/cairn.mcp.v1/verbs/assemble_hot.json",
  "title": "Cairn verb: assemble_hot",
  "x-cairn-contract": "cairn.mcp.v1",
  "x-cairn-verb-id": "assemble_hot",
  "x-cairn-capability": null,
  "x-cairn-auth": "rebac",
  "x-cairn-cli": {
    "command": "assemble_hot",
    "flags": [
      { "name": "session_id", "long": "session", "value_source": "string" },
      { "name": "budget",     "long": "budget",  "value_source": "u32" }
    ]
  },
  "x-cairn-skill-triggers": {
    "positive": ["use at the start of every turn to load the hot-memory prefix for this agent/session"],
    "negative": ["do NOT call in a tight inner loop — one call per turn"],
    "exclusivity": "this is the canonical hot-prefix surface"
  },
  "type": "object",
  "$defs": {
    "Args": {
      "type": "object",
      "additionalProperties": false,
      "properties": {
        "session_id": { "type": "string", "minLength": 1 },
        "budget":     { "type": "integer", "minimum": 0, "maximum": 4194304, "description": "Byte budget for the assembled prefix (default 25000; hard cap 4 MiB)." }
      }
    },
    "Data": {
      "type": "object",
      "additionalProperties": false,
      "x-cairn-validate": true,
      "required": ["prefix", "bytes"],
      "properties": {
        "prefix": { "type": "string", "description": "Assembled hot-memory text ready to inject into the agent prompt. May be empty when no hot-memory is available." },
        "bytes":  { "type": "integer", "minimum": 0, "maximum": 4194304 },
        "segments": {
          "type": "array",
          "items": { "$ref": "#/$defs/HotSegment" },
          "maxItems": 64,
          "description": "Recipe-step segments covering [0, bytes) contiguously, in declaration order. Three-state contract: (1) FIELD ABSENT ON THE WIRE — legacy cairn.mcp.v1 producer that predates this feature; consumers MUST treat prefix as opaque. (2) \"segments\": [] — new producer ran with NO recipe configured; the producer explicitly opts into the new contract but had nothing to emit. REQUIRED INVARIANT: prefix == \"\" AND bytes == 0. (3) \"segments\": [...] — new producer with a configured recipe; len() mirrors HotMemoryConfig.recipe.len() 1:1, including N zero-length entries when the recipe ran with no content. The Rust binding distinguishes (1) from (2) via Option<Vec<HotSegment>> (None vs Some(vec![])); JSON-only consumers distinguish them by field presence. NOTE: there is intentionally NO default: [] here — schema-aware tooling that materialized defaults would rewrite legacy-absent into canonical-empty and trip the EmptySegmentsRequiresEmptyPrefix invariant. Forward-compat depends on absence staying absence."
        }
      }
    },
    "HotSegment": {
      "type": "object",
      "additionalProperties": false,
      "required": ["step", "byte_start", "byte_end", "stability", "content_hash"],
      "properties": {
        "step":         { "$ref": "#/$defs/HotRecipeStep" },
        "byte_start":   { "type": "integer", "minimum": 0, "maximum": 4194304 },
        "byte_end":     { "type": "integer", "minimum": 0, "maximum": 4194304 },
        "stability":    { "$ref": "#/$defs/SegmentStability" },
        "content_hash": { "type": "string", "pattern": "^[0-9a-f]{64}$", "description": "Lowercase hex SHA-256 of prefix[byte_start..byte_end].as_bytes(). Wrappers in non-UTF-8 string-typed languages (JS/Java UTF-16) MUST encode prefix to UTF-8 bytes before slicing — naïve String.slice() uses code units and will corrupt non-ASCII segments." }
      }
    },
    "HotRecipeStep": {
      "type": "string",
      "enum": ["purpose", "index", "pinned_feedback", "top_salience_project", "active_playbook", "recent_user_signal"],
      "description": "Frozen for the lifetime of cairn.mcp.v1. Adding a recipe step requires bumping to cairn.mcp.v2."
    },
    "SegmentStability": {
      "type": "string",
      "enum": ["stable_1h", "stable_5m", "volatile"],
      "description": "Cache-lifetime hint per segment. Frozen for the lifetime of cairn.mcp.v1."
    }
  }
}
```

- [ ] **Step 2: Verify the schema is valid JSON**

Run: `python3 -m json.tool crates/cairn-idl/schema/verbs/assemble_hot.json > /dev/null && echo OK`
Expected: `OK`.

- [ ] **Step 3: Commit (do not run codegen yet — Task 2 lands the codegen support first)**

```bash
git add crates/cairn-idl/schema/verbs/assemble_hot.json
git commit -m "feat(idl): assemble_hot schema with HotSegment + frozen enums (#288)"
```

---

## Task 2: IDL codegen — support `x-cairn-validate: true`

The schema annotation tells the codegen to emit `#[serde(try_from = "<Name>Raw", into = "<Name>Raw")]` on the main struct and a sibling `<Name>Raw` mirror struct with `derive(Serialize, Deserialize)` and identical fields. Hand-written `TryFrom<Raw>` and `From<X> for Raw` impls live outside generated code (Task 9).

**Files:**
- Modify: `crates/cairn-idl/src/codegen/ir.rs` (add `validate: bool` flag to `StructDef`)
- Modify: `crates/cairn-idl/src/codegen/loader.rs` (parse `x-cairn-validate` into the IR)
- Modify: `crates/cairn-idl/src/codegen/emit_sdk.rs` (emit `try_from` + Raw mirror when `validate == true`)
- Modify: `crates/cairn-idl/src/codegen/skill_compat.rs` (if it references struct definitions, ensure no regression — likely no change needed)
- Test: `crates/cairn-idl/tests/codegen_emit_sdk.rs` (or wherever existing emit_sdk unit tests live; check `tests/` for the pattern)

- [ ] **Step 1: Read the existing IR and emitter**

Run: `wc -l crates/cairn-idl/src/codegen/{ir.rs,loader.rs,emit_sdk.rs}` to get a feel for size, then `grep -n "struct StructDef\|struct StructField" crates/cairn-idl/src/codegen/ir.rs | head` and `grep -n "fn emit_struct\|StructDef\|emit_struct_field" crates/cairn-idl/src/codegen/emit_sdk.rs | head -20` to find the touch points.

- [ ] **Step 2: Add `validate: bool` to `StructDef` in ir.rs**

```rust
// in crates/cairn-idl/src/codegen/ir.rs, in StructDef
pub struct StructDef {
    // ... existing fields
    /// `x-cairn-validate: true` on the schema. When set, the SDK emitter
    /// produces `#[serde(try_from = "<Name>Raw", into = "<Name>Raw")]` on
    /// the main struct and a sibling `<Name>Raw` with `derive(Deserialize,
    /// Serialize)`. The hand-written `TryFrom`/`From` impls live in the
    /// crate that consumes the type and run runtime invariant checks.
    pub validate: bool,
}
```

- [ ] **Step 3: Parse the annotation in loader.rs**

In the function that builds a `StructDef` from a `serde_json::Value` (search for where existing `x-cairn-*` annotations on `$defs` are parsed), read the optional `x-cairn-validate` boolean and store it in the IR. Default `false` when absent.

```rust
// where existing annotations are read on a $defs entry
let validate = entry
    .get("x-cairn-validate")
    .and_then(|v| v.as_bool())
    .unwrap_or(false);
```

- [ ] **Step 4: Write a failing emitter unit test**

In `crates/cairn-idl/tests/` add (or extend) a snapshot test that renders a tiny synthetic schema with `x-cairn-validate: true` on a struct and asserts the generated Rust contains `#[serde(try_from = "FooRaw", into = "FooRaw")]` and a `pub struct FooRaw`.

```rust
// crates/cairn-idl/tests/codegen_validate_annotation.rs
use cairn_idl::codegen;

#[test]
fn validate_annotation_emits_try_from_and_raw_mirror() {
    let schema = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://cairn.dev/test/foo.json",
        "x-cairn-contract": "cairn.mcp.v1",
        "x-cairn-verb-id": "foo",
        "type": "object",
        "$defs": {
            "Args": { "type": "object", "additionalProperties": false, "properties": {} },
            "Data": {
                "type": "object",
                "additionalProperties": false,
                "x-cairn-validate": true,
                "required": ["x"],
                "properties": { "x": { "type": "string" } }
            }
        }
    });
    let out = codegen::emit_for_schema_value(&schema).expect("codegen ok");
    let assemble = out.iter().find(|f| f.path.ends_with("foo.rs")).expect("foo.rs");
    assert!(
        assemble.contents.contains("#[serde(try_from = \"FooDataRaw\", into = \"FooDataRaw\")]"),
        "missing try_from on validated struct; got:\n{}", assemble.contents
    );
    assert!(
        assemble.contents.contains("pub struct FooDataRaw"),
        "missing FooDataRaw mirror; got:\n{}", assemble.contents
    );
}
```

(If `codegen::emit_for_schema_value` does not exist, use whatever helper the existing codegen tests use — read `crates/cairn-idl/tests/codegen_emit_*.rs` for the pattern.)

Run: `cargo nextest run -p cairn-idl validate_annotation -- --nocapture`
Expected: FAIL — no `try_from` annotation emitted.

- [ ] **Step 5: Implement emitter changes in emit_sdk.rs**

In the function that emits a `StructDef` (the one near where `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]` is written), branch on `def.validate`:

```rust
// pseudo-diff in emit_sdk.rs at the struct-emission point
if def.validate {
    w.line(format!("#[serde(try_from = \"{name}Raw\", into = \"{name}Raw\")]", name = def.name));
    w.line("#[derive(Debug, Clone, PartialEq, Serialize)]");
} else {
    w.line("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]");
}
w.line("#[serde(deny_unknown_fields)]");
w.line(format!("pub struct {} {{", def.name));
// ... existing field emission ...
w.line("}");

if def.validate {
    // Emit Raw mirror with both Serialize and Deserialize, no try_from.
    w.blank();
    w.line(format!("/// Wire-shape mirror of [`{}`]; deserialization target before validation runs.", def.name));
    w.line("#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]");
    w.line("#[serde(deny_unknown_fields)]");
    w.line(format!("pub struct {}Raw {{", def.name));
    // emit identical fields (re-use the same field-emit helper)
    w.line("}");
}
```

The hand-written `TryFrom<Raw>` / `From<X> for Raw` impls are *not* emitted by codegen — they live in the consuming crate (Task 9 for `AssembleHotData`).

- [ ] **Step 6: Run the unit test and verify it passes**

Run: `cargo nextest run -p cairn-idl validate_annotation -- --nocapture`
Expected: PASS.

- [ ] **Step 7: Run the broader codegen test suite (insta + skill_compat)**

Run: `cargo nextest run -p cairn-idl`
Expected: All pass. If existing snapshots changed for unrelated structs (no `x-cairn-validate`), the codegen change is wrong — only validated structs should change. Investigate any insta diff with `cargo insta diff -p cairn-idl`.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-idl/
git commit -m "feat(idl): codegen support for x-cairn-validate annotation (#288)"
```

---

## Task 3: Run codegen and verify generated types

**Files:**
- Auto-modified by codegen across `cairn-core`, `cairn-mcp`, `cairn-cli`, `cairn-sdk`.

- [ ] **Step 1: Run the codegen**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked`
Expected: Exits 0, prints which files changed.

- [ ] **Step 2: Inspect the regenerated `assemble_hot.rs`**

Run: `cat crates/cairn-core/src/generated/verbs/assemble_hot.rs`
Expected: contains
  - `pub struct AssembleHotData` with `#[serde(try_from = "AssembleHotDataRaw", into = "AssembleHotDataRaw")]` and `#[derive(Debug, Clone, PartialEq, Serialize)]` (no Deserialize).
  - `pub struct AssembleHotDataRaw` with `#[derive(..., Serialize, Deserialize)]` and the same fields.
  - `pub struct HotSegment { step: HotRecipeStep, byte_start: u64, byte_end: u64, stability: SegmentStability, content_hash: String }`.
  - `pub enum HotRecipeStep { Purpose, Index, PinnedFeedback, TopSalienceProject, ActivePlaybook, RecentUserSignal }` with `#[serde(rename_all = "snake_case")]` and `#[non_exhaustive]`.
  - `pub enum SegmentStability { Stable1h, Stable5m, Volatile }` with same.
  - `segments: Option<Vec<HotSegment>>` field with `#[serde(default, skip_serializing_if = "Option::is_none")]`.

If any of those are missing, fix the codegen (Task 2) before proceeding.

- [ ] **Step 3: Run `cargo check` to confirm the workspace still compiles** (it will fail until Task 9 because `TryFrom<AssembleHotDataRaw>` is not yet implemented)

Run: `cargo check --workspace --locked`
Expected: FAIL with "the trait `TryFrom<AssembleHotDataRaw>` is not implemented for `AssembleHotData`" — this is expected; Task 9 implements it.

- [ ] **Step 4: Commit the regenerated artifacts**

```bash
git add crates/cairn-core/src/generated/ crates/cairn-mcp/src/generated/ crates/cairn-cli/src/generated/ crates/cairn-sdk/src/generated/
git commit -m "chore(generated): regenerate from assemble_hot schema (#288)"
```

---

## Task 4: `cairn-core` module skeleton + `sha2` dep

**Files:**
- Create: `crates/cairn-core/src/verbs/assemble_hot/mod.rs`
- Create: `crates/cairn-core/src/verbs/assemble_hot/segments.rs`
- Modify: `crates/cairn-core/src/verbs/mod.rs` (add `pub mod assemble_hot;`)
- Modify: `crates/cairn-core/Cargo.toml` (add `sha2 = { workspace = true }`)
- Modify: `Cargo.toml` workspace root (add `sha2 = { version = "0.10", default-features = false }` to `[workspace.dependencies]` if not already present)

- [ ] **Step 1: Add `sha2` to workspace deps**

Open `Cargo.toml` at the workspace root, in `[workspace.dependencies]`:
```toml
sha2 = { version = "0.10", default-features = false }
```
(Skip if already present — `grep "^sha2" Cargo.toml`.)

In `crates/cairn-core/Cargo.toml` under `[dependencies]`:
```toml
sha2 = { workspace = true }
```

- [ ] **Step 2: Verify `sha2` resolves**

Run: `cargo tree -e normal -p cairn-core --depth 1 | grep sha2`
Expected: one line showing `sha2 v0.10.x`.

- [ ] **Step 3: Create the module skeleton**

Create `crates/cairn-core/src/verbs/assemble_hot/mod.rs`:

```rust
//! `assemble_hot` verb: hot-memory recipe assembly with cache-breakpoint
//! segment markers. Brief §5, §7, §8.0.f. Issue #288.

pub mod assembler;
pub mod raw;
pub mod segments;

pub use assembler::{assemble_hot, AssembleHotError};
pub use segments::{
    build_segments, default_stability, validate, validate_base, validate_segments,
    validate_with_recipe, AssembleHotValidationError, MAX_SEGMENTS,
};

use crate::config::HotMemoryRecipeStep as ConfigStep;
use crate::generated::verbs::assemble_hot::HotRecipeStep as IdlStep;

/// Convert a config-side recipe step into the IDL-side wire enum.
/// Required because the assembler walks `HotMemoryConfig.recipe`
/// (config-side) and feeds steps into `build_segments` (IDL-side).
impl From<ConfigStep> for IdlStep {
    fn from(s: ConfigStep) -> Self {
        match s {
            ConfigStep::Purpose => IdlStep::Purpose,
            ConfigStep::Index => IdlStep::Index,
            ConfigStep::PinnedFeedback => IdlStep::PinnedFeedback,
            ConfigStep::TopSalienceProject => IdlStep::TopSalienceProject,
            ConfigStep::ActivePlaybook => IdlStep::ActivePlaybook,
            ConfigStep::RecentUserSignal => IdlStep::RecentUserSignal,
        }
    }
}

#[cfg(test)]
mod from_config_tests {
    use super::*;

    #[test]
    fn from_config_recipe_step_round_trips() {
        let pairs = [
            (ConfigStep::Purpose, IdlStep::Purpose),
            (ConfigStep::Index, IdlStep::Index),
            (ConfigStep::PinnedFeedback, IdlStep::PinnedFeedback),
            (ConfigStep::TopSalienceProject, IdlStep::TopSalienceProject),
            (ConfigStep::ActivePlaybook, IdlStep::ActivePlaybook),
            (ConfigStep::RecentUserSignal, IdlStep::RecentUserSignal),
        ];
        for (cfg, idl) in pairs {
            assert_eq!(IdlStep::from(cfg), idl);
        }
    }
}
```

(The placeholder modules `assembler` and `raw` are created later; for now create empty files so the module declaration compiles.)

Create `crates/cairn-core/src/verbs/assemble_hot/segments.rs` with just a placeholder line:
```rust
//! Pure helper functions for `assemble_hot` segments. Filled in by Task 5.
```

Create `crates/cairn-core/src/verbs/assemble_hot/raw.rs` with:
```rust
//! `AssembleHotDataRaw` ↔ `AssembleHotData` validation bridge. Filled in by Task 9.
```

Create `crates/cairn-core/src/verbs/assemble_hot/assembler.rs` with:
```rust
//! Stub-body `HotMemoryAssembler`. Filled in by Task 11.

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AssembleHotError {}

pub fn assemble_hot(
    _config: &crate::config::HotMemoryConfig,
) -> Result<crate::generated::verbs::assemble_hot::AssembleHotData, AssembleHotError> {
    unreachable!("filled in by Task 11")
}
```

- [ ] **Step 4: Wire the module into `verbs/mod.rs`**

In `crates/cairn-core/src/verbs/mod.rs`, add:
```rust
pub mod assemble_hot;
```

(Search the file for the existing `pub mod` declarations and add it in alphabetical order.)

- [ ] **Step 5: `cargo check`**

Run: `cargo check --workspace --locked`
Expected: still failing on `TryFrom<AssembleHotDataRaw>` (unchanged), but the new module compiles.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/cairn-core/Cargo.toml crates/cairn-core/src/verbs/
git commit -m "feat(core): assemble_hot module skeleton + sha2 dep + From config conversion (#288)"
```

---

## Task 5: `build_segments` helper + happy-path unit tests

**Files:**
- Modify: `crates/cairn-core/src/verbs/assemble_hot/segments.rs`

- [ ] **Step 1: Write the failing test for the empty-recipe case**

Replace `segments.rs` with:
```rust
//! Pure helper functions for `assemble_hot` segments. Brief §5, §7.

use crate::generated::verbs::assemble_hot::{HotRecipeStep, HotSegment, SegmentStability};

/// Hard upper bound on `segments.len()` at the wire boundary. Mirrors
/// the schema's `maxItems: 64`. Defends generic decoders against
/// amplification.
pub const MAX_SEGMENTS: usize = 64;

/// Default stability hint per recipe step. Constants, not config.
pub const fn default_stability(step: HotRecipeStep) -> SegmentStability {
    match step {
        HotRecipeStep::Purpose | HotRecipeStep::Index => SegmentStability::Stable1h,
        HotRecipeStep::PinnedFeedback
        | HotRecipeStep::TopSalienceProject
        | HotRecipeStep::ActivePlaybook => SegmentStability::Stable5m,
        HotRecipeStep::RecentUserSignal => SegmentStability::Volatile,
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssembleHotValidationError {
    #[error("recipe.len() {recipe} != bodies.len() {bodies}")]
    RecipeBodiesLenMismatch { recipe: usize, bodies: usize },
}

/// Build (`prefix`, `segments`) from a configured recipe and parallel
/// bodies. See §5 of the design spec for the full producer contract.
///
/// ```
/// # use cairn_core::verbs::assemble_hot::{build_segments, default_stability};
/// # use cairn_core::generated::verbs::assemble_hot::{HotRecipeStep, SegmentStability};
/// let recipe = [HotRecipeStep::Purpose, HotRecipeStep::RecentUserSignal];
/// let bodies = ["...", "..."];
/// let (_prefix, segments) = build_segments(&recipe, &bodies).unwrap();
/// for w in segments.windows(2) {
///     if (w[0].stability, w[1].stability)
///        == (SegmentStability::Stable1h, SegmentStability::Volatile)
///     {
///         // fictional: Cache::breakpoint(w[0].byte_end);
///     }
/// }
/// ```
pub fn build_segments(
    recipe: &[HotRecipeStep],
    bodies: &[&str],
) -> Result<(String, Vec<HotSegment>), AssembleHotValidationError> {
    if recipe.len() != bodies.len() {
        return Err(AssembleHotValidationError::RecipeBodiesLenMismatch {
            recipe: recipe.len(),
            bodies: bodies.len(),
        });
    }
    todo!("implementation in next step");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_segments_empty_recipe() {
        let (prefix, segments) = build_segments(&[], &[]).unwrap();
        assert_eq!(prefix, "");
        assert!(segments.is_empty());
    }
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo nextest run -p cairn-core build_segments_empty_recipe`
Expected: FAIL with `not yet implemented` panic from `todo!`.

- [ ] **Step 3: Implement `build_segments`**

Replace the `todo!` with:
```rust
use sha2::{Digest, Sha256};

let prefix: String = bodies.iter().copied().collect();
let mut segments = Vec::with_capacity(recipe.len());
let mut cursor: u64 = 0;
for (step, body) in recipe.iter().zip(bodies.iter()) {
    let start = cursor;
    let end = cursor + body.len() as u64;
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    let content_hash = format!("{:x}", hasher.finalize());
    segments.push(HotSegment {
        step: *step,
        byte_start: start,
        byte_end: end,
        stability: default_stability(*step),
        content_hash,
    });
    cursor = end;
}
Ok((prefix, segments))
```

(Replace the `todo!()` line; `cursor` advances by `body.len()` to keep ranges contiguous in UTF-8 bytes.)

- [ ] **Step 4: Run and verify pass**

Run: `cargo nextest run -p cairn-core build_segments_empty_recipe`
Expected: PASS.

- [ ] **Step 5: Add the remaining happy-path unit tests**

Append to the `tests` module:
```rust
    use crate::generated::verbs::assemble_hot::HotRecipeStep::*;
    use crate::generated::verbs::assemble_hot::SegmentStability::*;

    #[test]
    fn default_stability_matches_brief() {
        assert_eq!(default_stability(Purpose), Stable1h);
        assert_eq!(default_stability(Index), Stable1h);
        assert_eq!(default_stability(PinnedFeedback), Stable5m);
        assert_eq!(default_stability(TopSalienceProject), Stable5m);
        assert_eq!(default_stability(ActivePlaybook), Stable5m);
        assert_eq!(default_stability(RecentUserSignal), Volatile);
    }

    #[test]
    fn build_segments_all_empty_bodies() {
        let recipe = [Purpose, Index, PinnedFeedback, TopSalienceProject, ActivePlaybook, RecentUserSignal];
        let bodies = ["", "", "", "", "", ""];
        let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
        assert_eq!(prefix, "");
        assert_eq!(segments.len(), 6);
        for s in &segments {
            assert_eq!(s.byte_start, 0);
            assert_eq!(s.byte_end, 0);
        }
    }

    #[test]
    fn build_segments_rejects_len_mismatch() {
        let err = build_segments(&[Purpose, Index], &["a"]).unwrap_err();
        assert_eq!(err, AssembleHotValidationError::RecipeBodiesLenMismatch { recipe: 2, bodies: 1 });
    }

    #[test]
    fn build_segments_aligns_steps_to_recipe() {
        let recipe = [Purpose, Index, RecentUserSignal];
        let bodies = ["p", "i", "r"];
        let (_, segments) = build_segments(&recipe, &bodies).unwrap();
        for (i, expected) in recipe.iter().enumerate() {
            assert_eq!(segments[i].step, *expected);
        }
    }

    #[test]
    fn build_segments_single_slot() {
        let (prefix, segments) = build_segments(&[Purpose], &["hello"]).unwrap();
        assert_eq!(prefix, "hello");
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].byte_start, 0);
        assert_eq!(segments[0].byte_end, 5);
    }

    #[test]
    fn content_hash_matches_segment_bytes() {
        let recipe = [Purpose, Index];
        let bodies = ["alpha", "beta"];
        let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
        for s in &segments {
            let slice = &prefix[s.byte_start as usize..s.byte_end as usize];
            let mut h = Sha256::new();
            h.update(slice.as_bytes());
            assert_eq!(s.content_hash, format!("{:x}", h.finalize()));
        }
    }

    #[test]
    fn build_segments_handles_non_ascii() {
        let body = "héllo·世界";
        let (prefix, segments) = build_segments(&[Purpose], &[body]).unwrap();
        let s = &segments[0];
        assert_eq!(s.byte_end - s.byte_start, body.len() as u64);
        assert_eq!(&prefix[s.byte_start as usize..s.byte_end as usize], body);
        let mut h = Sha256::new();
        h.update(body.as_bytes());
        assert_eq!(s.content_hash, format!("{:x}", h.finalize()));
    }
```

- [ ] **Step 6: Run all tests in the module**

Run: `cargo nextest run -p cairn-core verbs::assemble_hot::segments`
Expected: 7 PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/segments.rs
git commit -m "feat(core): build_segments helper + happy-path unit tests (#288)"
```

---

## Task 6: Validators + the full error enum

**Files:**
- Modify: `crates/cairn-core/src/verbs/assemble_hot/segments.rs`

- [ ] **Step 1: Expand the error enum**

Replace the existing `AssembleHotValidationError` with the full set:
```rust
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssembleHotValidationError {
    #[error("recipe.len() {recipe} != bodies.len() {bodies}")]
    RecipeBodiesLenMismatch { recipe: usize, bodies: usize },
    #[error("segment {index}: byte_start {start} > byte_end {end}")]
    DescendingRange { index: usize, start: u64, end: u64 },
    #[error("segment {index}: byte_start {start} != previous byte_end {prev_end}")]
    NonContiguous { index: usize, start: u64, prev_end: u64 },
    #[error("segments[0].byte_start {start} != 0")]
    DoesNotStartAtZero { start: u64 },
    #[error("segments.last().byte_end {end} != prefix.len() {prefix_len}")]
    DoesNotCoverPrefix { end: u64, prefix_len: u64 },
    #[error("data.bytes {bytes} != prefix.len() {prefix_len}")]
    BytesMismatch { bytes: u64, prefix_len: u64 },
    #[error("segment {index}: content_hash mismatch")]
    HashMismatch { index: usize },
    #[error("segment {index}: byte_end {end} > prefix.len() {prefix_len}")]
    OutOfBounds { index: usize, end: u64, prefix_len: u64 },
    #[error("segment {index}: expected step {expected:?}, got {got:?}")]
    StepMismatch { index: usize, expected: HotRecipeStep, got: HotRecipeStep },
    #[error("segments.len() {got} != expected recipe.len() {expected}")]
    RecipeLenMismatch { expected: usize, got: usize },
    #[error("legacy producer did not emit segments; cannot validate against recipe")]
    LegacyProducerSegmentsAbsent,
    #[error("Some(vec![]) requires prefix == \"\" and bytes == 0, got bytes {bytes}, prefix.len() {prefix_len}")]
    EmptySegmentsRequiresEmptyPrefix { bytes: u64, prefix_len: u64 },
    #[error("segment {index}: stability {got:?} does not match default for step {step:?} ({expected:?})")]
    StabilityMismatch { index: usize, step: HotRecipeStep, got: SegmentStability, expected: SegmentStability },
    #[error("segments.len() {got} exceeds maximum {max}")]
    TooManySegments { got: usize, max: usize },
}
```

- [ ] **Step 2: Add the validator functions**

Append to `segments.rs` (above the `#[cfg(test)] mod tests`):
```rust
use crate::generated::verbs::assemble_hot::AssembleHotData;

/// Wire invariants of `AssembleHotData` independent of `segments`.
pub fn validate_base(data: &AssembleHotData) -> Result<(), AssembleHotValidationError> {
    let prefix_len = data.prefix.len() as u64;
    if data.bytes != prefix_len {
        return Err(AssembleHotValidationError::BytesMismatch {
            bytes: data.bytes,
            prefix_len,
        });
    }
    Ok(())
}

/// Segment-layer invariants. See §5 of the design spec for the full
/// table; comments below pair each check to its error variant.
pub fn validate_segments(data: &AssembleHotData) -> Result<(), AssembleHotValidationError> {
    let segments = match &data.segments {
        None => return Ok(()),
        Some(v) => v,
    };
    let prefix_len = data.prefix.len() as u64;

    if segments.is_empty() {
        if !data.prefix.is_empty() || data.bytes != 0 {
            return Err(AssembleHotValidationError::EmptySegmentsRequiresEmptyPrefix {
                bytes: data.bytes,
                prefix_len,
            });
        }
        return Ok(());
    }

    if segments.len() > MAX_SEGMENTS {
        return Err(AssembleHotValidationError::TooManySegments {
            got: segments.len(),
            max: MAX_SEGMENTS,
        });
    }

    if segments[0].byte_start != 0 {
        return Err(AssembleHotValidationError::DoesNotStartAtZero { start: segments[0].byte_start });
    }

    let mut prev_end: u64 = 0;
    for (i, s) in segments.iter().enumerate() {
        if s.byte_start > s.byte_end {
            return Err(AssembleHotValidationError::DescendingRange {
                index: i,
                start: s.byte_start,
                end: s.byte_end,
            });
        }
        if i > 0 && s.byte_start != prev_end {
            return Err(AssembleHotValidationError::NonContiguous {
                index: i,
                start: s.byte_start,
                prev_end,
            });
        }
        if s.byte_end > prefix_len {
            return Err(AssembleHotValidationError::OutOfBounds {
                index: i,
                end: s.byte_end,
                prefix_len,
            });
        }
        let expected_stability = default_stability(s.step);
        if s.stability != expected_stability {
            return Err(AssembleHotValidationError::StabilityMismatch {
                index: i,
                step: s.step,
                got: s.stability,
                expected: expected_stability,
            });
        }
        let slice = &data.prefix[s.byte_start as usize..s.byte_end as usize];
        let mut h = sha2::Sha256::new();
        h.update(slice.as_bytes());
        let actual_hash = format!("{:x}", h.finalize());
        if s.content_hash != actual_hash {
            return Err(AssembleHotValidationError::HashMismatch { index: i });
        }
        prev_end = s.byte_end;
    }

    if prev_end != prefix_len {
        return Err(AssembleHotValidationError::DoesNotCoverPrefix {
            end: prev_end,
            prefix_len,
        });
    }
    Ok(())
}

/// Convenience: `validate_base` then `validate_segments`.
pub fn validate(data: &AssembleHotData) -> Result<(), AssembleHotValidationError> {
    validate_base(data)?;
    validate_segments(data)?;
    Ok(())
}

/// Like `validate`, plus recipe-alignment assertions.
pub fn validate_with_recipe(
    data: &AssembleHotData,
    expected: &[HotRecipeStep],
) -> Result<(), AssembleHotValidationError> {
    validate(data)?;
    let segments = data
        .segments
        .as_ref()
        .ok_or(AssembleHotValidationError::LegacyProducerSegmentsAbsent)?;
    if segments.len() != expected.len() {
        return Err(AssembleHotValidationError::RecipeLenMismatch {
            expected: expected.len(),
            got: segments.len(),
        });
    }
    for (i, (s, exp)) in segments.iter().zip(expected.iter()).enumerate() {
        if s.step != *exp {
            return Err(AssembleHotValidationError::StepMismatch {
                index: i,
                expected: *exp,
                got: s.step,
            });
        }
    }
    Ok(())
}
```

- [ ] **Step 3: `cargo check`**

Run: `cargo check -p cairn-core --locked`
Expected: PASS (still fails on `TryFrom<Raw>` until Task 9; if you scope `cargo check -p cairn-core --no-deps`, that subset passes; otherwise the workspace check still fails — that's fine for now).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/segments.rs
git commit -m "feat(core): validators for AssembleHotData + full error enum (#288)"
```

---

## Task 7: Validator unit tests (one per error variant)

**Files:**
- Modify: `crates/cairn-core/src/verbs/assemble_hot/segments.rs` (extend `mod tests`)

- [ ] **Step 1: Add a fixture helper inside `mod tests`**

```rust
    fn well_formed_data() -> AssembleHotData {
        let recipe = [Purpose, Index];
        let bodies = ["alpha", "beta"];
        let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
        AssembleHotData {
            bytes: prefix.len() as u64,
            prefix,
            segments: Some(segments),
        }
    }
```

- [ ] **Step 2: Add validator tests (paste, do not skip cases)**

```rust
    #[test]
    fn validate_base_accepts_well_formed() {
        assert!(validate_base(&well_formed_data()).is_ok());
    }

    #[test]
    fn validate_base_rejects_bytes_mismatch() {
        let mut d = well_formed_data();
        d.bytes = 999;
        assert!(matches!(
            validate_base(&d),
            Err(AssembleHotValidationError::BytesMismatch { bytes: 999, .. })
        ));
    }

    #[test]
    fn validate_segments_accepts_canonical_empty() {
        let d = AssembleHotData { bytes: 0, prefix: String::new(), segments: Some(vec![]) };
        assert!(validate_segments(&d).is_ok());
    }

    #[test]
    fn validate_segments_rejects_empty_with_non_empty_prefix() {
        let d = AssembleHotData { bytes: 3, prefix: "abc".into(), segments: Some(vec![]) };
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::EmptySegmentsRequiresEmptyPrefix { bytes: 3, prefix_len: 3 })
        ));
    }

    #[test]
    fn validate_segments_accepts_none() {
        let d = AssembleHotData { bytes: 0, prefix: String::new(), segments: None };
        assert!(validate_segments(&d).is_ok());
    }

    #[test]
    fn validate_segments_rejects_descending_range() {
        let mut d = well_formed_data();
        let segs = d.segments.as_mut().unwrap();
        segs[0].byte_start = 10;
        segs[0].byte_end = 5;
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::DescendingRange { .. })
        ));
    }

    #[test]
    fn validate_segments_rejects_non_contiguous() {
        let mut d = well_formed_data();
        let segs = d.segments.as_mut().unwrap();
        segs[1].byte_start += 1;
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::NonContiguous { .. })
        ));
    }

    #[test]
    fn validate_segments_rejects_does_not_start_at_zero() {
        let mut d = well_formed_data();
        d.segments.as_mut().unwrap()[0].byte_start = 1;
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::DoesNotStartAtZero { start: 1 })
        ));
    }

    #[test]
    fn validate_segments_rejects_does_not_cover_prefix() {
        let mut d = well_formed_data();
        let segs = d.segments.as_mut().unwrap();
        let last = segs.last_mut().unwrap();
        last.byte_end -= 1;
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::DoesNotCoverPrefix { .. })
        ));
    }

    #[test]
    fn validate_segments_rejects_hash_mismatch() {
        let mut d = well_formed_data();
        let segs = d.segments.as_mut().unwrap();
        segs[0].content_hash = "0".repeat(64);
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::HashMismatch { index: 0 })
        ));
    }

    #[test]
    fn validate_segments_rejects_out_of_bounds() {
        let mut d = well_formed_data();
        let segs = d.segments.as_mut().unwrap();
        let last = segs.last_mut().unwrap();
        last.byte_end = (d.prefix.len() as u64) + 100;
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::OutOfBounds { .. })
        ));
    }

    #[test]
    fn validate_segments_rejects_stability_mismatch() {
        let mut d = well_formed_data();
        d.segments.as_mut().unwrap()[0].stability = Volatile; // Purpose should be Stable1h
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::StabilityMismatch { index: 0, .. })
        ));
    }

    #[test]
    fn validate_segments_rejects_too_many() {
        // Build a payload with 65 zero-length segments. Use a 65-step
        // synthetic recipe that violates MAX_SEGMENTS.
        let recipe: Vec<HotRecipeStep> = std::iter::repeat(Purpose).take(65).collect();
        let bodies: Vec<&str> = std::iter::repeat("").take(65).collect();
        let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
        let d = AssembleHotData { bytes: prefix.len() as u64, prefix, segments: Some(segments) };
        assert!(matches!(
            validate_segments(&d),
            Err(AssembleHotValidationError::TooManySegments { got: 65, max: 64 })
        ));
    }

    #[test]
    fn validate_with_recipe_rejects_step_mismatch() {
        let d = well_formed_data();
        let expected = [Purpose, RecentUserSignal];
        assert!(matches!(
            validate_with_recipe(&d, &expected),
            Err(AssembleHotValidationError::StepMismatch { index: 1, .. })
        ));
    }

    #[test]
    fn validate_with_recipe_rejects_recipe_len_mismatch() {
        let d = well_formed_data();
        assert!(matches!(
            validate_with_recipe(&d, &[Purpose]),
            Err(AssembleHotValidationError::RecipeLenMismatch { expected: 1, got: 2 })
        ));
    }

    #[test]
    fn validate_with_recipe_rejects_legacy_absent() {
        let d = AssembleHotData { bytes: 0, prefix: String::new(), segments: None };
        assert!(matches!(
            validate_with_recipe(&d, &[]),
            Err(AssembleHotValidationError::LegacyProducerSegmentsAbsent)
        ));
    }

    #[test]
    fn build_segments_output_validates() {
        let recipe = [Purpose, Index, RecentUserSignal];
        let bodies = ["a", "bb", "ccc"];
        let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
        let d = AssembleHotData { bytes: prefix.len() as u64, prefix, segments: Some(segments) };
        validate(&d).unwrap();
        validate_with_recipe(&d, &recipe).unwrap();
    }
```

- [ ] **Step 3: Run all validator tests**

Run: `cargo nextest run -p cairn-core verbs::assemble_hot::segments::tests`
Expected: All PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/segments.rs
git commit -m "test(core): per-variant validator unit tests (#288)"
```

---

## Task 8: Property tests (`proptest`)

**Files:**
- Modify: `crates/cairn-core/src/verbs/assemble_hot/segments.rs` (extend `mod tests`)
- Modify: `crates/cairn-core/Cargo.toml` (ensure `proptest` is a dev-dep; check first with `grep proptest crates/cairn-core/Cargo.toml`).

- [ ] **Step 1: Add `proptest` if missing**

If `proptest` is not yet listed under `[dev-dependencies]`, add `proptest = { workspace = true }` (the workspace root already has `proptest` per the IDL crate; if not, add to workspace deps and the cairn-core dev-deps).

- [ ] **Step 2: Add property tests**

Append to `mod tests`:
```rust
    use proptest::prelude::*;

    fn step_strategy() -> impl Strategy<Value = HotRecipeStep> {
        prop_oneof![
            Just(Purpose),
            Just(Index),
            Just(PinnedFeedback),
            Just(TopSalienceProject),
            Just(ActivePlaybook),
            Just(RecentUserSignal),
        ]
    }

    proptest! {
        #[test]
        fn coverage_invariant(chunks in proptest::collection::vec((step_strategy(), ".{0,32}"), 0..16)) {
            let recipe: Vec<HotRecipeStep> = chunks.iter().map(|(s, _)| *s).collect();
            let bodies: Vec<&str> = chunks.iter().map(|(_, b)| b.as_str()).collect();
            let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
            if segments.is_empty() {
                prop_assert_eq!(prefix.len(), 0);
            } else {
                prop_assert_eq!(segments[0].byte_start, 0);
                prop_assert_eq!(segments.last().unwrap().byte_end, prefix.len() as u64);
                for w in segments.windows(2) {
                    prop_assert_eq!(w[0].byte_end, w[1].byte_start);
                }
            }
        }

        #[test]
        fn hash_stability(chunks in proptest::collection::vec((step_strategy(), ".{0,32}"), 0..16)) {
            let recipe: Vec<HotRecipeStep> = chunks.iter().map(|(s, _)| *s).collect();
            let bodies: Vec<&str> = chunks.iter().map(|(_, b)| b.as_str()).collect();
            let a = build_segments(&recipe, &bodies).unwrap();
            let b = build_segments(&recipe, &bodies).unwrap();
            prop_assert_eq!(a, b);
        }

        #[test]
        fn utf8_boundary_safety(chunks in proptest::collection::vec((step_strategy(), ".{0,32}"), 0..16)) {
            let recipe: Vec<HotRecipeStep> = chunks.iter().map(|(s, _)| *s).collect();
            let bodies: Vec<&str> = chunks.iter().map(|(_, b)| b.as_str()).collect();
            let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
            for s in &segments {
                let _ = &prefix[s.byte_start as usize..s.byte_end as usize]; // panics on bad UTF-8 boundary
            }
        }
    }
```

- [ ] **Step 3: Run property tests**

Run: `cargo nextest run -p cairn-core verbs::assemble_hot::segments::tests::coverage_invariant verbs::assemble_hot::segments::tests::hash_stability verbs::assemble_hot::segments::tests::utf8_boundary_safety`
Expected: PASS (each runs 256 cases by default).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/Cargo.toml crates/cairn-core/src/verbs/assemble_hot/segments.rs
git commit -m "test(core): proptest coverage + hash stability + utf8 safety (#288)"
```

---

## Task 9: `AssembleHotDataRaw` ↔ `AssembleHotData` validation bridge

**Files:**
- Modify: `crates/cairn-core/src/verbs/assemble_hot/raw.rs`

- [ ] **Step 1: Implement the bridge**

Replace the placeholder in `raw.rs` with:
```rust
//! `AssembleHotDataRaw` ↔ `AssembleHotData` validation bridge.
//!
//! `AssembleHotData` is generated with
//! `#[serde(try_from = "AssembleHotDataRaw", into = "AssembleHotDataRaw")]`,
//! so every deserialize path runs `TryFrom<Raw> for AssembleHotData`,
//! which calls [`validate_base`] and [`validate_segments`]. Bypass is
//! impossible — see §5/§8 of the design spec.

use crate::generated::verbs::assemble_hot::{AssembleHotData, AssembleHotDataRaw};
use super::segments::{validate_base, validate_segments, AssembleHotValidationError};

impl TryFrom<AssembleHotDataRaw> for AssembleHotData {
    type Error = AssembleHotValidationError;

    fn try_from(raw: AssembleHotDataRaw) -> Result<Self, Self::Error> {
        // Reconstruct the validated type from raw fields *without*
        // recursing into Deserialize (which would try_from again).
        let data = AssembleHotData {
            bytes: raw.bytes,
            prefix: raw.prefix,
            segments: raw.segments,
        };
        validate_base(&data)?;
        validate_segments(&data)?;
        Ok(data)
    }
}

impl From<AssembleHotData> for AssembleHotDataRaw {
    fn from(data: AssembleHotData) -> Self {
        AssembleHotDataRaw {
            bytes: data.bytes,
            prefix: data.prefix,
            segments: data.segments,
        }
    }
}
```

If the codegen named the Raw struct's fields differently, adjust the field names. Check by reading `crates/cairn-core/src/generated/verbs/assemble_hot.rs`.

- [ ] **Step 2: `cargo check` for the workspace**

Run: `cargo check --workspace --locked`
Expected: PASS now that `TryFrom<AssembleHotDataRaw>` is implemented.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/raw.rs
git commit -m "feat(core): AssembleHotData try_from validation bridge (#288)"
```

---

## Task 10: Trust-boundary integration tests

**Files:**
- Create: `crates/cairn-core/tests/assemble_hot_envelope.rs`

- [ ] **Step 1: Write the integration tests**

```rust
//! Trust-boundary integration tests for `assemble_hot`. Pin the
//! invariant that validation runs inside `Deserialize` itself, so
//! every code path (envelope decode, direct `serde_json::from_str`,
//! MCP, SDK, tests) cannot bypass it.

use cairn_core::generated::verbs::assemble_hot::AssembleHotData;
use cairn_core::verbs::assemble_hot::AssembleHotValidationError;

#[test]
fn envelope_decode_rejects_malformed_bytes() {
    // Legacy-shape payload (no segments) with bytes != prefix.len().
    let json = r#"{"bytes": 5, "prefix": "abc"}"#;
    let err = serde_json::from_str::<AssembleHotData>(json).unwrap_err();
    assert!(err.to_string().contains("BytesMismatch") || err.to_string().contains("bytes"));
}

#[test]
fn envelope_decode_accepts_legacy_well_formed() {
    let json = r#"{"bytes": 3, "prefix": "abc"}"#;
    let data: AssembleHotData = serde_json::from_str(json).unwrap();
    assert_eq!(data.segments, None);
}

#[test]
fn envelope_decode_round_trips_canonical_empty() {
    let json = r#"{"bytes": 0, "prefix": "", "segments": []}"#;
    let data: AssembleHotData = serde_json::from_str(json).unwrap();
    assert_eq!(data.segments, Some(vec![]));
    let re = serde_json::to_string(&data).unwrap();
    assert!(re.contains("\"segments\":[]"));
}

#[test]
fn envelope_decode_rejects_empty_segments_with_non_empty_prefix() {
    let json = r#"{"bytes": 3, "prefix": "abc", "segments": []}"#;
    let err = serde_json::from_str::<AssembleHotData>(json).unwrap_err();
    assert!(err.to_string().contains("EmptySegmentsRequiresEmptyPrefix") || err.to_string().to_lowercase().contains("empty"));
}

#[test]
fn envelope_decode_rejects_too_many_segments() {
    // 65 zero-length segments; should fail TooManySegments.
    let mut segments = String::from("[");
    for i in 0..65 {
        if i > 0 { segments.push(','); }
        segments.push_str(r#"{"step":"purpose","byte_start":0,"byte_end":0,"stability":"stable_1h","content_hash":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}"#);
    }
    segments.push(']');
    let json = format!(r#"{{"bytes": 0, "prefix": "", "segments": {}}}"#, segments);
    let err = serde_json::from_str::<AssembleHotData>(&json).unwrap_err();
    assert!(err.to_string().contains("TooManySegments") || err.to_string().contains("64"));
}

#[test]
fn envelope_decode_rejects_stability_mismatch() {
    // Purpose with stability volatile (should be stable_1h).
    let json = r#"{
        "bytes": 0,
        "prefix": "",
        "segments": [{
            "step": "purpose",
            "byte_start": 0,
            "byte_end": 0,
            "stability": "volatile",
            "content_hash": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        }]
    }"#;
    let err = serde_json::from_str::<AssembleHotData>(json).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("stability"));
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo nextest run -p cairn-core --test assemble_hot_envelope`
Expected: All PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/tests/assemble_hot_envelope.rs
git commit -m "test(core): trust-boundary integration tests for assemble_hot (#288)"
```

---

## Task 11: Stub-body assembler

**Files:**
- Modify: `crates/cairn-core/src/verbs/assemble_hot/assembler.rs`

- [ ] **Step 1: Replace the placeholder with a real implementation**

```rust
//! Stub-body `HotMemoryAssembler`. Walks `HotMemoryConfig.recipe`,
//! calls a stub `load_step_body` that returns `""` for every step, and
//! returns a fully validated `AssembleHotData`. Real source loading is
//! the missing-half of issue #193 — that PR replaces `load_step_body`
//! and changes nothing else.

use crate::config::HotMemoryConfig;
use crate::generated::verbs::assemble_hot::{AssembleHotData, HotRecipeStep};
use super::segments::{build_segments, AssembleHotValidationError};

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AssembleHotError {
    #[error("segment construction: {0}")]
    Segments(#[from] AssembleHotValidationError),
}

/// Run the hot-memory recipe and return a validated `AssembleHotData`.
pub fn assemble_hot(config: &HotMemoryConfig) -> Result<AssembleHotData, AssembleHotError> {
    let recipe: Vec<HotRecipeStep> = config.recipe.iter().copied().map(HotRecipeStep::from).collect();
    let bodies: Vec<String> = recipe.iter().copied().map(load_step_body).collect();
    let bodies_refs: Vec<&str> = bodies.iter().map(String::as_str).collect();
    let (prefix, segments) = build_segments(&recipe, &bodies_refs)?;
    Ok(AssembleHotData {
        bytes: prefix.len() as u64,
        prefix,
        segments: Some(segments),
    })
}

/// Load the body for one recipe step. Stub: always `""`. The
/// missing-half of #193 replaces this single function with the real
/// SQLite + markdown loader; nothing else here changes.
fn load_step_body(_step: HotRecipeStep) -> String {
    String::new()
}
```

If `HotMemoryConfig` exposes its recipe under a different field name, adjust (`config.recipe` is the spec text; verify against `crates/cairn-core/src/config/mod.rs:495+`).

- [ ] **Step 2: Add unit tests**

Append to `assembler.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotMemoryConfig;
    use crate::generated::verbs::assemble_hot::{HotRecipeStep, SegmentStability};

    #[test]
    fn assemble_hot_default_config_returns_six_zero_length_segments() {
        let cfg = HotMemoryConfig::default();
        let data = assemble_hot(&cfg).unwrap();
        assert_eq!(data.prefix, "");
        assert_eq!(data.bytes, 0);
        let segments = data.segments.expect("segments emitted");
        assert_eq!(segments.len(), cfg.recipe.len());
        for s in &segments {
            assert_eq!(s.byte_start, 0);
            assert_eq!(s.byte_end, 0);
        }
    }

    #[test]
    fn assemble_hot_empty_recipe() {
        let mut cfg = HotMemoryConfig::default();
        cfg.recipe.clear();
        let data = assemble_hot(&cfg).unwrap();
        assert_eq!(data.prefix, "");
        assert_eq!(data.segments, Some(vec![]));
    }

    #[test]
    fn assemble_hot_output_round_trips_through_deserialize() {
        let cfg = HotMemoryConfig::default();
        let data = assemble_hot(&cfg).unwrap();
        let json = serde_json::to_string(&data).unwrap();
        let back: crate::generated::verbs::assemble_hot::AssembleHotData = serde_json::from_str(&json).unwrap();
        assert_eq!(back, data);
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo nextest run -p cairn-core verbs::assemble_hot::assembler`
Expected: 3 PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/assembler.rs
git commit -m "feat(core): stub-body assemble_hot() over HotMemoryConfig.recipe (#288)"
```

---

## Task 12: Insta snapshot test

**Files:**
- Create: `crates/cairn-core/tests/assemble_hot_snapshots.rs`

- [ ] **Step 1: Write the snapshot test**

```rust
//! Snapshot the canonical JSON shape of `AssembleHotData` for a
//! deterministic fixture. The `.snap` file is the byte-stability
//! acceptance criterion for issue #288.

use cairn_core::generated::verbs::assemble_hot::AssembleHotData;
use cairn_core::verbs::assemble_hot::build_segments;
use cairn_core::generated::verbs::assemble_hot::HotRecipeStep::*;

#[test]
fn assemble_hot_data_canonical_json() {
    let recipe = [Purpose, Index, PinnedFeedback, TopSalienceProject, ActivePlaybook, RecentUserSignal];
    let bodies = ["purpose body\n", "index body\n", "pinned\n", "salience\n", "playbook\n", "signal\n"];
    let (prefix, segments) = build_segments(&recipe, &bodies).unwrap();
    let data = AssembleHotData { bytes: prefix.len() as u64, prefix, segments: Some(segments) };
    let json = serde_json::to_string_pretty(&data).unwrap();
    insta::assert_snapshot!(json);
}
```

- [ ] **Step 2: Run, then accept the snapshot**

Run: `cargo nextest run -p cairn-core --test assemble_hot_snapshots`
Expected: FAIL (snapshot missing).

Then: `cargo insta review` and accept. Or `INSTA_UPDATE=auto cargo nextest run -p cairn-core --test assemble_hot_snapshots` for non-interactive accept.

Verify `crates/cairn-core/tests/snapshots/assemble_hot_snapshots__assemble_hot_data_canonical_json.snap` exists and contains the expected six-segment JSON.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/tests/assemble_hot_snapshots.rs crates/cairn-core/tests/snapshots/
git commit -m "test(core): insta snapshot of canonical AssembleHotData JSON (#288)"
```

---

## Task 13: Wire `cairn-cli` verb

**Files:**
- Modify: `crates/cairn-cli/src/verbs/assemble_hot.rs`

- [ ] **Step 1: Read the existing handler and the `CliContext` shape**

Run: `cat crates/cairn-cli/src/verbs/assemble_hot.rs` and `grep -n "pub struct CliContext\|struct Context\|fn run.*ArgMatches" crates/cairn-cli/src/ -r | head -10`. The handler currently returns `unimplemented_response`; you need to replace that with a call to `cairn_core::verbs::assemble_hot::assemble_hot(&ctx.config.vault.hot_memory)`.

- [ ] **Step 2: Replace the handler**

```rust
//! `cairn assemble_hot` handler.

use std::process::ExitCode;

use cairn_core::generated::envelope::{ResponseData, ResponseVerb};
use cairn_core::verbs::assemble_hot;
use clap::ArgMatches;

use crate::context::CliContext;
use super::envelope::{emit_json, emit_human, error_response, human_error};

#[must_use]
pub fn run(sub: &ArgMatches, ctx: &CliContext) -> ExitCode {
    let json = sub.get_flag("json");
    match assemble_hot::assemble_hot(&ctx.config.vault.hot_memory) {
        Ok(data) => {
            let resp = crate::verbs::envelope::ok_response(
                ResponseVerb::AssembleHot,
                ResponseData::AssembleHot(data),
            );
            if json { emit_json(&resp); } else { emit_human(&resp); }
            ExitCode::SUCCESS
        }
        Err(e) => {
            let resp = error_response(ResponseVerb::AssembleHot, &e.to_string());
            if json { emit_json(&resp); } else {
                human_error("assemble_hot", "Internal", &e.to_string(), &resp.operation_id);
            }
            ExitCode::FAILURE
        }
    }
}
```

If the existing helper names differ (`ok_response`, `error_response`, `emit_human`), use whatever the other verbs use — open `crates/cairn-cli/src/verbs/search.rs` or another wired verb as a reference; the goal is consistency with sibling verbs, not invention.

If `run` does not currently take a `&CliContext`, follow the dispatch pattern used by other wired verbs to plumb it in (the dispatcher in `crates/cairn-cli/src/main.rs` constructs the context).

- [ ] **Step 3: Build and run a smoke check**

Run:
```bash
cargo build -p cairn-cli --locked
./target/debug/cairn assemble_hot --json
```
Expected: a JSON envelope with `data.segments` containing six zero-length entries (one per default recipe step). Exit code 0.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/verbs/assemble_hot.rs
git commit -m "feat(cli): wire cairn assemble_hot to stub-body assembler (#288)"
```

---

## Task 14: CLI end-to-end snapshot test

**Files:**
- Create: `crates/cairn-cli/tests/cli_assemble_hot.rs`

- [ ] **Step 1: Write the test**

```rust
//! End-to-end CLI snapshot for `cairn assemble_hot --json`. Exercises
//! the binary against a tempfile vault to lock the wire shape.

use std::process::Command;

#[test]
fn cairn_assemble_hot_json_emits_segments() {
    let vault = tempfile::tempdir().expect("tempdir");
    // Minimum vault: just initialise it (or skip if assemble_hot does
    // not require a vault on disk in P0). Follow the pattern in
    // crates/cairn-cli/tests/cli.rs for vault setup.
    // ...

    let exe = env!("CARGO_BIN_EXE_cairn");
    let output = Command::new(exe)
        .arg("--vault").arg(vault.path())
        .arg("assemble_hot")
        .arg("--json")
        .output()
        .expect("run cairn");
    assert!(output.status.success(), "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));

    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    let segments = value.pointer("/data/segments").expect("segments present");
    assert!(segments.is_array(), "segments should be array, got {}", segments);
    let arr = segments.as_array().unwrap();
    assert_eq!(arr.len(), 6, "default recipe has 6 steps");

    // Keep the snapshot deterministic: redact volatile fields like
    // operation_id before snapshotting if the envelope contains them.
    let mut redacted = value.clone();
    if let Some(op) = redacted.pointer_mut("/operation_id") {
        *op = serde_json::json!("<redacted>");
    }
    insta::assert_json_snapshot!(redacted);
}
```

If your project's existing CLI tests use a different vault-setup helper, follow it. If `--vault` is not the actual flag, use `--config` or whatever the other CLI tests use. Reading `crates/cairn-cli/tests/cli.rs` is how you find the right pattern.

- [ ] **Step 2: Run and accept snapshot**

Run: `cargo nextest run -p cairn-cli --test cli_assemble_hot`
Expected: FAIL on first run (no snapshot).

Then `cargo insta review` and accept.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/tests/cli_assemble_hot.rs crates/cairn-cli/tests/snapshots/
git commit -m "test(cli): end-to-end assemble_hot --json snapshot (#288)"
```

---

## Task 15: Wire `cairn-sdk` transport + round-trip test

**Files:**
- Modify: `crates/cairn-sdk/src/transport.rs`
- Modify: `crates/cairn-sdk/tests/surface.rs`

- [ ] **Step 1: Find the SDK's assemble_hot transport stub**

Run: `grep -n "assemble_hot\|AssembleHot\|unimplemented" crates/cairn-sdk/src/transport.rs | head`

Replace the stub with a real call to `cairn_core::verbs::assemble_hot::assemble_hot(...)`. The exact shape mirrors how other wired verbs work in the same file.

- [ ] **Step 2: Add an SDK round-trip test**

Append to `crates/cairn-sdk/tests/surface.rs`:
```rust
#[test]
fn sdk_assemble_hot_returns_typed_segments() {
    let sdk = cairn_sdk::Cairn::in_memory().expect("sdk");
    let resp = sdk.assemble_hot(Default::default()).expect("assemble_hot ok");
    let segments = resp.data.segments.expect("segments emitted");
    assert_eq!(segments.len(), 6);
    for s in &segments {
        assert_eq!(s.byte_start, 0);
        assert_eq!(s.byte_end, 0);
    }
}
```

(Adjust constructor / call shape to match the SDK's existing convention — `Cairn::in_memory()` is illustrative; use whatever the other tests in `surface.rs` use.)

- [ ] **Step 3: Run**

Run: `cargo nextest run -p cairn-sdk surface`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-sdk/
git commit -m "feat(sdk): wire assemble_hot transport + round-trip test (#288)"
```

---

## Task 16: Run the full verification checklist

**Files:** none modified — running CLAUDE.md §8 commands.

- [ ] **Step 1: Codegen no-diff**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked`
Expected: no file modified (clean working tree). If diffs appear, commit them and re-run.

- [ ] **Step 2: Format**

Run: `cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 3: Clippy**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Compile**

Run: `cargo check --workspace --all-targets --locked`
Expected: PASS.

- [ ] **Step 5: Tests**

Run: `cargo nextest run --workspace --locked --no-fail-fast`
Expected: all tests PASS.

- [ ] **Step 6: Doctests**

Run: `cargo test --doc --workspace --locked`
Expected: PASS (the doctest in `build_segments` runs here).

- [ ] **Step 7: Core dependency boundary**

Run: `./scripts/check-core-boundary.sh`
Expected: PASS.

- [ ] **Step 8: Docgen**

Run: `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`
Expected: PASS. If it fails because the `assemble_hot` docs need re-rendering, run with `-- --write` and commit the result.

- [ ] **Step 9: Supply chain**

Run sequentially:
```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```
Expected: each PASS.

- [ ] **Step 10: Doc build (optional, for full-fidelity)**

Run: `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked`
Expected: PASS.

- [ ] **Step 11: Insta review pass**

Run: `cargo insta pending-snapshots -p cairn-core -p cairn-cli`
Expected: empty (all snapshots accepted).

If the docgen step in §8 produces changes under `docs/site/src/reference/generated/`, commit those:

```bash
git add docs/site/src/reference/generated/
git commit -m "docs(generated): refresh assemble_hot docs (#288)"
```

---

## Task 17: Open the PR

- [ ] **Step 1: Push the branch**

```bash
git push -u origin feat/issue-288-assemble-hot-segments
```

- [ ] **Step 2: Open a PR**

```bash
gh pr create --title "feat(assemble_hot): emit cache-breakpoint segments (#288)" --body "$(cat <<'EOF'
## Summary

- Extends `AssembleHotData` with `Option<Vec<HotSegment>>` carrying recipe-step segments (step, byte range, stability hint, sha256 content_hash) so harness wrappers can attach provider-specific prompt-cache breakpoints.
- Layered validation (`validate_base` + `validate_segments` + `validate_with_recipe`) runs inside the `Deserialize` impl via `#[serde(try_from)]`; bypass is impossible.
- New stub-body `cairn_core::verbs::assemble_hot::assemble_hot(config)` walks `HotMemoryConfig.recipe` and returns a real `AssembleHotData`. CLI and SDK return that value end-to-end.
- Real source-loading (read `purpose.md`/SQLite/etc.) is deliberately stubbed; that's the missing-half of #193 and replaces a single function (`load_step_body`).

## Brief refs

§5 hot memory recipe, §7 hot prefix, §8.0.f assemble_hot verb shape, §8 contract parity.

## Invariants touched

- New optional field on `cairn.mcp.v1` (forward-compat: `Option<Vec<...>>`, no schema default; serializes as absent only when `None`).
- Frozen for v1: `HotRecipeStep` and `SegmentStability` enum variants.
- New direct dep: `sha2`.

## Test plan

- [ ] Unit + property tests in `cairn-core::verbs::assemble_hot::segments`
- [ ] Per-variant validator tests
- [ ] Trust-boundary integration tests in `crates/cairn-core/tests/assemble_hot_envelope.rs`
- [ ] Insta snapshot of canonical JSON
- [ ] CLI end-to-end snapshot of `cairn assemble_hot --json`
- [ ] SDK round-trip test
- [ ] Doctest demonstrating wrapper translating segments to fictional `Cache::breakpoint()`
- [ ] Full CLAUDE.md §8 checklist (fmt / clippy / nextest / doctest / docgen / deny / audit / machete)

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Return the PR URL when done.

---

## Self-Review Notes

Coverage check completed against the spec:

- **§3 schema**: Task 1 — exact JSON.
- **§4 Rust types**: Task 3 — generated automatically, verified.
- **§5 helper + validators + errors + bridge**: Tasks 4–9.
- **§5a stub assembler + From conversion**: Tasks 4 (From), 11 (assembler).
- **§6.1 unit**: Tasks 5, 7. **§6.2 proptest**: Task 8. **§6.3 insta**: Task 12. **§6.4 doctest**: included in `build_segments` in Task 5; verified by §6 of Task 16.
- **Trust-boundary integration**: Task 10. **CLI end-to-end**: Task 14. **SDK**: Task 15.
- **§7 acceptance map**: every row maps to a task.
- **§8 risks** (codegen drift, skill-pack snapshots, envelope hook architecture, sha2 dep, non_exhaustive, wire-compat invariant, stub load_step_body): each addressed in a task or in the verification checklist.
- **§9 verification**: Task 16.

Type consistency check: `AssembleHotValidationError` variants used in Task 6 match the variants tested in Task 7. `HotRecipeStep` / `SegmentStability` names from generated code (Task 3) match imports in Tasks 5/7/10/12. `validate_with_recipe` signature in spec matches usage in Task 7.

Placeholder scan: no "TBD"/"TODO"; every code step has the actual code. Two intentional handoff points where the engineer must read existing code to find the integrating pattern (Tasks 13/15 — sibling verbs as reference) — these are in-codebase references, not unspecified work.
