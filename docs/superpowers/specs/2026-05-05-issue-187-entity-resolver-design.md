# Issue #187 — Three-tier entity resolver design

**Status**: design / pre-implementation
**Date**: 2026-05-05
**Issue**: https://github.com/windoliver/cairn/issues/187
**Brief sections**: §5 (ingestion pipeline — Extract → Classify → Store), §4 (`LLMProvider` contract)
**Updates**: #74 (LLM extractor — adds dedup strategy)

## 1. Goal

Implement entity deduplication during `ingest` so that the same concept under
different names (`AuthService`, `auth_service`, `Auth Service`) resolves to one
`EntityNode` rather than poisoning the knowledge graph with duplicates.

The resolver is a **pure pipeline stage in `cairn-core`** — no I/O, no store
calls. The caller (ingest verb, separate issue) pre-fetches in-scope candidates
and invokes the resolver with the candidate name plus an `&[EntityNode]` slice.

## 2. Three-tier cascade

Following Graphiti (arXiv 2501.13956 §3.2):

1. **Tier 1 — Exact match** (sync, free). Lowercase + punctuation-strip the
   candidate name; linear-scan `existing[i].name_norm` for equality.
2. **Tier 2 — MinHash 3-gram Jaccard** (sync, free). Shingles → 128-perm MinHash
   signature → pairwise Jaccard against each `existing` entity. Jaccard ≥ 0.85
   → merge; multiple ≥ 0.85 → ambiguous (surface to caller).
3. **Tier 3 — LLM pairwise dedup** (async, gated on `LLMProvider`). Fires only
   when Tier 1+2 produce no merge. Picks top-1 existing entity with Jaccard
   ∈ `[llm_low_band, fuzzy_threshold)` and asks the model "are these the same?".
   Merges if `same==true && confidence ≥ llm_min_confidence`, else `New`.

Tier 3 is best-effort. `LlmError::NotConfigured` and `LlmError::CapabilityMissing`
silently skip Tier 3 (P0 offline invariant: zero `LLMProvider` → Tiers 1+2 still
produce correct merges). All other `LlmError` variants propagate as
`EntityResolutionError::Llm` so ops can debug transport / auth failures.

## 3. Module layout

New module under `crates/cairn-core/src/pipeline/entity_resolve/`:

```
pipeline/entity_resolve/
├── mod.rs          ← EntityResolver, Resolution, EntityResolutionError, ResolverConfig
├── normalize.rs    ← Tier 1: name_norm + exact match (pure, sync)
├── minhash.rs      ← Tier 2: 3-gram shingles, 128-perm MinHash, Jaccard (pure, sync)
└── llm.rs          ← Tier 3: prompt builder, JSON schema, confidence gate (async)
```

Sibling to `pipeline/extract/` and `pipeline/filter/`. Brief §5.2 places this
stage between Extract and Store: `… Extract → Filter → Classify → Resolve → Store`.

Module is registered in `pipeline/mod.rs`. No new workspace-crate deps. One
external dep added: `twox-hash` (feature `xxh3`) for stable, fast,
non-cryptographic hashing — std `DefaultHasher` is documented as cross-version
unstable, so determinism (required for reproducible vault state and snapshot
tests) rules it out.

## 4. Public surface

```rust
// pipeline/entity_resolve/mod.rs

pub struct EntityResolver {
    config: ResolverConfig,
    permutation_seeds: [u64; MAX_NUM_PERMUTATIONS],   // derived from config.hash_seed at construct time
    llm: Option<Arc<dyn LLMProvider>>,
}

impl EntityResolver {
    pub fn new(config: ResolverConfig, llm: Option<Arc<dyn LLMProvider>>)
        -> Result<Self, ResolverConfigError>;

    pub async fn resolve(
        &self,
        candidate_name: &str,
        existing: &[EntityNode],
    ) -> Result<Resolution, EntityResolutionError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Resolution {
    /// Tier 1 or Tier 2 (single hit) or Tier 3 (LLM accepted) → merge.
    Merge(EntityId),
    /// No tier produced a merge → caller creates a new node.
    New,
    /// Tier 2 found multiple entities with Jaccard ≥ fuzzy_threshold.
    /// Caller decides: skip (create new + flag for `lint`), or invoke
    /// LLM disambiguation across the set, or surface to user.
    Ambiguous(Vec<EntityId>),
}

#[derive(Debug, Clone, Copy)]
pub struct ResolverConfig {
    pub fuzzy_threshold: f32,        // default 0.85
    pub llm_low_band: f32,           // default 0.5  (Tier 3 only fires above this)
    pub llm_min_confidence: f32,     // default 0.7  (LLM "same: true" merge gate)
    pub num_permutations: usize,     // default 128, max MAX_NUM_PERMUTATIONS
    pub hash_seed: u64,              // default DEFAULT_HASH_SEED (fixed const, cross-process determinism)
}

pub const MAX_NUM_PERMUTATIONS: usize = 128;
pub const DEFAULT_HASH_SEED: u64 = 0x_CA12_F1A6_5EED_BEEF;

impl Default for ResolverConfig { /* issue-spec'd defaults */ }
impl ResolverConfig {
    pub fn validate(&self) -> Result<(), ResolverConfigError>;
}
```

