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
  "required": ["prefix", "bytes", "segments"],
  "properties": {
    "prefix":   { "type": "string", "description": "Assembled hot-memory text ready to inject into the agent prompt. May be empty when no hot-memory is available." },
    "bytes":    { "type": "integer", "minimum": 0, "maximum": 4194304 },
    "segments": {
      "type": "array",
      "items": { "$ref": "#/$defs/HotSegment" },
      "description": "Recipe-step segments covering [0, bytes) contiguously, in declaration order."
    }
  }
}
```

`segments` is **required** (never absent). Its length equals the number of chunks the assembler passed to `build_segments`:

- `build_segments(&[])` → `("", vec![])`. The "no hot-memory available" case.
- `build_segments(&[(step, ""), (step, ""), ...])` → `("", vec![<N zero-length segments>])`. All recipe slots ran but produced no content; the segments are kept so wrappers can still see slot identity.

Both are legal wire shapes. Wrappers that only care about non-empty content filter on `byte_end > byte_start`.

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
    pub segments: Vec<HotSegment>,
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

/// Build (`prefix`, `segments`) from an ordered list of `(step, body)` chunks.
///
/// - `prefix` = concatenation of bodies in input order.
/// - `segments.len() == chunks.len()`. Empty input → empty output. All-empty
///   bodies → N zero-length segments (kept, not dropped — preserves slot
///   identity so wrappers can still inspect what the assembler attempted).
/// - Each segment's `byte_start..byte_end` is a half-open UTF-8 byte range
///   into `prefix`. Ranges cover `[0, prefix.len())` with no gaps, no
///   overlaps. First start is 0; last end is `prefix.len()`.
/// - `content_hash` = lowercase hex `sha256` over the segment's bytes
///   (`prefix[byte_start..byte_end].as_bytes()`), no domain separation.
pub fn build_segments(chunks: &[(HotRecipeStep, &str)]) -> (String, Vec<HotSegment>);
```

**Why keep zero-length segments instead of dropping them.** The recipe-order invariant is "segments[i].step matches the i-th chunk the assembler passed". Dropping empty segments breaks that alignment and forces wrappers to re-derive which slots ran. A wrapper that wants only non-empty segments filters on `byte_end > byte_start` — trivial. The "no hot-memory at all" case is distinct: the assembler passes `&[]` (no chunks) and gets `(String::new(), vec![])`.

**Why no domain separation in the hash.** The issue's stated goal is "byte-stable across runs when inputs unchanged" — that's `sha256(bytes)`. Wrappers that key on hash already know which segment slot they're caching, so cross-slot collisions aren't a real risk.

**Dependency.** `sha2 = "0.10"` added to `cairn-core` workspace deps. No transitive bloat — `sha2` is already in the dep tree via other crates; verify with `cargo tree -e normal -p cairn-core --depth 1`.

**No CLI / store changes.** `cairn-cli/src/verbs/assemble_hot.rs` stays returning `unimplemented_response`. When the real assembler lands (separate issue) it calls `build_segments(...)` to populate the response. Capability advertisement (§8.0.a) is unchanged.

## 6. Tests

### 6.1 Unit tests (`segments.rs` `mod tests`)

- `default_stability_matches_brief` — table-asserts the six step → stability mappings.
- `build_segments_empty_input_returns_empty` — `build_segments(&[])` → `(String::new(), vec![])`.
- `build_segments_all_empty_chunks_keeps_zero_length_segments` — six empty chunks → `prefix == ""`, `segments.len() == 6`, every `byte_start == byte_end == 0`. Pins the empty-prefix vs zero-length-segments contract from §3.
- `build_segments_single_chunk` — one step, prefix = body, one segment with full range.
- `build_segments_preserves_recipe_order` — chunks in declaration order produce segments in that order.
- `content_hash_matches_segment_bytes` — for each segment, `hex(sha256(prefix[s..e].as_bytes())) == s.content_hash`.
- `build_segments_handles_non_ascii` — chunk body `"héllo·世界"` (multi-byte UTF-8). Asserts `byte_end - byte_start == body.len()` (bytes, not chars), `prefix[s..e] == body`, and `s.content_hash == hex(sha256(body.as_bytes()))`. Pins the UTF-8-byte-offset wire contract; documents that wrappers MUST use byte slicing, not code-unit slicing.

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
/// let chunks = vec![
///     (HotRecipeStep::Purpose, "..."),
///     (HotRecipeStep::RecentUserSignal, "..."),
/// ];
/// let (_prefix, segments) = build_segments(&chunks);
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
| `segments` populated in declaration order | unit `build_segments_preserves_recipe_order`; property `coverage_invariant` |
| `byte_range` covers `[0, bytes)` with no gaps / no overlaps | property `coverage_invariant` |
| `content_hash` byte-stable across runs when inputs unchanged | property `hash_stability`; insta snapshot |
| `--json` output of `cairn assemble_hot` includes segments; insta snapshot covers shape | **Partial.** The wire shape is locked by an insta snapshot of `AssembleHotData` JSON serialized from a hand-built fixture. The CLI binary itself still returns `unimplemented_response` because the verb is not yet wired to a `HotMemoryAssembler`; the end-to-end CLI snapshot ships with that wiring PR (missing-half of #193). Called out in §2 and the PR description. |
| No provider-specific terms in Cairn types — only stability hints | review |
| Doctest demonstrates wrapper translating segments to fictional `Cache::breakpoint()` | doctest on `build_segments` |

## 8. Risks

- **Codegen drift.** Editing `assemble_hot.json` requires re-running `cargo run -p cairn-idl --bin cairn-codegen` and committing regenerated `.rs` across `cairn-core`, `cairn-mcp`, `cairn-cli`, `cairn-sdk`. CI gates on no-diff.
- **Skill-pack codegen.** `crates/cairn-idl/tests/codegen_emit_skill.rs` and `skill_compat.rs` likely snapshot the verb's data shape — `cargo insta review` + commit.
- **`bytes` field semantics.** Now equals both `prefix.len() as u64` and `segments.last().byte_end` (when non-empty). Property test asserts both.
- **New direct dep.** `sha2` becomes a direct `cairn-core` dep. Justify in PR; verify with `cargo tree`.
- **`#[non_exhaustive]` enums** force downstream `match` arms. Acceptable per CLAUDE.md §6.10.

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
