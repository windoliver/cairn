# Issue #82 — Hot Prefix Retrieval Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the stub `load_step_body` with real source-specific
retrieval for the six v1 `HotRecipeStep` variants, applying uniform
admissibility (visibility, scope, confidence, staleness) and emitting
typed inclusion/exclusion traces.

**Architecture:** Pure-projection: caller (cairn-cli/sdk) pre-filters
records by kind/visibility/scope and feeds them into a typed
`HotMemoryInputs`. Core applies admissibility + ranking + top-K + body
assembly. The IDL grows one optional `debug` field on `AssembleHotData`;
the recipe enum stays frozen for cairn.mcp.v1.

**Tech Stack:** Rust 1.95, edition 2024, `thiserror`, `chrono`,
`insta`, `proptest`, `cargo-nextest`, `cairn-idl` codegen.

**Spec:** `docs/superpowers/specs/2026-05-08-issue-82-hot-prefix-retrieval-design.md`

---

## File Map

**Create:**
- `crates/cairn-core/src/verbs/assemble_hot/inclusion.rs`
- `crates/cairn-core/src/verbs/assemble_hot/admissibility.rs`
- `crates/cairn-core/src/verbs/assemble_hot/sources/mod.rs`
- `crates/cairn-core/src/verbs/assemble_hot/sources/purpose.rs`
- `crates/cairn-core/src/verbs/assemble_hot/sources/index.rs`
- `crates/cairn-core/src/verbs/assemble_hot/sources/pinned.rs`
- `crates/cairn-core/src/verbs/assemble_hot/sources/project.rs`
- `crates/cairn-core/src/verbs/assemble_hot/sources/playbook.rs`
- `crates/cairn-core/src/verbs/assemble_hot/sources/user_signal.rs`
- `crates/cairn-core/src/verbs/assemble_hot/inputs.rs` — `HotMemoryInputs`
- `crates/cairn-core/tests/assemble_hot_inputs.rs`
- `crates/cairn-core/tests/assemble_hot_privacy.rs`
- `crates/cairn-core/tests/assemble_hot_debug.rs`
- `crates/cairn-cli/tests/assemble_hot_smoke.rs`

**Modify:**
- `crates/cairn-idl/schema/verbs/assemble_hot.json` — add optional `debug` + `HotStepTrace` def
- `crates/cairn-core/src/generated/verbs/assemble_hot.rs` — regenerated
- `crates/cairn-core/src/verbs/assemble_hot/mod.rs` — re-exports
- `crates/cairn-core/src/verbs/assemble_hot/assembler.rs` — refactor to take `HotMemoryInputs`
- `crates/cairn-core/tests/assemble_hot_snapshots.rs` — fixture-driven, deterministic body

---

## Task 1: IDL — add optional `debug` field + `HotStepTrace`

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/assemble_hot.json`
- Regenerate: `crates/cairn-core/src/generated/verbs/assemble_hot.rs`

- [ ] **Step 1: Read current IDL**

```bash
cat crates/cairn-idl/schema/verbs/assemble_hot.json
```

- [ ] **Step 2: Add `debug` ref + named defs**

Edit `crates/cairn-idl/schema/verbs/assemble_hot.json`. Add `"debug"` after `"segments"` inside `Data.properties` as a `$ref` so codegen emits a stable type name:

```json
        "debug": {
          "$ref": "#/$defs/HotMemoryDebug",
          "description": "Optional inclusion/exclusion trace per recipe step. Absent unless the caller requested debug output. Wire-compatible addition for cairn.mcp.v1: older consumers ignore unknown fields."
        }
```

Add four new `$defs` entries after `SegmentStability`:

```json
    ,
    "HotMemoryDebug": {
      "type": "object",
      "additionalProperties": false,
      "required": ["steps"],
      "properties": {
        "steps": {
          "type": "array",
          "maxItems": 64,
          "items": { "$ref": "#/$defs/HotStepTrace" }
        }
      }
    },
    "HotStepTrace": {
      "type": "object",
      "additionalProperties": false,
      "required": ["step", "included", "excluded"],
      "properties": {
        "step":     { "$ref": "#/$defs/HotRecipeStep" },
        "included": { "type": "array", "items": { "$ref": "#/$defs/HotInclusion" } },
        "excluded": { "type": "array", "items": { "$ref": "#/$defs/HotExclusion" } }
      }
    },
    "HotInclusion": {
      "type": "object",
      "additionalProperties": false,
      "required": ["record_id", "score", "note"],
      "properties": {
        "record_id": { "type": "string", "minLength": 1 },
        "score":     { "type": "number" },
        "note":      { "type": "string" }
      }
    },
    "HotExclusion": {
      "type": "object",
      "additionalProperties": false,
      "required": ["record_id", "reason"],
      "properties": {
        "record_id": { "type": "string", "minLength": 1 },
        "reason":    { "$ref": "#/$defs/HotExclusionReason" }
      }
    },
    "HotExclusionReason": {
      "type": "string",
      "enum": [
        "tombstoned",
        "forgotten_scope",
        "below_confidence_floor",
        "out_of_scope",
        "visibility_denied",
        "outside_recency_window",
        "beyond_top_k",
        "not_pinned",
        "empty_body"
      ]
    }
```

After `cargo run -p cairn-idl --bin cairn-codegen --locked` runs, verify the generated module exports types named `HotMemoryDebug`, `HotStepTrace`, `HotInclusion`, `HotExclusion`, and `HotExclusionReason`:

```bash
grep -E 'pub (struct|enum) (HotMemoryDebug|HotStepTrace|HotInclusion|HotExclusion|HotExclusionReason)' \
  crates/cairn-core/src/generated/verbs/assemble_hot.rs
```

Expected: five hits. If a name differs (e.g., codegen emits `Debug` instead of `HotMemoryDebug` for an inline object), reconcile by renaming in Tasks 11/12 — the names referenced there must match codegen output verbatim.

- [ ] **Step 3: Regenerate codegen**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked`
Expected: writes new types into `crates/cairn-core/src/generated/verbs/assemble_hot.rs`. No errors.

- [ ] **Step 4: Verify codegen idempotency**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
Expected: exit 0, no diff.

- [ ] **Step 5: Build to confirm no breakage**

Run: `cargo check -p cairn-core --locked`
Expected: success.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-idl/schema/verbs/assemble_hot.json \
        crates/cairn-core/src/generated/verbs/assemble_hot.rs
git commit -m "feat(idl): add optional debug field on AssembleHotData (issue #82)

Adds HotStepTrace + HotInclusion + HotExclusion definitions and an
optional debug property on Data. Wire-compatible addition for
cairn.mcp.v1; older consumers ignore unknown fields. The HotRecipeStep
enum is unchanged."
```

---

## Task 2: ExclusionReason + InclusionTrace types

**Files:**
- Create: `crates/cairn-core/src/verbs/assemble_hot/inclusion.rs`
- Modify: `crates/cairn-core/src/verbs/assemble_hot/mod.rs`

- [ ] **Step 1: Write the inclusion module skeleton + unit test**

Create `crates/cairn-core/src/verbs/assemble_hot/inclusion.rs`:

```rust
//! Inclusion / exclusion trace types for `assemble_hot` (issue #82).
//!
//! Surface the typed reason a record either entered the assembled hot
//! prefix or was filtered out. Adapter callers gate this behind a
//! per-call `include_debug` flag so production callers do not pay for
//! the bookkeeping when they do not need it.

use crate::domain::RecordId;

/// Why a candidate record did not enter the assembled hot prefix.
///
/// Wire form: snake_case strings matching `HotExclusion.reason`
/// in `cairn-idl/schema/verbs/assemble_hot.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExclusionReason {
    /// The record's `tombstoned` flag was set.
    Tombstoned,
    /// The record sat under a forget-state-scope.
    ForgottenScope,
    /// `record.confidence < 0.3` (matches the profile synthesizer floor).
    BelowConfidenceFloor,
    /// The record's `scope` is not satisfied by the caller's authorized scope.
    OutOfScope,
    /// The record's `visibility` is not in `authorized_visibility`.
    VisibilityDenied,
    /// The record's `updated_at` is older than the source's recency window.
    OutsideRecencyWindow,
    /// The record was admissible but ranked below the source's top-K cap.
    BeyondTopK,
    /// The record did not match the source's pin / kind narrowing.
    NotPinned,
    /// `record.body` was empty.
    EmptyBody,
}

impl ExclusionReason {
    /// Wire-form name as exported by the IDL `HotExclusion.reason` enum.
    #[must_use]
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Tombstoned => "tombstoned",
            Self::ForgottenScope => "forgotten_scope",
            Self::BelowConfidenceFloor => "below_confidence_floor",
            Self::OutOfScope => "out_of_scope",
            Self::VisibilityDenied => "visibility_denied",
            Self::OutsideRecencyWindow => "outside_recency_window",
            Self::BeyondTopK => "beyond_top_k",
            Self::NotPinned => "not_pinned",
            Self::EmptyBody => "empty_body",
        }
    }
}

/// Why a record did enter the assembled hot prefix, with the rank score.
#[derive(Debug, Clone, PartialEq)]
pub struct InclusionTrace {
    /// Record that contributed to the segment body.
    pub record_id: RecordId,
    /// Source-specific score (e.g. `salience × recency` for pinned).
    pub score: f64,
    /// Human-readable note describing why this record won (`"top-1 by updated_at"`).
    pub note: &'static str,
}

