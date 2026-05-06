# Design — `assemble_hot` cache-breakpoint segments (issue #288)

**Status:** Approved (brainstorm 2026-05-05).
**Brief refs:** §5 hot memory recipe; §7 hot prefix; §8.0.f `assemble_hot` verb shape; §8 contract parity.
**Issue:** [#288](https://github.com/windoliver/cairn/issues/288).

## 1. Goal

Extend the IDL-generated `AssembleHotData` to carry recipe-step segments alongside the assembled `prefix`, so harness wrappers can attach provider-specific prompt-cache breakpoints (Anthropic `cache_control`, OpenAI cache primitive, etc.) without Cairn knowing anything about provider cache APIs.

This PR is the **types + pure helper** slice. Wiring `cairn assemble_hot` to a real `HotMemoryAssembler` is out of scope (separate issue, the missing-half of #193).

## 2. Non-goals

- Mutating the `prefix` format or adding separators between segments.
- Any provider-specific cache type (`cache_control`, `ttl: "5m"`) leaking into Cairn surfaces.
- Wiring the CLI verb. `cairn assemble_hot` keeps returning `unimplemented_response` until a real `HotMemoryAssembler` lands (separate issue, missing-half of #193). **This is a deliberate, declared partial fulfilment of issue #288**: the wire-shape acceptance criterion ("`--json` output of `cairn assemble_hot` includes segments; insta snapshot covers shape") is satisfied at the type/JSON level (snapshot of `AssembleHotData` JSON), not at the CLI-binary level. The PR description for #288 must call this out so reviewers and the assembler PR author both know the CLI snapshot is owed by the next PR.
- Unifying the IDL-generated `HotRecipeStep` with `cairn-core::config::HotMemoryRecipeStep`. A `From` conversion lands with the assembler PR that needs it.

## 3. IDL schema change

`crates/cairn-idl/schema/verbs/assemble_hot.json` extends the `Data` definition:

```json
"Data": {
  "type": "object",
  "additionalProperties": false,
  "x-cairn-validate": true,
  "required": ["prefix", "bytes"],
  "properties": {
    "prefix":   { "type": "string", "description": "Assembled hot-memory text ready to inject into the agent prompt. May be empty when no hot-memory is available." },
    "bytes":    { "type": "integer", "minimum": 0, "maximum": 4194304 },
    "segments": {
      "type": "array",
      "items": { "$ref": "#/$defs/HotSegment" },
      "default": [],
      "description": "Recipe-step segments covering [0, bytes) contiguously, in declaration order. Three-state contract: (1) FIELD ABSENT ON THE WIRE — legacy `cairn.mcp.v1` producer that predates this feature; consumers MUST treat `prefix` as opaque. (2) `\"segments\": []` — new producer ran with NO recipe configured; the producer explicitly opts into the new contract but had nothing to emit. REQUIRED INVARIANT: `prefix == \"\"` AND `bytes == 0`. (3) `\"segments\": [...]` — new producer with a configured recipe; `len()` mirrors `HotMemoryConfig.recipe.len()` 1:1, including N zero-length entries when the recipe ran with no content. The Rust binding distinguishes (1) from (2) via `Option<Vec<HotSegment>>` (None vs Some(vec![])); JSON-only consumers distinguish them by field presence."
    }
  }
}
```

**Wire-compat: `segments` is optional under `cairn.mcp.v1`, not required.** CLAUDE.md §6.10: "adding a required field is a breaking change. Use `#[serde(default)]` + optional fields for forward compat." The generated Rust type encodes the field as `Option<Vec<HotSegment>>` so absent, empty, and non-empty are all distinguishable after deserialization (§4). Schema-side, `segments` is omitted from `required` and the absent-on-wire case is the legacy path.

`segments` is an **optional, three-state field** (forward-compat with frozen `cairn.mcp.v1`):