### `EntityResolutionError`

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EntityResolutionError {
    /// Tier 3 LLM call failed with a non-skippable error
    /// (transport / auth / parse / budget). NotConfigured and
    /// CapabilityMissing are silently mapped to Resolution::New
    /// per Tier 3's offline-graceful contract.
    #[error("llm tier-3 failed: {source}")]
    Llm { #[source] source: LlmError },

    /// Tier 3 returned a payload the caller could not interpret
    /// even though no LlmError was raised. Defence-in-depth for
    /// providers that bypass schema enforcement; should be
    /// unreachable when LLMProvider honours the schema arg.
    #[error("llm tier-3 returned malformed payload: {detail}")]
    LlmInvalidResponse { detail: String },
}
```

### `ResolverConfigError` (construction-time only)

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResolverConfigError {
    #[error("fuzzy_threshold must be in [0.0, 1.0], got {got}")]
    FuzzyThresholdOutOfRange { got: f32 },
    #[error("llm_low_band ({low}) must be < fuzzy_threshold ({high})")]
    LlmBandInverted { low: f32, high: f32 },
    #[error("llm_min_confidence must be in [0.0, 1.0], got {got}")]
    LlmMinConfidenceOutOfRange { got: f32 },
    #[error("num_permutations must be > 0 and ≤ {max}", max = MAX_NUM_PERMUTATIONS)]
    NumPermutationsOutOfRange { got: usize },
}
```

Validating at construction guarantees `resolve()` itself never errors on config.

## 5. Component details

### 5.1 `normalize.rs` (Tier 1)

```rust
pub fn normalize(s: &str) -> String;            // lowercase, retain [a-z0-9 ], collapse runs of whitespace, trim
fn exact_match<'a>(norm: &str, existing: &'a [EntityNode]) -> Option<&'a EntityId>;
```

- ASCII-only retention. Non-ASCII letters (`é`, `ç`, …) are stripped. This
  matches the issue's `is_alphanumeric() || ' '` filter when applied to
  lowercased ASCII input. Documented as a known limitation; non-ASCII names
  fall through to Tier 2 (which also operates on `name_norm`) and Tier 3.
- Idempotent: `normalize(normalize(s)) == normalize(s)` (proptest).

### 5.2 `minhash.rs` (Tier 2)

```rust
pub struct MinHashSignature(pub [u64; MAX_NUM_PERMUTATIONS]);
                          // physically 128 wide; effective length = config.num_permutations
fn shingles(norm: &str) -> SmallVec<[(usize, usize); 16]>;   // (start, end) byte ranges over norm
fn signature(norm: &str, shingles: &[(usize, usize)], seeds: &[u64]) -> MinHashSignature;
fn jaccard(a: &MinHashSignature, b: &MinHashSignature, n: usize) -> f32;

/// Per-existing scored entry returned alongside FuzzyOutcome so Tier 3
/// can pick its top-1 in-band candidate without re-signing existing entities.
struct Scored<'a> { node: &'a EntityNode, jaccard: f32 }

enum FuzzyOutcome { None, One(EntityId), Many(Vec<EntityId>) }

/// Returns (FuzzyOutcome, all_scored). all_scored is sorted descending by
/// jaccard; ties broken by EntityId lex order for determinism.
fn fuzzy_match<'a>(
    cand_sig: &MinHashSignature,
    existing: &'a [EntityNode],
    seeds: &[u64],
    threshold: f32,
    n: usize,
) -> (FuzzyOutcome, Vec<Scored<'a>>);
```

- 3-gram shingles over UTF-8 chars (use `char_indices()` to derive byte ranges
  — never split mid-char). Strings shorter than 3 chars produce one shingle of
  the whole string.
- Hash function: `twox_hash::XxHash64::oneshot(seed, bytes)`. One seed
  per permutation slot (precomputed in `EntityResolver::new` from
  `config.hash_seed` via splitmix64).
- For each permutation `i`: signature slot `i = min over shingles of hash`.
- `jaccard(a, b, n) = matching_slots / n` where matching_slots counts equal
  values in `a.0[..n] vs b.0[..n]`.
- `fuzzy_match` re-signatures every `existing` entity on each call. P0 scale
  is small (single-vault, ≤ low-thousands of entities); LSH banding and
  persisted signatures are filed as a follow-up issue (out of scope here).

### 5.3 `llm.rs` (Tier 3)

```rust
pub(super) async fn llm_dedup(
    provider: &dyn LLMProvider,
    candidate_name: &str,
    top_match: &EntityNode,
    min_confidence: f32,
) -> Result<Resolution, EntityResolutionError>;
```

Prompt (verbatim from issue):

```
Are these two entities the same real-world concept?
  A: {candidate_name}
  B: {top_match.name}
Respond as JSON: { "same": <bool>, "confidence": <float 0..1>, "reasoning": <string> }
```

JSON schema (sent in `CompletionRequest.schema`; provider validates before
returning):

```json
{
  "type": "object",
  "additionalProperties": false,
  "required": ["same", "confidence", "reasoning"],
  "properties": {
    "same":       { "type": "boolean" },
    "confidence": { "type": "number", "minimum": 0, "maximum": 1 },
    "reasoning":  { "type": "string", "maxLength": 512 }
  }
}
```

Decision:
- `same==true && confidence ≥ min_confidence` → `Resolution::Merge(top_match.id)`
- otherwise → `Resolution::New`

`reasoning` is logged at `tracing::debug` only (privacy; brief §14 — never log
record bodies above debug).

### 5.4 Orchestration (`mod.rs::resolve`)

```
1. norm = normalize(candidate_name)
2. if let Some(id) = exact_match(&norm, existing): return Merge(id.clone())
3. cand_sig = signature(norm, shingles(&norm), &self.permutation_seeds[..n])
4. (outcome, scored) = fuzzy_match(&cand_sig, existing, &seeds, fuzzy_threshold, n)
   match outcome:
     One(id)   → return Merge(id)
     Many(ids) → return Ambiguous(ids)
     None      → fall through (scored already sorted desc by jaccard)
5. // Tier 3
   top = scored.first()  // highest-jaccard existing; deterministic on ties
   if top is None or top.jaccard < llm_low_band: return New
   match self.llm.as_ref():
     None    → return New
     Some(p) → match llm_dedup(p, name, top.node, min_conf).await:
                 Ok(res)                                                → return res
                 Err(Llm { source: NotConfigured | CapabilityMissing }) → return New  // silent skip
                 Err(e)                                                 → return Err(e)
```

The fuzzy pass already computes `cand_sig`; Tier 3's "top-1 by Jaccard" reuses
the same per-existing signatures — no extra hashing.

## 6. Invariants (CLAUDE.md §4)

- **Harness-agnostic**: pure `cairn-core` module, no harness assumptions.
- **Stand-alone P0**: Tiers 1+2 fully functional with `llm: None`.
- **CLI-is-ground-truth**: resolver is a building block; CLI binding lives
  in the ingest verb (separate issue).
- **Seven contracts**: Tier 3 uses `LLMProvider` (existing contract). No new
  contract introduced.
- **Pure functions**: `normalize`, `shingles`, `signature`, `jaccard`,
  `fuzzy_match`, `exact_match` are all sync and side-effect-free.
- **Fail closed on capability**: missing `LLMProvider` capability → silent skip
  (per issue) — every other failure surfaces.
- **`#![forbid(unsafe_code)]`** inherited workspace-level.
- **No `unwrap()` / `expect()`** in `cairn-core` (deny-linted).
- **Privacy**: `reasoning` only logged at `tracing::debug`. Names are entity
  identifiers, not body content; `info`-level logging of names is acceptable.

## 7. Testing

### Unit (in-module `#[cfg(test)]`)
- `normalize.rs` — fixtures: `"AuthService"`, `"auth_service"`, `"Auth  Service"`
  → `"authservice"` / `"auth service"` / `"auth service"`. Empty input → empty.
  UTF-8 multibyte: `"AuthSérvice"` → `"authsrvice"` (documented Latin-1 strip).
- `minhash.rs` — shingle count for `"abc"` (1) / `"abcd"` (2); signature
  determinism (same input twice → byte-identical sig); `jaccard(sig, sig) == 1.0`;
  `jaccard` of disjoint signatures == 0.0.
- `llm.rs` — prompt format snapshot test; schema accepts well-formed payload,
  rejects missing fields; `same:true && conf < min` → `New`; `same:false` → `New`.
- `mod.rs` — tier ordering: exact preempts fuzzy preempts LLM; empty `existing`
  → `New` without LLM call; `llm: None` → `New` with band hit; `Ambiguous` skips
  LLM; mock `NotConfiguredLlm` → `New`; mock `UnreachableLlm` → `Err(Llm)`.

### Property tests (`proptest`)
- `normalize_idempotent`: `∀ s: String. normalize(normalize(&s)) == normalize(&s)`.
- `signature_determinism`: same shingles + same seeds → byte-identical signature.
- `jaccard_bounds`: `0.0 ≤ jaccard(a, b, n) ≤ 1.0` for arbitrary signatures.
- `jaccard_self`: `jaccard(a, a, n) == 1.0`.
- **Boundary (issue AC)**: synthetic shingle-set construction such that
  `jaccard == 107/128 ≈ 0.836` and `jaccard == 109/128 ≈ 0.852` are reachable;
  assert `fuzzy_match` at threshold 0.85 returns `None` for the first and
  `One` for the second.

### Integration (`crates/cairn-core/tests/`)
- `entity_resolver_offline.rs` — end-to-end with `llm: None`; verifies the AC
  "Tier 1 + Tier 2 fully functional with zero `LLMProvider`".
- `entity_resolver_llm_skip.rs` — stub LLM returning `NotConfigured` then
  `CapabilityMissing`; confirms silent skip → `Resolution::New`.

### Mock `LLMProvider` (test-only, lives next to the resolver tests)
- `StubLlm` returning canned `CompletionOutput::Json` for inspection of prompt
  + decision branches.
- `NotConfiguredLlm` / `CapMissingLlm` returning the corresponding `LlmError`.
- `UnreachableLlm` returning `LlmError::ProviderUnreachable`.

### Boundary script
- `./scripts/check-core-boundary.sh` — must pass; no new workspace-crate deps.

### Supply chain
- `cargo deny check` — `twox-hash` is dual-licensed Apache-2.0 OR MIT,
  already covered by `deny.toml` allowlist; no allowlist change needed.
- `cargo machete` — verify `twox-hash` is actually used.

## 8. Acceptance criteria mapping (issue #187)

| AC item | Where satisfied |
|---|---|
| `EntityResolver` as a pure struct in `cairn-core` (no I/O, no storage) | §3, §4 — module under `pipeline/`, no store deps; LLM is contract-level not adapter |
| Tier 1 + Tier 2 fully functional with zero `LLMProvider` | §5.4 step 5 — `llm: None` short-circuits to `Resolution::New` |
| Tier 3 skips gracefully on `NotConfigured` / `CapabilityMissing` (issue's `CapabilityUnavailable`) | §5.4 step 5 — both variants mapped to `New` |
| `proptest` normalization idempotency | §7 — `normalize_idempotent` |
| Jaccard boundary 0.84 → new, 0.85 → merge | §7 — boundary property test using `107/128` and `109/128` |
| No `unwrap()` / `expect()` in `cairn-core`; typed `EntityResolutionError` | §4 — typed enum; lint enforced workspace-level |
| `./scripts/check-core-boundary.sh` passes | §7 — verification |

## 9. Out of scope (filed separately if not already)

- LSH banding / persisted MinHash signatures in store schema (perf optimization).
- `entity_episodes` upsert and merge persistence (caller — ingest verb).
- Store-side scope-tuple filtering of candidate set (caller — ingest verb).
- Tier 3 disambiguation across `Resolution::Ambiguous` candidate sets
  (caller policy decision; resolver surfaces the set, ingest decides).
- `lint` workflow flagging of `Ambiguous` outcomes.
- CLI / SDK / MCP wiring of the resolver (those surfaces invoke the ingest
  verb, which in turn invokes the resolver).

## 10. References

- Brief §4 — `LLMProvider` contract.
- Brief §5.2 — write-path pipeline ordering.
- Brief §14 — privacy / never log bodies above `debug`.
- Brief §15 — fail closed on capability.
- Graphiti, arXiv 2501.13956 §3.2 — three-tier entity dedup pattern.
- Issue #187 — this design.
- Issue #74 — LLM extractor (consumer of this resolver upstream).