/// Why a record was filtered, paired with its `record_id`.
#[derive(Debug, Clone, PartialEq)]
pub struct ExclusionTrace {
    /// Record that was filtered.
    pub record_id: RecordId,
    /// Typed reason.
    pub reason: ExclusionReason,
}

/// Output of one source's `select` call.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LoadedSegment {
    /// Concatenated markdown body produced by the source.
    pub body: String,
    /// Records that contributed to `body`, in emission order.
    pub included: Vec<InclusionTrace>,
    /// Records that were filtered, in evaluation order.
    pub excluded: Vec<ExclusionTrace>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusion_reason_wire_strings_match_idl() {
        // The wire strings must mirror the IDL HotExclusion.reason enum.
        assert_eq!(ExclusionReason::Tombstoned.as_wire(), "tombstoned");
        assert_eq!(ExclusionReason::ForgottenScope.as_wire(), "forgotten_scope");
        assert_eq!(
            ExclusionReason::BelowConfidenceFloor.as_wire(),
            "below_confidence_floor"
        );
        assert_eq!(ExclusionReason::OutOfScope.as_wire(), "out_of_scope");
        assert_eq!(
            ExclusionReason::VisibilityDenied.as_wire(),
            "visibility_denied"
        );
        assert_eq!(
            ExclusionReason::OutsideRecencyWindow.as_wire(),
            "outside_recency_window"
        );
        assert_eq!(ExclusionReason::BeyondTopK.as_wire(), "beyond_top_k");
        assert_eq!(ExclusionReason::NotPinned.as_wire(), "not_pinned");
        assert_eq!(ExclusionReason::EmptyBody.as_wire(), "empty_body");
    }

    #[test]
    fn loaded_segment_default_is_empty() {
        let s = LoadedSegment::default();
        assert!(s.body.is_empty());
        assert!(s.included.is_empty());
        assert!(s.excluded.is_empty());
    }
}
```

- [ ] **Step 2: Add module to `mod.rs`**

Edit `crates/cairn-core/src/verbs/assemble_hot/mod.rs` — add `pub mod inclusion;` and re-export:

```rust
pub mod assembler;
pub mod inclusion;
pub mod raw;
pub mod segments;

pub use assembler::{AssembleHotError, assemble_hot};
pub use inclusion::{ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment};
pub use segments::{
    AssembleHotValidationError, MAX_SEGMENTS, build_segments, default_stability, validate,
    validate_base, validate_segments, validate_with_recipe,
};
```

- [ ] **Step 3: Run unit tests**

Run: `cargo nextest run -p cairn-core --locked verbs::assemble_hot::inclusion`
Expected: 2 tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/inclusion.rs \
        crates/cairn-core/src/verbs/assemble_hot/mod.rs
git commit -m "feat(assemble_hot): typed inclusion/exclusion traces (issue #82)"
```

---

## Task 3: Admissibility predicate

**Files:**
- Create: `crates/cairn-core/src/verbs/assemble_hot/admissibility.rs`
- Modify: `crates/cairn-core/src/verbs/assemble_hot/mod.rs`

- [ ] **Step 1: Write tests + module in one file**

Create `crates/cairn-core/src/verbs/assemble_hot/admissibility.rs`:

```rust
//! Uniform admissibility predicate (issue #82, brief §6, §7).
//!
//! Every source delegates to [`admit`] before its own ranking. This
//! re-checks visibility / scope / confidence / body invariants the
//! adapter is supposed to have applied — defense-in-depth at the
//! trust boundary so a buggy or malicious adapter cannot smuggle a
//! record into the hot prefix.

use crate::domain::record::MemoryRecord;
use crate::domain::scope::ScopeTuple;
use crate::domain::taxonomy::MemoryVisibility;

use super::inclusion::ExclusionReason;

/// Confidence floor — matches `pipeline::profile::synthesize::CONFIDENCE_FLOOR`.
pub const CONFIDENCE_FLOOR: f32 = 0.3;

/// Returns `Ok(())` if the record passes the uniform admissibility
/// gates, otherwise the typed exclusion reason.
///
/// Order matches §7's "fail-closed" intent: tombstone > scope >
/// visibility > confidence > body. The first failing gate wins; later
/// gates are not evaluated.
pub fn admit(
    record: &MemoryRecord,
    authorized_scope: &ScopeTuple,
    authorized_visibility: &[MemoryVisibility],
) -> Result<(), ExclusionReason> {
    if record.body.is_empty() {
        return Err(ExclusionReason::EmptyBody);
    }
    if !scope_satisfied(authorized_scope, &record.scope) {
        return Err(ExclusionReason::OutOfScope);
    }
    if !authorized_visibility.contains(&record.visibility) {
        return Err(ExclusionReason::VisibilityDenied);
    }
    if record.confidence.is_nan() || record.confidence < CONFIDENCE_FLOOR {
        return Err(ExclusionReason::BelowConfidenceFloor);
    }
    Ok(())
}

/// Every set dimension on `authorized` must equal the record's value
/// on the same dimension. The record may carry additional dimensions
/// (e.g. session_id) — those are not narrowed.
fn scope_satisfied(authorized: &ScopeTuple, record: &ScopeTuple) -> bool {
    let pairs: [(&Option<String>, &Option<String>); 7] = [
        (&authorized.tenant, &record.tenant),
        (&authorized.workspace, &record.workspace),
        (&authorized.project, &record.project),
        (&authorized.session_id, &record.session_id),
        (&authorized.entity, &record.entity),
        (&authorized.user, &record.user),
        (&authorized.agent, &record.agent),
    ];
    for (auth, rec) in pairs {
        if let Some(a) = auth
            && Some(a) != rec.as_ref()
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;

    fn allow_private() -> Vec<MemoryVisibility> {
        vec![MemoryVisibility::Private]
    }

    fn matching_scope() -> ScopeTuple {
        ScopeTuple {
            user: Some("hmn:tafeng".to_owned()),
            ..ScopeTuple::default()
        }
    }

    #[test]
    fn admits_canonical_sample_record() {
        let r = sample_record();
        assert_eq!(admit(&r, &matching_scope(), &allow_private()), Ok(()));
    }

    #[test]
    fn rejects_empty_body() {
        let mut r = sample_record();
        r.body = String::new();
        assert_eq!(
            admit(&r, &matching_scope(), &allow_private()),
            Err(ExclusionReason::EmptyBody)
        );
    }

    #[test]
    fn rejects_visibility_not_in_allowlist() {
        let r = sample_record();
        // sample_record() is Private; allowlist only Public → denied.
        let only_public = vec![MemoryVisibility::Public];
        assert_eq!(
            admit(&r, &matching_scope(), &only_public),
            Err(ExclusionReason::VisibilityDenied)
        );
    }

    #[test]
    fn rejects_below_confidence_floor() {
        let mut r = sample_record();
        r.confidence = 0.29;
        assert_eq!(
            admit(&r, &matching_scope(), &allow_private()),
            Err(ExclusionReason::BelowConfidenceFloor)
        );
    }

    #[test]
    fn rejects_scope_mismatch() {
        let r = sample_record();
        let other_user = ScopeTuple {
            user: Some("hmn:someoneelse".to_owned()),
            ..ScopeTuple::default()
        };
        assert_eq!(
            admit(&r, &other_user, &allow_private()),
            Err(ExclusionReason::OutOfScope)
        );
    }

    #[test]
    fn empty_authorized_scope_admits_any_scope() {
        let r = sample_record();
        let empty = ScopeTuple::default();
        assert_eq!(admit(&r, &empty, &allow_private()), Ok(()));
    }
}
```

- [ ] **Step 2: Register the module in `mod.rs`**

Edit `crates/cairn-core/src/verbs/assemble_hot/mod.rs`:

```rust
pub mod admissibility;
pub mod assembler;
pub mod inclusion;
pub mod raw;
pub mod segments;

pub use admissibility::{CONFIDENCE_FLOOR, admit};
```

(keep existing re-exports below.)

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core --locked verbs::assemble_hot::admissibility`
Expected: 6 tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/admissibility.rs \
        crates/cairn-core/src/verbs/assemble_hot/mod.rs
git commit -m "feat(assemble_hot): uniform admissibility predicate (issue #82)"
```

---

## Task 4: HotMemoryInputs struct

**Files:**
- Create: `crates/cairn-core/src/verbs/assemble_hot/inputs.rs`
- Modify: `crates/cairn-core/src/verbs/assemble_hot/mod.rs`

- [ ] **Step 1: Write the inputs module + smoke test**

Create `crates/cairn-core/src/verbs/assemble_hot/inputs.rs`:

```rust
//! Pure-projection inputs for `assemble_hot` (issue #82).
//!
//! Adapters (`cairn-cli`, `cairn-sdk`) materialize this struct from a
//! `MemoryStore` query + filesystem reads. Core never touches the
//! store or the filesystem.

use crate::domain::Rfc3339Timestamp;
use crate::domain::record::MemoryRecord;
use crate::domain::scope::ScopeTuple;
use crate::domain::taxonomy::MemoryVisibility;

/// Pre-filtered record + filesystem inputs for [`super::assemble_hot`].
///
/// Slot contracts (caller-side responsibility, all enforced by the
/// adapter, double-checked by core):
///
/// * `pinned_candidates`: `kind ∈ {user, feedback} ∧ is_static = 1`,
///   already narrowed to the authorized scope/visibility. `is_static`
///   is a store-side projection (no field on `MemoryRecord`); the
///   caller is the only authority for it.
/// * `project_candidates`: `kind = project`.
/// * `playbook_candidates`: `kind = playbook`.
/// * `user_signal_candidates`: `kind = user_signal`.
/// * `purpose_md` / `index_md`: pre-read file content.
///
/// Each record-bearing slot may include records the source-side ranker
/// later excludes (low confidence, scope mismatch, etc.). Core's
/// admissibility check is the trust boundary, not the slot membership.
#[derive(Debug, Clone)]
pub struct HotMemoryInputs<'a> {
    /// Body of `purpose.md`, already read from disk by the adapter.
    pub purpose_md: &'a str,
    /// Body of `index.md`, already read from disk by the adapter.
    pub index_md: &'a str,
    /// Caller-narrowed pinned-feedback candidates.
    pub pinned_candidates: &'a [&'a MemoryRecord],
    /// `project`-kind candidates for the salience source.
    pub project_candidates: &'a [&'a MemoryRecord],
    /// `playbook`-kind candidates.
    pub playbook_candidates: &'a [&'a MemoryRecord],
    /// `user_signal`-kind candidates.
    pub user_signal_candidates: &'a [&'a MemoryRecord],
    /// Reference instant. Recency windows are computed against this.
    pub now: Rfc3339Timestamp,
    /// Authorized scope for this assembly call (re-checked per record).
    pub scope: ScopeTuple,
    /// Visibility tiers the caller is authorized to see.
    pub authorized_visibility: &'a [MemoryVisibility],
    /// When `true`, the assembler populates `AssembleHotData.debug`.
    pub include_debug: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::taxonomy::MemoryVisibility;

    #[test]
    fn inputs_is_constructible() {
        let r = sample_record();
        let recs = [&r];
        let now = Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid");
        let scope = ScopeTuple::default();
        let allow = [MemoryVisibility::Private];
        let inputs = HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &recs,
            project_candidates: &[],
            playbook_candidates: &[],
            user_signal_candidates: &[],
            now,
            scope,
            authorized_visibility: &allow,
            include_debug: false,
        };
        assert_eq!(inputs.pinned_candidates.len(), 1);
        assert!(!inputs.include_debug);
    }
}
```

- [ ] **Step 2: Re-export from `mod.rs`**

Edit `crates/cairn-core/src/verbs/assemble_hot/mod.rs` — add `pub mod inputs;` and `pub use inputs::HotMemoryInputs;`.

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core --locked verbs::assemble_hot::inputs`
Expected: 1 test pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/inputs.rs \
        crates/cairn-core/src/verbs/assemble_hot/mod.rs
git commit -m "feat(assemble_hot): HotMemoryInputs projection (issue #82)"
```

---

## Task 5: Sources scaffold + purpose/index pass-through

**Files:**
- Create: `crates/cairn-core/src/verbs/assemble_hot/sources/mod.rs`
- Create: `crates/cairn-core/src/verbs/assemble_hot/sources/purpose.rs`
- Create: `crates/cairn-core/src/verbs/assemble_hot/sources/index.rs`
- Modify: `crates/cairn-core/src/verbs/assemble_hot/mod.rs`

- [ ] **Step 1: Create the sources module**

Create `crates/cairn-core/src/verbs/assemble_hot/sources/mod.rs`:

```rust
//! Source-specific `select` functions for each `HotRecipeStep`. Each
//! one returns a [`super::inclusion::LoadedSegment`] — never errors —
//! so the assembler can compose them deterministically.

pub mod index;
pub mod pinned;
pub mod playbook;
pub mod project;
pub mod purpose;
pub mod user_signal;
```

- [ ] **Step 2: Write the purpose source + tests**

Create `crates/cairn-core/src/verbs/assemble_hot/sources/purpose.rs`:

```rust
//! `Purpose` source: pass-through `purpose.md` content.

use crate::verbs::assemble_hot::inclusion::LoadedSegment;
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

/// Render the purpose segment by copying `inputs.purpose_md` verbatim.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    LoadedSegment {
        body: inputs.purpose_md.to_owned(),
        included: Vec::new(),
        excluded: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Rfc3339Timestamp;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn empty_inputs<'a>(purpose: &'a str) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: purpose,
            index_md: "",
            pinned_candidates: &[],
            project_candidates: &[],
            playbook_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn passes_purpose_md_through_verbatim() {
        let s = select(&empty_inputs("# Purpose\nI am a purpose.\n"));
        assert_eq!(s.body, "# Purpose\nI am a purpose.\n");
        assert!(s.included.is_empty());
        assert!(s.excluded.is_empty());
    }

    #[test]
    fn empty_purpose_emits_empty_segment() {
        let s = select(&empty_inputs(""));
        assert!(s.body.is_empty());
    }
}
```

- [ ] **Step 3: Write the index source + tests (mirrors purpose)**

Create `crates/cairn-core/src/verbs/assemble_hot/sources/index.rs`:

```rust
//! `Index` source: pass-through `index.md` content.

use crate::verbs::assemble_hot::inclusion::LoadedSegment;
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

/// Render the index segment by copying `inputs.index_md` verbatim.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    LoadedSegment {
        body: inputs.index_md.to_owned(),
        included: Vec::new(),
        excluded: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Rfc3339Timestamp;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn empty_inputs<'a>(index: &'a str) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: index,
            pinned_candidates: &[],
            project_candidates: &[],
            playbook_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn passes_index_md_through_verbatim() {
        let s = select(&empty_inputs("# Index\n- a.md\n"));
        assert_eq!(s.body, "# Index\n- a.md\n");
    }
}
```

- [ ] **Step 4: Wire into `mod.rs`**

Edit `crates/cairn-core/src/verbs/assemble_hot/mod.rs` — add `pub mod sources;`.

- [ ] **Step 5: Run tests**

Run: `cargo nextest run -p cairn-core --locked verbs::assemble_hot::sources`
Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/sources \
        crates/cairn-core/src/verbs/assemble_hot/mod.rs
git commit -m "feat(assemble_hot): purpose + index pass-through sources (issue #82)"
```

---

## Task 6: Body framing helper

**Files:**
- Create: `crates/cairn-core/src/verbs/assemble_hot/sources/render.rs`
- Modify: `crates/cairn-core/src/verbs/assemble_hot/sources/mod.rs`

- [ ] **Step 1: Write the renderer + tests**

Create `crates/cairn-core/src/verbs/assemble_hot/sources/render.rs`:

```rust
//! Per-record body framing shared by every record-backed source.

use crate::domain::record::MemoryRecord;

/// Render one record into the canonical hot-prefix block:
///
/// ```text
/// ## <kind>: <first-body-line>
/// <body>
///
/// ```
///
/// Trailing blank line separates blocks. Identical for every source so
/// downstream consumers never have to special-case ranking origin.
#[must_use]
pub fn render_record_block(record: &MemoryRecord) -> String {
    let first_line = record.body.lines().next().unwrap_or_default();
    let mut out = String::with_capacity(record.body.len() + 64);
    out.push_str("## ");
    out.push_str(record.kind.as_str());
    out.push_str(": ");
    out.push_str(first_line);
    out.push('\n');
    out.push_str(&record.body);
    if !record.body.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;

    #[test]
    fn renders_canonical_block() {
        let r = sample_record();
        let out = render_record_block(&r);
        assert!(out.starts_with("## user: user prefers dark mode\n"));
        assert!(out.ends_with("\n\n"));
        assert!(out.contains("user prefers dark mode"));
    }

    #[test]
    fn handles_record_body_without_trailing_newline() {
        let mut r = sample_record();
        r.body = "single line".to_owned();
        let out = render_record_block(&r);
        assert!(out.ends_with("\n\n"), "must always end with blank line");
    }
}
```

- [ ] **Step 2: Add module to `sources/mod.rs`**

Edit `crates/cairn-core/src/verbs/assemble_hot/sources/mod.rs`:

```rust
pub mod index;
pub mod pinned;
pub mod playbook;
pub mod project;
pub mod purpose;
pub mod render;
pub mod user_signal;
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-core --locked verbs::assemble_hot::sources::render`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/sources/render.rs \
        crates/cairn-core/src/verbs/assemble_hot/sources/mod.rs
git commit -m "feat(assemble_hot): canonical record-body framing helper (issue #82)"
```

---

## Task 7: PinnedFeedback source

**Files:**
- Create: `crates/cairn-core/src/verbs/assemble_hot/sources/pinned.rs`

- [ ] **Step 1: Write the failing tests + source**

Create `crates/cairn-core/src/verbs/assemble_hot/sources/pinned.rs`:

```rust
//! `PinnedFeedback` source — top 8 `user`/`feedback` records ranked by
//! `salience × recency_decay(now − updated_at)`.
//!
//! Pin semantics for v0.1: caller pre-filters to records with
//! `kind ∈ {user, feedback} ∧ is_static = 1`. Core re-checks the
//! `kind` half (signed payload) but trusts the caller for `is_static`
//! (store-side projection, not on `MemoryRecord`). See spec
//! "Pin semantics" + design brief §7.1.

use crate::domain::Rfc3339Timestamp;
use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use crate::verbs::assemble_hot::admissibility::admit;
use crate::verbs::assemble_hot::inclusion::{
    ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment,
};
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

use super::render::render_record_block;

/// Top-K cap from brief §7 ("top 8 by salience × recency").
const TOP_K: usize = 8;

/// Half-life for the recency decay term, in seconds.
/// 30 days × 86400 s/day. Matches the brief's "salience × recency"
/// shorthand by giving recent records a non-trivial multiplier without
/// fully cliff-sliding old high-salience records.
const RECENCY_HALF_LIFE_SECS: f64 = 30.0 * 86_400.0;

/// Select up to 8 pinned-feedback records for the hot prefix.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    let mut included_with_score: Vec<(InclusionTrace, &MemoryRecord)> = Vec::new();
    let mut excluded: Vec<ExclusionTrace> = Vec::new();

    for &record in inputs.pinned_candidates {
        if !is_pinned_kind(record.kind) {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason: ExclusionReason::NotPinned,
            });
            continue;
        }
        if let Err(reason) =
            admit(record, &inputs.scope, inputs.authorized_visibility)
        {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason,
            });
            continue;
        }
        let score = pin_score(record, &inputs.now);
        included_with_score.push((
            InclusionTrace {
                record_id: record.id.clone(),
                score,
                note: "salience × recency",
            },
            record,
        ));
    }

    // Sort: score desc, then record_id desc as deterministic tiebreaker.
    included_with_score.sort_by(|a, b| {
        b.0.score
            .partial_cmp(&a.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.0.record_id.as_str().cmp(a.0.record_id.as_str()))
    });

    // Bucket overflow into BeyondTopK exclusions before truncation so
    // the debug trace explains why a candidate did not make the cut.
    let overflow = included_with_score.split_off(TOP_K.min(included_with_score.len()));
    for (trace, _) in overflow {
        excluded.push(ExclusionTrace {
            record_id: trace.record_id,
            reason: ExclusionReason::BeyondTopK,
        });
    }

    let mut body = String::new();
    let mut included: Vec<InclusionTrace> = Vec::with_capacity(included_with_score.len());
    for (trace, record) in included_with_score {
        body.push_str(&render_record_block(record));
        included.push(trace);
    }

    LoadedSegment {
        body,
        included,
        excluded,
    }
}

fn is_pinned_kind(kind: MemoryKind) -> bool {
    matches!(kind, MemoryKind::User | MemoryKind::Feedback)
}

fn pin_score(record: &MemoryRecord, now: &Rfc3339Timestamp) -> f64 {
    let age_secs = age_seconds(now, &record.updated_at);
    let decay = (-age_secs / RECENCY_HALF_LIFE_SECS).exp();
    f64::from(record.salience) * decay
}

fn age_seconds(now: &Rfc3339Timestamp, updated_at: &Rfc3339Timestamp) -> f64 {
    // Negative ages (record stamped slightly in the future relative
    // to `now`) clamp to zero so decay never blows up past 1.0.
    let now_dt = now.as_chrono();
    let upd_dt = updated_at.as_chrono();
    let secs = (now_dt - upd_dt).num_seconds().max(0);
    secs as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn fresh_input<'a>(records: &'a [&'a MemoryRecord], now: &str) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: records,
            project_candidates: &[],
            playbook_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse(now).expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    fn user_record(id: &str, salience: f32, updated_at: &str) -> MemoryRecord {
        let mut r = sample_record();
        r.id = crate::domain::RecordId::parse(id).expect("valid id");
        r.target_id = crate::domain::TargetId::parse(id).expect("valid target");
        r.kind = MemoryKind::User;
        r.salience = salience;
        r.updated_at = Rfc3339Timestamp::parse(updated_at).expect("valid");
        r
    }

    #[test]
    fn ranks_by_salience_times_recency() {
        let recent_low = user_record(
            "01HQZX9F5N0000000000000001",
            0.4,
            "2026-04-22T14:00:00Z",
        );
        let old_high = user_record(
            "01HQZX9F5N0000000000000002",
            0.9,
            "2025-04-22T14:00:00Z",
        );
        let now = "2026-04-22T15:00:00Z";
        let inputs = fresh_input(&[&recent_low, &old_high], now);
        let s = select(&inputs);
        // 0.4 × ~1.0 = 0.4 ; 0.9 × exp(-365/30) ≈ 0.9 × 5.7e-6 ≈ 5e-6.
        // Recent low-salience must win.
        assert_eq!(s.included[0].record_id, recent_low.id);
        assert_eq!(s.included[1].record_id, old_high.id);
    }

    #[test]
    fn caps_at_top_8_and_emits_beyond_top_k() {
        let recs: Vec<MemoryRecord> = (1..=12)
            .map(|i| {
                user_record(
                    &format!("01HQZX9F5N00000000000000{:02}", i),
                    f32::from(i) / 12.0,
                    "2026-04-22T14:00:00Z",
                )
            })
            .collect();
        let refs: Vec<&MemoryRecord> = recs.iter().collect();
        let s = select(&fresh_input(&refs, "2026-04-22T15:00:00Z"));
        assert_eq!(s.included.len(), 8);
        let beyond = s
            .excluded
            .iter()
            .filter(|e| e.reason == ExclusionReason::BeyondTopK)
            .count();
        assert_eq!(beyond, 4);
    }

    #[test]
    fn excludes_non_user_feedback_kind_with_not_pinned() {
        let mut r = user_record("01HQZX9F5N0000000000000001", 0.9, "2026-04-22T14:00:00Z");
        r.kind = MemoryKind::Project;
        let s = select(&fresh_input(&[&r], "2026-04-22T15:00:00Z"));
        assert!(s.included.is_empty());
        assert_eq!(s.excluded.len(), 1);
        assert_eq!(s.excluded[0].reason, ExclusionReason::NotPinned);
    }

    #[test]
    fn excludes_low_confidence() {
        let mut r = user_record("01HQZX9F5N0000000000000001", 0.9, "2026-04-22T14:00:00Z");
        r.confidence = 0.2;
        let s = select(&fresh_input(&[&r], "2026-04-22T15:00:00Z"));
        assert_eq!(s.excluded[0].reason, ExclusionReason::BelowConfidenceFloor);
    }

    #[test]
    fn deterministic_tiebreaker_on_equal_score() {
        let a = user_record("01HQZX9F5N0000000000000001", 0.5, "2026-04-22T14:00:00Z");
        let b = user_record("01HQZX9F5N0000000000000002", 0.5, "2026-04-22T14:00:00Z");
        let s = select(&fresh_input(&[&a, &b], "2026-04-22T15:00:00Z"));
        // Tiebreaker: record_id desc → id ending …02 first, then …01.
        assert_eq!(s.included[0].record_id, b.id);
        assert_eq!(s.included[1].record_id, a.id);
    }

    #[test]
    fn future_timestamp_clamps_age_to_zero() {
        let r = user_record(
            "01HQZX9F5N0000000000000001",
            0.5,
            "2026-04-22T15:00:01Z", // 1s after now
        );
        let s = select(&fresh_input(&[&r], "2026-04-22T15:00:00Z"));
        // No panic, no NaN — the record is included with score == salience.
        assert_eq!(s.included.len(), 1);
        assert!((s.included[0].score - 0.5).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p cairn-core --locked verbs::assemble_hot::sources::pinned`
Expected: 6 tests pass.

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: no warnings. If `clippy::cast_precision_loss` fires for `secs as f64`, allow it locally with a one-line reason or use `f64::from(i32::try_from(secs).unwrap_or(i32::MAX))`.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/sources/pinned.rs
git commit -m "feat(assemble_hot): pinned-feedback source (issue #82)"
```

---

## Task 8: TopSalienceProject source

**Files:**
- Create: `crates/cairn-core/src/verbs/assemble_hot/sources/project.rs`

- [ ] **Step 1: Write the source + tests**

Create `crates/cairn-core/src/verbs/assemble_hot/sources/project.rs`:

```rust
//! `TopSalienceProject` source — top 6 `project`-kind records by salience.
//!
//! Sort key matches the lint canary regression
//! (`tied_salience_top_k_picks_largest_records_conservatively`):
//! salience desc, then byte size desc as tiebreaker, then record_id desc
//! to keep the assembled prefix bytewise-deterministic.

use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use crate::verbs::assemble_hot::admissibility::admit;
use crate::verbs::assemble_hot::inclusion::{
    ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment,
};
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

use super::render::render_record_block;

const TOP_K: usize = 6;