- **Absent on the wire** → deserializes to `None`. Legacy `cairn.mcp.v1` producer that predates this feature, or a future producer that opts out. Wrappers fall back to treating `prefix` as opaque.
- **`"segments": []` on the wire** → deserializes to `Some(vec![])`. Canonical wire shape for "new producer ran with no recipe configured" (`HotMemoryConfig.recipe` is empty). Distinguishable from legacy absence. Validated invariant (§5): `prefix == ""` AND `bytes == 0`. The envelope hook enforces this; a payload with a non-empty `prefix` and `segments: []` is rejected at the trust boundary, not silently treated as opaque.
- **`"segments": [...]` on the wire** → deserializes to `Some(vec![...])`. Canonical non-empty: `len()` mirrors `HotMemoryConfig.recipe.len()` 1:1, including N zero-length entries when the recipe ran but produced no content.

The three states are kept distinct after deserialization by encoding `segments` as `Option<Vec<HotSegment>>` (§4). `Vec::new()` alone cannot represent absence vs configured-empty vs unconfigured — `Option` does.

Producers in this PR always emit `Some(...)`. Legacy-absent is only produced by older peers. The envelope hook validates the inner `Vec` only when `Some`.

`HotSegment` definition (added under `$defs`):

```json
"HotSegment": {
  "type": "object",
  "additionalProperties": false,
  "required": ["step", "byte_start", "byte_end", "stability", "content_hash"],
  "properties": {
    "step":         { "enum": ["purpose","index","pinned_feedback","top_salience_project","active_playbook","recent_user_signal"] },
    "byte_start":   { "type": "integer", "minimum": 0, "maximum": 4194304 },
    "byte_end":     { "type": "integer", "minimum": 0, "maximum": 4194304 },
    "stability":    { "enum": ["stable_1h","stable_5m","volatile"] },
    "content_hash": { "type": "string", "pattern": "^[0-9a-f]{64}$" }
  }
}
```

**Wire-shape rationale:**

- Flat `byte_start` / `byte_end` (half-open) over a nested `Range` object: lighter JSON, no extra `$defs`. The half-open invariant is asserted by tests (`segments[i].byte_end == segments[i+1].byte_start`).
- **Cross-segment invariants are not expressible in JSON Schema 2020-12 portably** (no cross-property comparison). Per-field bounds (`byte_start`/`byte_end` ∈ `[0, 4194304]`) are in the schema; everything else is enforced at runtime in two layers (§5):
    - `validate_base(&data)` — `bytes == prefix.len()`. Wire invariants of `AssembleHotData` that hold regardless of whether `segments` was emitted.
    - `validate_segments(&data)` — per-segment monotonicity, contiguity, bounds, hash correctness. Only meaningful when `segments` is non-empty.