/// Select up to 6 project records ranked by salience.
#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    let mut admissible: Vec<(InclusionTrace, &MemoryRecord)> = Vec::new();
    let mut excluded: Vec<ExclusionTrace> = Vec::new();

    for &record in inputs.project_candidates {
        if record.kind != MemoryKind::Project {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason: ExclusionReason::NotPinned,
            });
            continue;
        }
        if let Err(reason) = admit(record, &inputs.scope, inputs.authorized_visibility) {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason,
            });
            continue;
        }
        admissible.push((
            InclusionTrace {
                record_id: record.id.clone(),
                score: f64::from(record.salience),
                note: "salience desc",
            },
            record,
        ));
    }

    admissible.sort_by(|a, b| {
        b.0.score
            .partial_cmp(&a.0.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.1.body.len().cmp(&a.1.body.len()))
            .then_with(|| b.0.record_id.as_str().cmp(a.0.record_id.as_str()))
    });

    let overflow = admissible.split_off(TOP_K.min(admissible.len()));
    for (trace, _) in overflow {
        excluded.push(ExclusionTrace {
            record_id: trace.record_id,
            reason: ExclusionReason::BeyondTopK,
        });
    }

    let mut body = String::new();
    let mut included: Vec<InclusionTrace> = Vec::with_capacity(admissible.len());
    for (trace, record) in admissible {
        body.push_str(&render_record_block(record));
        included.push(trace);
    }

    LoadedSegment { body, included, excluded }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Rfc3339Timestamp;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn project_record(id: &str, salience: f32, body: &str) -> MemoryRecord {
        let mut r = sample_record();
        r.id = crate::domain::RecordId::parse(id).expect("valid");
        r.target_id = crate::domain::TargetId::parse(id).expect("valid");
        r.kind = MemoryKind::Project;
        r.salience = salience;
        r.body = body.to_owned();
        r
    }

    fn input_with<'a>(records: &'a [&'a MemoryRecord]) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &[],
            project_candidates: records,
            playbook_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn caps_at_top_6() {
        let recs: Vec<MemoryRecord> = (1..=10)
            .map(|i| {
                project_record(
                    &format!("01HQZX9F5N00000000000000{:02}", i),
                    f32::from(i) / 10.0,
                    "body",
                )
            })
            .collect();
        let refs: Vec<&MemoryRecord> = recs.iter().collect();
        let s = select(&input_with(&refs));
        assert_eq!(s.included.len(), 6);
    }

    #[test]
    fn rejects_non_project_kind_with_not_pinned() {
        let mut r = project_record("01HQZX9F5N0000000000000001", 0.9, "x");
        r.kind = MemoryKind::Feedback;
        let s = select(&input_with(&[&r]));
        assert!(s.included.is_empty());
        assert_eq!(s.excluded[0].reason, ExclusionReason::NotPinned);
    }

    #[test]
    fn ties_break_by_body_size_then_record_id() {
        // Six records with identical salience, two of them larger.
        // Top-6 must include the larger ones; the seventh tied record
        // is BeyondTopK.
        let small1 = project_record("01HQZX9F5N0000000000000001", 0.5, "x");
        let small2 = project_record("01HQZX9F5N0000000000000002", 0.5, "x");
        let small3 = project_record("01HQZX9F5N0000000000000003", 0.5, "x");
        let small4 = project_record("01HQZX9F5N0000000000000004", 0.5, "x");
        let small5 = project_record("01HQZX9F5N0000000000000005", 0.5, "x");
        let large1 = project_record("01HQZX9F5N0000000000000006", 0.5, &"L".repeat(100));
        let large2 = project_record("01HQZX9F5N0000000000000007", 0.5, &"L".repeat(100));
        let recs = [&small1, &small2, &small3, &small4, &small5, &large1, &large2];
        let s = select(&input_with(&recs));
        let included_ids: Vec<&str> = s.included.iter().map(|i| i.record_id.as_str()).collect();
        // Both larges must be included.
        assert!(included_ids.contains(&large1.id.as_str()));
        assert!(included_ids.contains(&large2.id.as_str()));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p cairn-core --locked verbs::assemble_hot::sources::project`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/sources/project.rs
git commit -m "feat(assemble_hot): top-salience project source (issue #82)"
```

---

## Task 9: ActivePlaybook source

**Files:**
- Create: `crates/cairn-core/src/verbs/assemble_hot/sources/playbook.rs`

- [ ] **Step 1: Write the source + tests**

Create `crates/cairn-core/src/verbs/assemble_hot/sources/playbook.rs`:

```rust
//! `ActivePlaybook` source — single most-recently-updated `playbook` record.

use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use crate::verbs::assemble_hot::admissibility::admit;
use crate::verbs::assemble_hot::inclusion::{
    ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment,
};
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

use super::render::render_record_block;

const TOP_K: usize = 1;

#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    let mut admissible: Vec<(InclusionTrace, &MemoryRecord)> = Vec::new();
    let mut excluded: Vec<ExclusionTrace> = Vec::new();

    for &record in inputs.playbook_candidates {
        if record.kind != MemoryKind::Playbook {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason: ExclusionReason::NotPinned,
            });
            continue;
        }
        if let Err(reason) = admit(record, &inputs.scope, inputs.authorized_visibility) {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason,
            });
            continue;
        }
        admissible.push((
            InclusionTrace {
                record_id: record.id.clone(),
                score: 0.0,
                note: "most recent updated_at",
            },
            record,
        ));
    }

    admissible.sort_by(|a, b| {
        b.1.updated_at
            .cmp_chronological(&a.1.updated_at)
            .then_with(|| b.0.record_id.as_str().cmp(a.0.record_id.as_str()))
    });

    let overflow = admissible.split_off(TOP_K.min(admissible.len()));
    for (trace, _) in overflow {
        excluded.push(ExclusionTrace {
            record_id: trace.record_id,
            reason: ExclusionReason::BeyondTopK,
        });
    }

    let mut body = String::new();
    let mut included: Vec<InclusionTrace> = Vec::with_capacity(admissible.len());
    for (trace, record) in admissible {
        body.push_str(&render_record_block(record));
        included.push(trace);
    }

    LoadedSegment { body, included, excluded }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Rfc3339Timestamp;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn playbook_record(id: &str, updated_at: &str) -> MemoryRecord {
        let mut r = sample_record();
        r.id = crate::domain::RecordId::parse(id).expect("valid");
        r.target_id = crate::domain::TargetId::parse(id).expect("valid");
        r.kind = MemoryKind::Playbook;
        r.updated_at = Rfc3339Timestamp::parse(updated_at).expect("valid");
        r
    }

    fn input_with<'a>(records: &'a [&'a MemoryRecord]) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &[],
            project_candidates: &[],
            playbook_candidates: records,
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn most_recent_wins() {
        let old = playbook_record("01HQZX9F5N0000000000000001", "2026-04-20T12:00:00Z");
        let new = playbook_record("01HQZX9F5N0000000000000002", "2026-04-22T14:00:00Z");
        let s = select(&input_with(&[&old, &new]));
        assert_eq!(s.included.len(), 1);
        assert_eq!(s.included[0].record_id, new.id);
        assert_eq!(s.excluded.len(), 1);
        assert_eq!(s.excluded[0].reason, ExclusionReason::BeyondTopK);
    }

    #[test]
    fn empty_input_emits_empty_body() {
        let s = select(&input_with(&[]));
        assert!(s.body.is_empty());
        assert!(s.included.is_empty());
        assert!(s.excluded.is_empty());
    }

    #[test]
    fn rejects_non_playbook_kind() {
        let mut r = playbook_record("01HQZX9F5N0000000000000001", "2026-04-22T14:00:00Z");
        r.kind = MemoryKind::Project;
        let s = select(&input_with(&[&r]));
        assert!(s.included.is_empty());
        assert_eq!(s.excluded[0].reason, ExclusionReason::NotPinned);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p cairn-core --locked verbs::assemble_hot::sources::playbook`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/sources/playbook.rs
git commit -m "feat(assemble_hot): active-playbook source (issue #82)"
```

---

## Task 10: RecentUserSignal source

**Files:**
- Create: `crates/cairn-core/src/verbs/assemble_hot/sources/user_signal.rs`

- [ ] **Step 1: Write the source + tests**

Create `crates/cairn-core/src/verbs/assemble_hot/sources/user_signal.rs`:

```rust
//! `RecentUserSignal` source — `user_signal` records inside the last 24h.

use crate::domain::Rfc3339Timestamp;
use crate::domain::record::MemoryRecord;
use crate::domain::taxonomy::MemoryKind;
use crate::verbs::assemble_hot::admissibility::admit;
use crate::verbs::assemble_hot::inclusion::{
    ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment,
};
use crate::verbs::assemble_hot::inputs::HotMemoryInputs;

use super::render::render_record_block;

const RECENCY_WINDOW_SECS: i64 = 24 * 60 * 60;

#[must_use]
pub fn select(inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    let now_dt = inputs.now.as_chrono();
    let mut admissible: Vec<(InclusionTrace, &MemoryRecord)> = Vec::new();
    let mut excluded: Vec<ExclusionTrace> = Vec::new();

    for &record in inputs.user_signal_candidates {
        if record.kind != MemoryKind::UserSignal {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason: ExclusionReason::NotPinned,
            });
            continue;
        }
        if let Err(reason) = admit(record, &inputs.scope, inputs.authorized_visibility) {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason,
            });
            continue;
        }
        if !within_window(&record.updated_at, &now_dt) {
            excluded.push(ExclusionTrace {
                record_id: record.id.clone(),
                reason: ExclusionReason::OutsideRecencyWindow,
            });
            continue;
        }
        admissible.push((
            InclusionTrace {
                record_id: record.id.clone(),
                score: 0.0,
                note: "last 24h, updated_at desc",
            },
            record,
        ));
    }

    admissible.sort_by(|a, b| {
        b.1.updated_at
            .cmp_chronological(&a.1.updated_at)
            .then_with(|| b.0.record_id.as_str().cmp(a.0.record_id.as_str()))
    });

    let mut body = String::new();
    let mut included: Vec<InclusionTrace> = Vec::with_capacity(admissible.len());
    for (trace, record) in admissible {
        body.push_str(&render_record_block(record));
        included.push(trace);
    }

    LoadedSegment { body, included, excluded }
}

fn within_window(
    updated_at: &Rfc3339Timestamp,
    now_dt: &chrono::DateTime<chrono::Utc>,
) -> bool {
    let upd_dt = updated_at.as_chrono();
    let age = (*now_dt - upd_dt).num_seconds();
    (0..=RECENCY_WINDOW_SECS).contains(&age)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::record::tests_export::sample_record;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn signal(id: &str, updated_at: &str) -> MemoryRecord {
        let mut r = sample_record();
        r.id = crate::domain::RecordId::parse(id).expect("valid");
        r.target_id = crate::domain::TargetId::parse(id).expect("valid");
        r.kind = MemoryKind::UserSignal;
        r.updated_at = Rfc3339Timestamp::parse(updated_at).expect("valid");
        r
    }

    fn input_with<'a>(records: &'a [&'a MemoryRecord], now: &str) -> HotMemoryInputs<'a> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &[],
            project_candidates: &[],
            playbook_candidates: &[],
            user_signal_candidates: records,
            now: Rfc3339Timestamp::parse(now).expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn includes_record_at_window_boundary() {
        // 2026-04-21T15:00:00Z → 2026-04-22T15:00:00Z is exactly 86_400 s.
        let r = signal("01HQZX9F5N0000000000000001", "2026-04-21T15:00:00Z");
        let s = select(&input_with(&[&r], "2026-04-22T15:00:00Z"));
        assert_eq!(s.included.len(), 1);
    }

    #[test]
    fn excludes_record_one_second_past_window() {
        let r = signal("01HQZX9F5N0000000000000001", "2026-04-21T14:59:59Z");
        let s = select(&input_with(&[&r], "2026-04-22T15:00:00Z"));
        assert!(s.included.is_empty());
        assert_eq!(s.excluded[0].reason, ExclusionReason::OutsideRecencyWindow);
    }

    #[test]
    fn rejects_non_signal_kind() {
        let mut r = signal("01HQZX9F5N0000000000000001", "2026-04-22T14:59:59Z");
        r.kind = MemoryKind::Project;
        let s = select(&input_with(&[&r], "2026-04-22T15:00:00Z"));
        assert!(s.included.is_empty());
        assert_eq!(s.excluded[0].reason, ExclusionReason::NotPinned);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo nextest run -p cairn-core --locked verbs::assemble_hot::sources::user_signal`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/sources/user_signal.rs
git commit -m "feat(assemble_hot): recent-user-signal source (issue #82)"
```

---

## Task 11: Refactor assembler to take HotMemoryInputs + emit debug

**Files:**
- Modify: `crates/cairn-core/src/verbs/assemble_hot/assembler.rs`
- Modify: `crates/cairn-core/src/verbs/assemble_hot/mod.rs`
- Modify: `crates/cairn-core/tests/assemble_hot_snapshots.rs` (existing test that uses old assembler)

- [ ] **Step 1: Read the existing test that depends on the old signature**

Run: `cat crates/cairn-core/tests/assemble_hot_snapshots.rs`
Expected: identifies the call sites that pass `&HotMemoryConfig` directly.

- [ ] **Step 2: Rewrite `assembler.rs`**

Replace `crates/cairn-core/src/verbs/assemble_hot/assembler.rs` with:

```rust
//! `HotMemoryAssembler` — pure top-level entry point (issue #82).
//!
//! Walks `config.recipe`, dispatches each step to its source-specific
//! `select`, glues the bodies together via `build_segments`, validates
//! the wire shape, and (when `inputs.include_debug` is true) emits a
//! `HotStepTrace` per step.

use crate::config::HotMemoryConfig;
use crate::generated::verbs::assemble_hot::{
    AssembleHotData, HotExclusion, HotExclusionReason, HotInclusion, HotRecipeStep, HotStepTrace,
    HotMemoryDebug,
};

use super::inclusion::{ExclusionReason, ExclusionTrace, InclusionTrace, LoadedSegment};
use super::inputs::HotMemoryInputs;
use super::segments::{AssembleHotValidationError, build_segments, validate};
use super::sources;

/// Errors returned by [`assemble_hot`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AssembleHotError {
    /// Segment construction or validation failed.
    #[error("segment construction: {0}")]
    Segments(#[from] AssembleHotValidationError),
    /// The assembled prefix exceeds the vault's configured `max_bytes`.
    #[error("hot memory exceeded budget: {got} > {max} bytes")]
    BudgetExceeded {
        /// Actual prefix length.
        got: u64,
        /// Configured `HotMemoryConfig.max_bytes`.
        max: u64,
    },
}

/// Run the recipe and emit a validated `AssembleHotData`. Pure: same
/// inputs always produce the same output.
pub fn assemble_hot(
    inputs: &HotMemoryInputs<'_>,
    config: &HotMemoryConfig,
) -> Result<AssembleHotData, AssembleHotError> {
    let recipe: Vec<HotRecipeStep> = config
        .recipe
        .iter()
        .copied()
        .map(HotRecipeStep::from)
        .collect();

    let mut bodies: Vec<String> = Vec::with_capacity(recipe.len());
    let mut traces: Vec<HotStepTrace> = Vec::with_capacity(recipe.len());

    for step in recipe.iter().copied() {
        let segment = run_step(step, inputs);
        if inputs.include_debug {
            traces.push(to_step_trace(step, &segment));
        }
        bodies.push(segment.body);
    }

    let bodies_refs: Vec<&str> = bodies.iter().map(String::as_str).collect();
    let (prefix, segments) = build_segments(&recipe, &bodies_refs)?;

    let bytes = prefix.len() as u64;
    let max = u64::from(config.max_bytes);
    if bytes > max {
        return Err(AssembleHotError::BudgetExceeded { got: bytes, max });
    }

    let debug = if inputs.include_debug {
        Some(HotMemoryDebug { steps: traces })
    } else {
        None
    };

    let data = AssembleHotData {
        bytes,
        prefix,
        segments: Some(segments),
        debug,
    };
    validate(&data)?;
    Ok(data)
}

fn run_step(step: HotRecipeStep, inputs: &HotMemoryInputs<'_>) -> LoadedSegment {
    match step {
        HotRecipeStep::Purpose => sources::purpose::select(inputs),
        HotRecipeStep::Index => sources::index::select(inputs),
        HotRecipeStep::PinnedFeedback => sources::pinned::select(inputs),
        HotRecipeStep::TopSalienceProject => sources::project::select(inputs),
        HotRecipeStep::ActivePlaybook => sources::playbook::select(inputs),
        HotRecipeStep::RecentUserSignal => sources::user_signal::select(inputs),
    }
}

fn to_step_trace(step: HotRecipeStep, segment: &LoadedSegment) -> HotStepTrace {
    HotStepTrace {
        step,
        included: segment.included.iter().map(to_inclusion).collect(),
        excluded: segment.excluded.iter().map(to_exclusion).collect(),
    }
}

fn to_inclusion(trace: &InclusionTrace) -> HotInclusion {
    HotInclusion {
        record_id: trace.record_id.as_str().to_owned(),
        score: trace.score,
        note: trace.note.to_owned(),
    }
}

fn to_exclusion(trace: &ExclusionTrace) -> HotExclusion {
    HotExclusion {
        record_id: trace.record_id.as_str().to_owned(),
        reason: to_wire_reason(trace.reason),
    }
}

fn to_wire_reason(reason: ExclusionReason) -> HotExclusionReason {
    match reason {
        ExclusionReason::Tombstoned => HotExclusionReason::Tombstoned,
        ExclusionReason::ForgottenScope => HotExclusionReason::ForgottenScope,
        ExclusionReason::BelowConfidenceFloor => HotExclusionReason::BelowConfidenceFloor,
        ExclusionReason::OutOfScope => HotExclusionReason::OutOfScope,
        ExclusionReason::VisibilityDenied => HotExclusionReason::VisibilityDenied,
        ExclusionReason::OutsideRecencyWindow => HotExclusionReason::OutsideRecencyWindow,
        ExclusionReason::BeyondTopK => HotExclusionReason::BeyondTopK,
        ExclusionReason::NotPinned => HotExclusionReason::NotPinned,
        ExclusionReason::EmptyBody => HotExclusionReason::EmptyBody,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotMemoryConfig;
    use crate::domain::Rfc3339Timestamp;
    use crate::domain::scope::ScopeTuple;
    use crate::domain::taxonomy::MemoryVisibility;

    fn empty_inputs() -> HotMemoryInputs<'static> {
        HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &[],
            project_candidates: &[],
            playbook_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        }
    }

    #[test]
    fn default_recipe_with_empty_inputs_returns_zero_bytes() {
        let cfg = HotMemoryConfig::default();
        let data = assemble_hot(&empty_inputs(), &cfg).unwrap();
        assert_eq!(data.prefix, "");
        assert_eq!(data.bytes, 0);
        assert_eq!(data.debug, None);
    }

    #[test]
    fn budget_exceeded_returns_typed_error() {
        let cfg = HotMemoryConfig {
            max_bytes: 8,
            ..HotMemoryConfig::default()
        };
        let mut inputs = empty_inputs();
        let big = "AAAA".repeat(8); // 32 bytes
        inputs.purpose_md = &big;
        let err = assemble_hot(&inputs, &cfg).unwrap_err();
        match err {
            AssembleHotError::BudgetExceeded { got, max } => {
                assert_eq!(got, 32);
                assert_eq!(max, 8);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn include_debug_populates_per_step_trace() {
        let cfg = HotMemoryConfig::default();
        let mut inputs = empty_inputs();
        inputs.include_debug = true;
        let data = assemble_hot(&inputs, &cfg).unwrap();
        let debug = data.debug.expect("debug populated");
        assert_eq!(debug.steps.len(), cfg.recipe.len());
    }

    #[test]
    fn include_debug_false_omits_field() {
        let cfg = HotMemoryConfig::default();
        let data = assemble_hot(&empty_inputs(), &cfg).unwrap();
        assert!(data.debug.is_none());
    }
}
```

- [ ] **Step 3: Update existing snapshot test**

Read `crates/cairn-core/tests/assemble_hot_snapshots.rs` first:

Run: `cat crates/cairn-core/tests/assemble_hot_snapshots.rs`

Then rewrite the call sites that previously used `assemble_hot(&cfg)` /
`assemble_hot_with_loader(&cfg, |_| Ok(...))` to construct
`HotMemoryInputs` and pass it positionally.

For an empty-input snapshot:

```rust
let inputs = HotMemoryInputs {
    purpose_md: "purpose body\n",
    index_md: "index body\n",
    pinned_candidates: &[],
    project_candidates: &[],
    playbook_candidates: &[],
    user_signal_candidates: &[],
    now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").expect("valid"),
    scope: ScopeTuple::default(),
    authorized_visibility: &[MemoryVisibility::Private],
    include_debug: false,
};
let data = assemble_hot(&inputs, &cfg).expect("assemble");
insta::assert_yaml_snapshot!(data);
```

If the prior snapshot file fails: regenerate via `cargo insta review`.

- [ ] **Step 4: Run all assemble_hot tests**

Run: `cargo nextest run -p cairn-core --locked verbs::assemble_hot`
Expected: every source test + 4 new assembler tests pass.

- [ ] **Step 5: Run snapshot test**

Run: `cargo nextest run -p cairn-core --locked --test assemble_hot_snapshots`
Expected: tests pass after `cargo insta accept` if there is intentional drift.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/verbs/assemble_hot/assembler.rs \
        crates/cairn-core/src/verbs/assemble_hot/mod.rs \
        crates/cairn-core/tests/assemble_hot_snapshots.rs \
        crates/cairn-core/tests/snapshots/
git commit -m "feat(assemble_hot): refactor assembler to take HotMemoryInputs (issue #82)"
```

---

## Task 12: Privacy + integration tests

**Files:**
- Create: `crates/cairn-core/tests/assemble_hot_inputs.rs`
- Create: `crates/cairn-core/tests/assemble_hot_privacy.rs`
- Create: `crates/cairn-core/tests/assemble_hot_debug.rs`

- [ ] **Step 1: Write the integration test**

Create `crates/cairn-core/tests/assemble_hot_inputs.rs`:

```rust
//! End-to-end integration: default-recipe assembly with mixed-kind
//! fixtures. Asserts the prefix bytes stay under budget and every
//! recipe step contributes exactly one segment.

use cairn_core::config::HotMemoryConfig;
use cairn_core::domain::Rfc3339Timestamp;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::scope::ScopeTuple;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::verbs::assemble_hot::{HotMemoryInputs, assemble_hot};

#[test]
fn default_recipe_with_mixed_records_stays_within_budget() {
    let mut user = sample_record();
    user.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000001").unwrap();
    user.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000001").unwrap();
    user.kind = MemoryKind::User;
    let mut project = sample_record();
    project.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000002").unwrap();
    project.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000002").unwrap();
    project.kind = MemoryKind::Project;
    let mut playbook = sample_record();
    playbook.id =
        cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000003").unwrap();
    playbook.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000003").unwrap();
    playbook.kind = MemoryKind::Playbook;
    let mut signal = sample_record();
    signal.id = cairn_core::domain::RecordId::parse("01HQZX9F5N0000000000000004").unwrap();
    signal.target_id =
        cairn_core::domain::TargetId::parse("01HQZX9F5N0000000000000004").unwrap();
    signal.kind = MemoryKind::UserSignal;

    let pinned = [&user];
    let projects = [&project];
    let playbooks = [&playbook];
    let signals = [&signal];

    let inputs = HotMemoryInputs {
        purpose_md: "# Purpose\nact as a careful agent.\n",
        index_md: "# Index\n- a.md\n- b.md\n",
        pinned_candidates: &pinned,
        project_candidates: &projects,
        playbook_candidates: &playbooks,
        user_signal_candidates: &signals,
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").unwrap(),
        scope: ScopeTuple::default(),
        authorized_visibility: &[MemoryVisibility::Private],
        include_debug: false,
    };
    let cfg = HotMemoryConfig::default();
    let data = assemble_hot(&inputs, &cfg).expect("assemble");

    assert!(data.bytes <= u64::from(cfg.max_bytes));
    let segments = data.segments.expect("segments emitted");
    assert_eq!(segments.len(), cfg.recipe.len());
    assert!(data.prefix.contains("user prefers dark mode"));
}
```

- [ ] **Step 2: Write the privacy test**

Create `crates/cairn-core/tests/assemble_hot_privacy.rs`:

```rust
//! Privacy regressions: tombstone, low-confidence, scope mismatch, and
//! visibility denials must exclude records across every source.

use cairn_core::config::HotMemoryConfig;
use cairn_core::domain::Rfc3339Timestamp;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::scope::ScopeTuple;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::verbs::assemble_hot::{HotMemoryInputs, assemble_hot};

fn make_input<'a>(
    pinned: &'a [&'a cairn_core::domain::record::MemoryRecord],
    visibility: &'a [MemoryVisibility],
    scope: ScopeTuple,
) -> HotMemoryInputs<'a> {
    HotMemoryInputs {
        purpose_md: "",
        index_md: "",
        pinned_candidates: pinned,
        project_candidates: &[],
        playbook_candidates: &[],
        user_signal_candidates: &[],
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").unwrap(),
        scope,
        authorized_visibility: visibility,
        include_debug: true,
    }
}