- **Validation runs inside `Deserialize` itself, not in an optional shim.** Existing call sites use `serde_json::from_str::<Response>` and `from_slice::<Response>` directly (see `cairn-cli/tests/envelope_tests.rs`); a hand-written `try_decode_data` wrapper would not intercept those paths and the trust boundary would leak. Instead, this PR makes `AssembleHotData` use `#[serde(try_from = "AssembleHotDataRaw")]`, where `AssembleHotDataRaw` is a private mirror struct with the same fields and the inverse `From<AssembleHotData>` impl for `Serialize`. The `TryFrom<AssembleHotDataRaw> for AssembleHotData` impl runs `validate_base` unconditionally and `validate_segments` whenever `segments.is_some()`. Errors surface as `serde::de::Error` (turning into `serde_json::Error`/`envelope::DecodeError`). **Every code path that deserializes the type — generated `Response`, direct serde calls, MCP, SDK, tests — runs validation; bypass is impossible.** Codegen change: the optional emitter learns to emit `#[serde(try_from = ..., into = ...)]` for verbs that opt in via an `x-cairn-validate: true` schema annotation; this is added to `assemble_hot.json` in this PR and is a one-time codegen feature.
- **Offsets are UTF-8 byte positions into `prefix`, not code-unit positions.** Wrappers in languages with non-UTF-8 string types (JavaScript / Java use UTF-16, Python `str` is opaque) MUST encode `prefix` to UTF-8 bytes before slicing. JS: `new TextEncoder().encode(prefix).slice(s, e)`. Python: `prefix.encode("utf-8")[s:e]`. Naïve `String.prototype.slice(s, e)` in JS uses UTF-16 code units and will corrupt any non-ASCII segment, including miscomputing `content_hash` against the wrong bytes. Documented on the schema and pinned by a non-ASCII unit test (§6.1).
- Step wire values match existing `cairn-core::config::HotMemoryRecipeStep` snake_case (note: `top_salience_project`, not the issue's informal `top_salience`).
- `content_hash` is lowercase hex sha256 (64 chars) over the segment's UTF-8 bytes. Hex over base64url because every other content-hash in the brief / Rust ecosystem is hex; 64 chars in JSON is fine.

## 4. Generated Rust types

After re-running `cargo run -p cairn-idl --bin cairn-codegen`, `crates/cairn-core/src/generated/verbs/assemble_hot.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssembleHotData {
    pub bytes: u64,
    pub prefix: String,
    /// Three-state encoding (§3):
    /// - `None`: legacy `cairn.mcp.v1` peer that predates this feature
    ///   (the field was absent on the wire).
    /// - `Some(vec![])`: new producer that ran but had no recipe
    ///   configured (`HotMemoryConfig.recipe` empty). Canonical and
    ///   distinguishable from absent.
    /// - `Some(vec![...])`: new producer with a configured recipe.
    ///   `len()` mirrors `HotMemoryConfig.recipe.len()` 1:1.
    ///
    /// `#[serde(default)]` deserializes absent → `None`.
    /// `skip_serializing_if = "Option::is_none"` is permitted (and is
    /// what the existing codegen emits for optional fields): it skips
    /// only the `None` case, while `Some(vec![])` and `Some(vec![...])`
    /// both still serialize, preserving the three-state distinction.
    /// `skip_serializing_if = "Vec::is_empty"` is **forbidden** —
    /// it would collapse `Some(vec![])` into legacy-absent on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segments: Option<Vec<HotSegment>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HotSegment {
    pub step: HotRecipeStep,
    pub byte_start: u64,
    pub byte_end: u64,
    pub stability: SegmentStability,
    pub content_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HotRecipeStep {
    Purpose, Index, PinnedFeedback,
    TopSalienceProject, ActivePlaybook, RecentUserSignal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SegmentStability { Stable1h, Stable5m, Volatile }
```

`#[non_exhaustive]` on both enums per CLAUDE.md §6.10 (public enums that may grow).

The IDL-generated `HotRecipeStep` is structurally identical to the hand-written `cairn-core::config::HotMemoryRecipeStep`. They are kept separate by the workspace's IDL/config split (IDL = wire types from JSON Schema; config = parsed YAML). A `From` conversion will land with the assembler PR that needs it.

## 5. Pure helper in `cairn-core`

**Trust model.** Producer-side step-order alignment is enforced by construction in `build_segments(recipe, bodies)`. Generic-envelope decode in `cairn-core` checks payload self-consistency (`validate_base` always; `validate_segments` whenever `segments.is_some()`) but does **not** enforce that the steps match a configured recipe — it cannot, because the wire layer has no access to `HotMemoryConfig`. Consumers that know which recipe their producer should be running call `validate_with_recipe` themselves.

Step labels are inherently producer-controlled — there is no out-of-band signal cairn-core can compare them against, so making `validate_with_recipe` mandatory at the wire boundary is technically impossible without inflating every payload with a recipe-identity checksum (rejected as overkill: the workspace's only producer is `build_segments`, which is alignment-by-construction). This is the same trust posture as every other typed wire surface in the workspace: structural validity at the boundary, semantic alignment by either producer-side construction or caller-supplied expectation. **Wrapper guidance**: an external wrapper that does NOT have access to the producer's `HotMemoryConfig` should treat segment step labels as advisory metadata only; cache placement should rely on `stability` transitions and `content_hash` (which `validate_segments` does enforce), not on `step` semantics.


New module `crates/cairn-core/src/verbs/assemble_hot/segments.rs` (no I/O, no adapter deps):

```rust
use crate::generated::verbs::assemble_hot::{HotRecipeStep, HotSegment, SegmentStability};

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

/// Build (`prefix`, `segments`) from a configured recipe and parallel bodies.
///
/// **Recipe alignment is by construction.** The helper takes the recipe
/// (e.g. `&HotMemoryConfig::default().recipe`) and a parallel `bodies`
/// slice, and emits exactly one segment per recipe slot. Producers cannot
/// reorder, omit, or duplicate steps — the type signature forbids it.
///
/// - `recipe.len() == bodies.len()`; mismatch returns
///   `AssembleHotValidationError::RecipeBodiesLenMismatch` (the only
///   build-time error).
/// - `prefix` = concatenation of `bodies[i]` in recipe order.
/// - `segments[i].step == recipe[i]` for all i. Zero-length segments
///   are kept (canonical empty-content shape).
/// - `byte_start..byte_end` is a half-open UTF-8 byte range into `prefix`.
///   Ranges cover `[0, prefix.len())` with no gaps, no overlaps.
/// - `content_hash` = lowercase hex `sha256` over the segment's bytes.
///
/// "No recipe configured" (the recipe is empty) → `(String::new(), vec![])`.
/// Distinct from "recipe ran with no content" (N zero-length segments).
pub fn build_segments(
    recipe: &[HotRecipeStep],
    bodies: &[&str],
) -> Result<(String, Vec<HotSegment>), AssembleHotValidationError>;

/// Wire invariants of `AssembleHotData` that hold regardless of whether
/// `segments` was emitted: `data.bytes == data.prefix.len() as u64`.
/// Always called by the envelope decode hook.
pub fn validate_base(data: &AssembleHotData) -> Result<(), AssembleHotValidationError>;

/// Segment-specific invariants, applied whenever the producer emitted
/// segments (`data.segments.is_some()`):
///
/// - On `None` (legacy-absent), returns `Ok(())` — nothing to check.
/// - On `Some(vec![])` (canonical empty-recipe), enforces the
///   "empty-segments-implies-empty-prefix" invariant: `data.prefix`
///   MUST be empty and `data.bytes` MUST be 0, otherwise returns
///   `EmptySegmentsRequiresEmptyPrefix`. Without this, a malformed
///   producer could emit `{prefix:"abc", bytes:3, segments:[]}` and
///   slip a non-canonical payload past the trust boundary under the
///   "no recipe configured" state.
/// - On `Some(vec![...])`, runs the per-segment checks: `byte_start <=
///   byte_end`, contiguous in declaration order, first start 0, last
///   end `prefix.len()`, every `content_hash` matches its slice.
///
/// Does NOT check recipe alignment (the payload alone does not know
/// what recipe was configured); use `validate_with_recipe` for that.
pub fn validate_segments(data: &AssembleHotData) -> Result<(), AssembleHotValidationError>;

/// Convenience: `validate_base` then `validate_segments`.
pub fn validate(data: &AssembleHotData) -> Result<(), AssembleHotValidationError>;

/// Like `validate`, but also asserts that `data.segments` is `Some` and
/// matches the expected recipe (`segments[i].step == expected[i]` for
/// all i; `segments.len() == expected.len()`). Returns
/// `LegacyProducerSegmentsAbsent` when `data.segments` is `None` — the
/// legacy-absent case can never satisfy a recipe assertion. Called by
/// consumers that require a recipe-emitting producer.
pub fn validate_with_recipe(
    data: &AssembleHotData,
    expected: &[HotRecipeStep],
) -> Result<(), AssembleHotValidationError>;

/// Errors `build_segments` / `validate` / `validate_with_recipe` can return.
/// `#[non_exhaustive]`.
#[derive(Debug, thiserror::Error)]
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
}
```

**Why keep zero-length segments instead of dropping them.** The recipe-order invariant is "`segments[i].step` matches the i-th configured recipe step". Dropping empty segments breaks alignment, forces wrappers to re-derive which slots ran, and creates two non-equivalent encodings for the same `(prefix, recipe)` state (an idempotency hazard for cache planners). Keeping zero-length segments gives one canonical non-empty wire shape per recipe: when `Some(vec)`, `vec.len() == recipe.len()` always; "recipe ran, no content" = N zero-length entries. A wrapper that only wants non-empty content filters on `byte_end > byte_start`.

**Why no domain separation in the hash.** The issue's stated goal is "byte-stable across runs when inputs unchanged" — that's `sha256(bytes)`. Wrappers that key on hash already know which segment slot they're caching, so cross-slot collisions aren't a real risk.

**Dependency.** `sha2 = "0.10"` added to `cairn-core` workspace deps. No transitive bloat — `sha2` is already in the dep tree via other crates; verify with `cargo tree -e normal -p cairn-core --depth 1`.

**No CLI / store changes.** `cairn-cli/src/verbs/assemble_hot.rs` stays returning `unimplemented_response`. When the real assembler lands (separate issue) it calls `build_segments(...)` to populate the response. Capability advertisement (§8.0.a) is unchanged.

## 6. Tests

### 6.1 Unit tests (`segments.rs` `mod tests`)

- `default_stability_matches_brief` — table-asserts the six step → stability mappings.
- `build_segments_empty_recipe` — `build_segments(&[], &[])` → `(String::new(), vec![])`. Distinct from "recipe ran with no content".
- `build_segments_all_empty_bodies` — recipe of length 6, all bodies `""` → `prefix == ""`, `segments.len() == 6`, every `byte_start == byte_end == 0`. Pins the canonical empty-content shape (§3).
- `build_segments_rejects_len_mismatch` — `build_segments(&[Purpose, Index], &["a"])` → `Err(RecipeBodiesLenMismatch { .. })`.
- `build_segments_aligns_steps_to_recipe` — for arbitrary recipe + bodies, `segments[i].step == recipe[i]` for all i.
- `build_segments_single_slot` — one step, prefix = body, one segment with full range.
- `content_hash_matches_segment_bytes` — for each segment, `hex(sha256(prefix[s..e].as_bytes())) == s.content_hash`.
- `build_segments_handles_non_ascii` — body `"héllo·世界"` (multi-byte UTF-8). Asserts `byte_end - byte_start == body.len()` (bytes, not chars), `prefix[s..e] == body`, and `s.content_hash == hex(sha256(body.as_bytes()))`. Pins the UTF-8-byte-offset wire contract.

**Validator unit tests** (one per error variant in `AssembleHotValidationError`):

- `validate_base_accepts_well_formed` — `bytes == prefix.len()`, segments may be empty.
- `validate_base_rejects_bytes_mismatch` — `data.bytes != prefix.len()`. Independent of `segments`.
- `validate_segments_accepts_well_formed` — happy path on a 6-slot fixture.
- `validate_segments_accepts_canonical_empty` — `Some(vec![])` with `prefix == ""` and `bytes == 0` returns `Ok(())`.
- `validate_segments_rejects_empty_with_non_empty_prefix` — `Some(vec![])` with `prefix == "abc"` returns `EmptySegmentsRequiresEmptyPrefix`. Pins the invariant Codex flagged: a malformed producer cannot smuggle opaque content past the trust boundary by claiming "no recipe configured".
- `validate_segments_accepts_none` — `None` returns `Ok(())` (legacy-absent has no segment-layer invariants to check).
- `validate_segments_rejects_descending_range` — hand-crafted `{byte_start: 10, byte_end: 5}`.
- `validate_segments_rejects_non_contiguous` — gap between two segments.
- `validate_segments_rejects_does_not_start_at_zero`.
- `validate_segments_rejects_does_not_cover_prefix` — last segment ends before `prefix.len()`.
- `validate_segments_rejects_hash_mismatch` — flip a content_hash byte.
- `validate_segments_rejects_out_of_bounds` — last segment ends past `prefix.len()`.
- `validate_with_recipe_rejects_step_mismatch` — payload's step does not match expected recipe at index i.
- `validate_with_recipe_rejects_recipe_len_mismatch` — `segments.len() != expected.len()`.
- `build_segments_output_validates` — round-trip: `validate(&build(...).unwrap()) == Ok(())` for any `(recipe, bodies)`.

**Trust-boundary integration tests** (in `cairn-core/tests/`):

These specifically exercise `serde_json::from_str::<Response>` (the call shape used by the existing test suite and SDK) to prove validation cannot be bypassed. Tests that deserialize `AssembleHotData` directly are equally covered because the `try_from` annotation makes the path identical.


- `envelope_decode_rejects_malformed_assemble_hot` — craft an envelope JSON with a bad `byte_end`; assert `ResponseEnvelope::try_decode_data` returns the validation error, not `Ok`.
- `envelope_decode_rejects_legacy_with_bytes_mismatch` — legacy payload (no `segments`) where `bytes != prefix.len()`; assert decode rejects via `validate_base`. Pins that absent segments do **not** bypass base invariants.
- `envelope_decode_accepts_legacy_well_formed` — legacy payload (no `segments`) with `bytes == prefix.len()` decodes to `data.segments == None` (not `Some(vec![])`) and returns `Ok`. Pins the legacy-absent → `None` mapping.
- `envelope_decode_round_trips_canonical_empty` — payload with `prefix==""`, `bytes==0`, `"segments": []` round-trips as `Some(vec![])`. Pins that the absent-vs-empty distinction survives serialize → deserialize.
- `envelope_decode_rejects_empty_segments_with_non_empty_prefix` — payload with `prefix=="abc"`, `bytes==3`, `"segments": []` is rejected with `EmptySegmentsRequiresEmptyPrefix`, not silently treated as opaque.
- `envelope_decode_round_trips_non_empty` — full 6-slot payload round-trips as `Some(vec![...len 6])`.

### 6.2 Property tests (`proptest`)

- **Coverage invariant.** For arbitrary `Vec<(HotRecipeStep, String)>`: segments cover `[0, prefix.len())` with no gaps (`segments[i].byte_end == segments[i+1].byte_start`) and no overlaps; first start is 0; last end is `prefix.len()`.
- **Hash stability.** `build_segments(chunks) == build_segments(chunks)` (deterministic — no time, no RNG).
- **UTF-8 boundary safety.** Every `prefix[s..e]` is valid UTF-8. Safe by construction (concat-only, no mid-codepoint slicing); the test pins it.

### 6.3 Snapshot test (`insta`)

`crates/cairn-core/tests/assemble_hot_snapshots.rs`. Hand-built fixture: a six-tuple chunk list with deterministic byte content. Snapshot the JSON serialization of `AssembleHotData` (including `segments`). This is the **acceptance criterion** "byte-stable across runs when inputs unchanged" — the `.snap` file *is* the stability check.

### 6.4 Doctest

On `build_segments` — walks a fictional wrapper translating segments to a `Cache::breakpoint()` call between longer-lived → shorter-lived transitions:

```rust
/// ```
/// # use cairn_core::verbs::assemble_hot::{build_segments, HotRecipeStep, SegmentStability};
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
```

### 6.5 Out of scope this PR

CLI snapshot of `cairn assemble_hot --json` — the verb is unwired. That snapshot test ships with the assembler PR.

## 7. Acceptance criteria mapping (issue #288)

| Criterion | Where verified |
|---|---|
| `segments` populated in declaration order | **Producer side, by construction:** `build_segments(recipe, bodies)` accepts recipe + bodies as parallel slices and emits `segments[i].step == recipe[i]` for all i — the helper signature makes reorder/dup/omit unrepresentable. **Consumer side, opt-in:** `validate_with_recipe(data, expected)` enforces the same invariant when the consumer knows which recipe to expect. The generic envelope hook does **not** enforce step order, because `cairn-core` envelope decoding has no access to `HotMemoryConfig` (config is loaded by `cairn-cli`/SDK callers, not the wire layer). This is a deliberate trust-model choice: in P0, the only producer is `cairn-core::build_segments`, and the type system enforces alignment there. External producers that bypass the helper are out of the P0 trust model; consumers facing such producers MUST call `validate_with_recipe` themselves. Tests: unit `build_segments_aligns_steps_to_recipe`; `validate_with_recipe_rejects_step_mismatch`. |
| `byte_range` covers `[0, bytes)` with no gaps / no overlaps | property `coverage_invariant`; `validate()` rejects malformed payloads at runtime (§5) |
| `content_hash` byte-stable across runs when inputs unchanged | property `hash_stability`; insta snapshot |
| `--json` output of `cairn assemble_hot` includes segments; insta snapshot covers shape | **Partial.** The wire shape is locked by an insta snapshot of `AssembleHotData` JSON serialized from a hand-built fixture. The CLI binary itself still returns `unimplemented_response` because the verb is not yet wired to a `HotMemoryAssembler`; the end-to-end CLI snapshot ships with that wiring PR (missing-half of #193). Called out in §2 and the PR description. |
| No provider-specific terms in Cairn types — only stability hints | review |
| Doctest demonstrates wrapper translating segments to fictional `Cache::breakpoint()` | doctest on `build_segments` |

## 8. Risks

- **Codegen drift.** Editing `assemble_hot.json` requires re-running `cargo run -p cairn-idl --bin cairn-codegen` and committing regenerated `.rs` across `cairn-core`, `cairn-mcp`, `cairn-cli`, `cairn-sdk`. CI gates on no-diff. The existing codegen pattern for optional fields (`emit_sdk.rs:~1796`) emits `#[serde(default, skip_serializing_if = "Option::is_none")]` — that is **the right pattern** for `segments`: it skips only the `None` case (legacy-absent on the wire), while `Some(vec![])` and `Some(vec![...])` both still serialize. The forbidden pattern is `Vec::is_empty`, which would collapse the canonical-empty case. Verify the generated output for `assemble_hot.rs` matches §4 character-for-character before merging.
- **Skill-pack codegen.** `crates/cairn-idl/tests/codegen_emit_skill.rs` and `skill_compat.rs` likely snapshot the verb's data shape — `cargo insta review` + commit.
- **Validation must live in `Deserialize`, not an external shim.** Existing call sites deserialize `Response` directly via `serde_json::from_str`/`from_slice`; a hand-written `try_decode_data` shim would not intercept those paths. This PR adds an `x-cairn-validate: true` schema annotation that the codegen interprets as `#[serde(try_from = "<Type>Raw", into = "<Type>Raw")]`, with `TryFrom<Raw>` calling `validate_base` + `validate_segments` (when `segments.is_some()`). Both `Some(vec![])` and `Some(vec![...])` go through validation; only `None` skips the segment layer. `assemble_hot` is the first verb to use this annotation; future verbs that need post-deserialize invariants opt in the same way.
- **`bytes` field semantics.** Now equals both `prefix.len() as u64` and `segments.last().byte_end` (when `segments` is non-empty). Property test asserts both.
- **New direct dep.** `sha2` becomes a direct `cairn-core` dep. Justify in PR; verify with `cargo tree`.
- **`#[non_exhaustive]` enums** force downstream `match` arms. Acceptable per CLAUDE.md §6.10.
- **Wire-compat invariant.** `segments` is optional (§3). A future PR that wants to make it required must bump the contract version (e.g., `cairn.mcp.v2`); not allowed under `cairn.mcp.v1`.

## 9. Verification (CLAUDE.md §8)

```
cargo run -p cairn-idl --bin cairn-codegen --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo deny check
cargo audit --deny warnings
cargo machete
cargo insta review   # for any snapshot churn
```