#[test]
fn pinned_record_with_low_confidence_does_not_appear() {
    let mut r = sample_record();
    r.kind = MemoryKind::User;
    r.confidence = 0.1;
    let cfg = HotMemoryConfig::default();
    let pinned = [&r];
    let inputs = make_input(
        &pinned,
        &[MemoryVisibility::Private],
        ScopeTuple::default(),
    );
    let data = assemble_hot(&inputs, &cfg).unwrap();
    assert!(!data.prefix.contains(&r.body));
}

#[test]
fn pinned_record_with_visibility_denial_does_not_appear() {
    let mut r = sample_record();
    r.kind = MemoryKind::User;
    r.visibility = MemoryVisibility::Org;
    let cfg = HotMemoryConfig::default();
    let pinned = [&r];
    let inputs = make_input(&pinned, &[MemoryVisibility::Private], ScopeTuple::default());
    let data = assemble_hot(&inputs, &cfg).unwrap();
    assert!(!data.prefix.contains(&r.body));
}

#[test]
fn pinned_record_with_scope_mismatch_does_not_appear() {
    let mut r = sample_record();
    r.kind = MemoryKind::User;
    let cfg = HotMemoryConfig::default();
    let pinned = [&r];
    let other_user = ScopeTuple {
        user: Some("hmn:other".to_owned()),
        ..ScopeTuple::default()
    };
    let inputs = make_input(&pinned, &[MemoryVisibility::Private], other_user);
    let data = assemble_hot(&inputs, &cfg).unwrap();
    assert!(!data.prefix.contains(&r.body));
}
```

- [ ] **Step 3: Write the debug-output test**

Create `crates/cairn-core/tests/assemble_hot_debug.rs`:

```rust
//! `include_debug` round-trip: every recipe step emits a `HotStepTrace`,
//! and an excluded record carries a typed reason.

use cairn_core::config::HotMemoryConfig;
use cairn_core::domain::Rfc3339Timestamp;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::scope::ScopeTuple;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::generated::verbs::assemble_hot::HotExclusionReason;
use cairn_core::verbs::assemble_hot::{HotMemoryInputs, assemble_hot};

#[test]
fn debug_field_present_when_requested() {
    let cfg = HotMemoryConfig::default();
    let inputs = HotMemoryInputs {
        purpose_md: "",
        index_md: "",
        pinned_candidates: &[],
        project_candidates: &[],
        playbook_candidates: &[],
        user_signal_candidates: &[],
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").unwrap(),
        scope: ScopeTuple::default(),
        authorized_visibility: &[MemoryVisibility::Private],
        include_debug: true,
    };
    let data = assemble_hot(&inputs, &cfg).unwrap();
    let debug = data.debug.expect("debug present");
    assert_eq!(debug.steps.len(), cfg.recipe.len());
}

#[test]
fn excluded_record_carries_typed_reason() {
    let mut r = sample_record();
    r.kind = MemoryKind::User;
    r.confidence = 0.1; // below floor
    let cfg = HotMemoryConfig::default();
    let pinned = [&r];
    let inputs = HotMemoryInputs {
        purpose_md: "",
        index_md: "",
        pinned_candidates: &pinned,
        project_candidates: &[],
        playbook_candidates: &[],
        user_signal_candidates: &[],
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").unwrap(),
        scope: ScopeTuple::default(),
        authorized_visibility: &[MemoryVisibility::Private],
        include_debug: true,
    };
    let data = assemble_hot(&inputs, &cfg).unwrap();
    let debug = data.debug.expect("debug present");
    let pinned_step = debug
        .steps
        .iter()
        .find(|t| t.step == cairn_core::generated::verbs::assemble_hot::HotRecipeStep::PinnedFeedback)
        .expect("pinned step trace");
    assert_eq!(pinned_step.excluded.len(), 1);
    assert_eq!(
        pinned_step.excluded[0].reason,
        HotExclusionReason::BelowConfidenceFloor
    );
}
```

- [ ] **Step 4: Run all integration tests**

Run: `cargo nextest run -p cairn-core --locked --tests`
Expected: every test passes. If a generated type name (`HotMemoryDebug`,
`HotExclusionReason`, `HotInclusion`, `HotExclusion`, `HotStepTrace`)
disagrees with what codegen emits, replace the references with the
real codegen names.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/tests/assemble_hot_inputs.rs \
        crates/cairn-core/tests/assemble_hot_privacy.rs \
        crates/cairn-core/tests/assemble_hot_debug.rs
git commit -m "test(assemble_hot): privacy + debug + integration coverage (issue #82)"
```

---

## Task 13: Determinism property test

**Files:**
- Modify: `crates/cairn-core/tests/assemble_hot_inputs.rs` (extend with proptest)
- Modify: `crates/cairn-core/Cargo.toml` (add proptest dev-dep if absent)

- [ ] **Step 1: Verify proptest dep**

Run: `grep -c '^proptest' crates/cairn-core/Cargo.toml`
Expected: a non-zero count, indicating the dev-dep is already declared.
If zero, add `proptest = { workspace = true }` under `[dev-dependencies]`.

- [ ] **Step 2: Add the determinism test to `assemble_hot_inputs.rs`**

Append to `crates/cairn-core/tests/assemble_hot_inputs.rs`:

```rust
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn assemble_hot_is_deterministic_for_same_inputs(seed in 0u64..1024) {
        let mut r = sample_record();
        r.id = cairn_core::domain::RecordId::parse(&format!(
            "01HQZX9F5N0000000000000{:03}",
            seed % 1000
        )).unwrap();
        r.target_id = cairn_core::domain::TargetId::parse(&format!(
            "01HQZX9F5N0000000000000{:03}",
            seed % 1000
        )).unwrap();
        r.kind = MemoryKind::User;
        let pinned = [&r];

        let inputs = HotMemoryInputs {
            purpose_md: "",
            index_md: "",
            pinned_candidates: &pinned,
            project_candidates: &[],
            playbook_candidates: &[],
            user_signal_candidates: &[],
            now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").unwrap(),
            scope: ScopeTuple::default(),
            authorized_visibility: &[MemoryVisibility::Private],
            include_debug: false,
        };
        let cfg = HotMemoryConfig::default();
        let a = assemble_hot(&inputs, &cfg).expect("assemble");
        let b = assemble_hot(&inputs, &cfg).expect("assemble");
        prop_assert_eq!(a.prefix, b.prefix);
        prop_assert_eq!(a.bytes, b.bytes);
    }
}
```

- [ ] **Step 3: Run the property test**

Run: `cargo nextest run -p cairn-core --locked --test assemble_hot_inputs`
Expected: 32 cases pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/tests/assemble_hot_inputs.rs \
        crates/cairn-core/Cargo.toml
git commit -m "test(assemble_hot): proptest determinism (issue #82)"
```

---

## Task 14: CLI smoke test

**Files:**
- Create: `crates/cairn-cli/tests/assemble_hot_smoke.rs`

- [ ] **Step 1: Check existing CLI test layout**

Run: `ls crates/cairn-cli/tests/ | head`
Expected: existing fixture-based tests show how `cairn-cli` wires
config + inputs in tests.

- [ ] **Step 2: Write the smoke test**

Create `crates/cairn-cli/tests/assemble_hot_smoke.rs`:

```rust
//! Smoke test: build a fixture-driven `HotMemoryInputs` and call
//! `assemble_hot` end-to-end. Full SQLite + filesystem adapter wiring
//! is owned by issue #80; this test only proves the pure path is
//! reachable from outside `cairn-core`.

use cairn_core::config::HotMemoryConfig;
use cairn_core::domain::Rfc3339Timestamp;
use cairn_core::domain::record::tests_export::sample_record;
use cairn_core::domain::scope::ScopeTuple;
use cairn_core::domain::taxonomy::{MemoryKind, MemoryVisibility};
use cairn_core::verbs::assemble_hot::{HotMemoryInputs, assemble_hot};

#[test]
fn assemble_hot_runs_with_minimal_inputs() {
    let mut r = sample_record();
    r.kind = MemoryKind::User;
    let pinned = [&r];
    let inputs = HotMemoryInputs {
        purpose_md: "# Purpose\nact carefully.\n",
        index_md: "# Index\n",
        pinned_candidates: &pinned,
        project_candidates: &[],
        playbook_candidates: &[],
        user_signal_candidates: &[],
        now: Rfc3339Timestamp::parse("2026-04-22T15:00:00Z").unwrap(),
        scope: ScopeTuple::default(),
        authorized_visibility: &[MemoryVisibility::Private],
        include_debug: false,
    };
    let cfg = HotMemoryConfig::default();
    let data = assemble_hot(&inputs, &cfg).expect("assemble");
    assert!(data.bytes > 0);
    let segments = data.segments.expect("segments emitted");
    assert_eq!(segments.len(), cfg.recipe.len());
}
```

- [ ] **Step 3: Verify cairn-cli has cairn-core in dev-deps**

Run: `grep '^cairn-core' crates/cairn-cli/Cargo.toml`
Expected: a workspace dependency line. If missing under `[dev-dependencies]`, add it.

- [ ] **Step 4: Run the smoke test**

Run: `cargo nextest run -p cairn-cli --locked --test assemble_hot_smoke`
Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/tests/assemble_hot_smoke.rs
git commit -m "test(cairn-cli): assemble_hot smoke (issue #82)"
```

---

## Task 15: Final verification sweep

**Files:** none (read-only checks)

- [ ] **Step 1: fmt**

Run: `cargo fmt --all --check`
Expected: exit 0. If not, run `cargo fmt --all` and amend the most-recent commit.

- [ ] **Step 2: clippy**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: exit 0.

- [ ] **Step 3: check**

Run: `cargo check --workspace --all-targets --locked`
Expected: exit 0.

- [ ] **Step 4: nextest**

Run: `cargo nextest run --workspace --locked --no-fail-fast`
Expected: every test passes. Capture failure list if any.

- [ ] **Step 5: doctest**

Run: `cargo test --doc --workspace --locked`
Expected: every doctest passes.

- [ ] **Step 6: core boundary**

Run: `./scripts/check-core-boundary.sh`
Expected: exit 0.

- [ ] **Step 7: codegen drift**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
Expected: exit 0, no diff.

- [ ] **Step 8: docgen**

Run: `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`
Expected: exit 0. If config defaults grew via this issue, run without
`--check`, commit the docs change in a separate `docs:` commit.

- [ ] **Step 9: rustdoc**

Run:
```bash
RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked
```
Expected: exit 0.

- [ ] **Step 10: PR-ready summary**

Verify the branch contains commits for:
- `feat(idl): …` (Task 1)
- `feat(assemble_hot): typed inclusion/exclusion …` (Task 2)
- `feat(assemble_hot): uniform admissibility …` (Task 3)
- `feat(assemble_hot): HotMemoryInputs …` (Task 4)
- `feat(assemble_hot): purpose + index …` (Task 5)
- `feat(assemble_hot): canonical record-body framing …` (Task 6)
- `feat(assemble_hot): pinned-feedback source …` (Task 7)
- `feat(assemble_hot): top-salience project source …` (Task 8)
- `feat(assemble_hot): active-playbook source …` (Task 9)
- `feat(assemble_hot): recent-user-signal source …` (Task 10)
- `feat(assemble_hot): refactor assembler …` (Task 11)
- `test(assemble_hot): privacy + debug + integration …` (Task 12)
- `test(assemble_hot): proptest determinism …` (Task 13)
- `test(cairn-cli): assemble_hot smoke (issue #82)` (Task 14)

Run: `git log --oneline main..HEAD`
Expected: 14 commits in the listed order.
