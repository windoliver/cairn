# Policy Trace — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the typed `policy_trace` infrastructure in `cairn-core` (closed `PolicyGate` enum, body-free `PolicyDetail`, `PolicyTraceEntry`, `From` impls from existing pipeline outcomes, `to_wire` mapping), advertise the `cairn.mcp.v1.policy_trace` capability, and document the gate vocabulary. PR2 adds the `explain_filter` pure function and `RecordExclusion` type for read-path explainability.

**Architecture:** All gate logic stays in pure `cairn-core` modules. Verbs already exist as stubs (issue #9 / #46 wires runtime); this issue delivers the **infrastructure** verbs will consume, plus the IDL and docs that make `policy_trace: v1` a stable contract surface today. No CLI verb-runtime threading is in scope — that lands with each verb's implementation.

**Tech Stack:** Rust 1.95.0, edition 2024, `serde` + `schemars`, `proptest`, `insta`, `rstest`, `nextest`. IDL = JSON Schema 2020-12 in `crates/cairn-idl/schema/`, codegen = `cargo run -p cairn-idl --bin cairn-codegen`. Docgen = `cargo run -p cairn-cli --bin cairn-docgen -- --write`.

**Spec source:** `docs/superpowers/specs/2026-04-29-policy-trace-design.md`. **Issue:** [#95](https://github.com/windoliver/cairn/issues/95). **Brief:** §14, §5.1, §5.2, §8.0.b, §6.3, §4.2.

---

## Scope reality (read first)

The five verbs touched by the spec are currently stubs (`unimplemented_response`) except for `ingest --resync`, which has its own handler that does *not* invoke the §5.2 gates today. There is therefore no verb-dispatch site that can populate a non-empty `policy_trace` end-to-end yet. The plan reflects this:

- **In scope (this issue):** core types, From impls, `to_wire`, `Capabilities` IDL variant, gate-vocabulary doc, comprehensive unit/property/snapshot tests, capability advertisement, `explain_filter` pure function (PR2), `RecordExclusion` (PR2), IDL changes for `search`/`retrieve` (PR2).
- **Deferred to verb issues (#9, #46):** wiring the trace builder into verb runtime (`ingest`, `capture_trace`, `forget`, `search`, `retrieve`). Those issues now have concrete API to call.

Acceptance criteria are met by typed contract + tests proving the body-free invariant and visibility-respecting shape; runtime evidence lives in the verb issues.

---

## File structure

### PR1 — types, IDL capability, docs

**Create:**
- `crates/cairn-core/src/policy_trace/mod.rs` — module entry, public re-exports.
- `crates/cairn-core/src/policy_trace/gate.rs` — `PolicyGate` closed enum + `Display`.
- `crates/cairn-core/src/policy_trace/outcome.rs` — `PolicyOutcome` enum.
- `crates/cairn-core/src/policy_trace/detail.rs` — `PolicyDetail` enum + `to_wire_string`.
- `crates/cairn-core/src/policy_trace/entry.rs` — `PolicyTraceEntry`, constructors, `to_wire`.
- `crates/cairn-core/src/policy_trace/from_pipeline.rs` — `From<&Decision>`, `From<&RedactedPayload>`, `From<&FencedPayload>` impls.
- `crates/cairn-core/tests/policy_trace_round_trip.rs` — proptest round-trip.
- `crates/cairn-core/tests/policy_trace_body_free.rs` — JSON-walker invariant.
- `docs/site/src/reference/policy-gates.md` — gate vocabulary reference.

**Modify:**
- `crates/cairn-core/src/lib.rs` — add `pub mod policy_trace;`.
- `crates/cairn-idl/schema/capabilities/capabilities.json` — append `cairn.mcp.v1.policy_trace`.
- `crates/cairn-core/src/generated/common/mod.rs` — regenerated (new variant).
- `crates/cairn-cli/src/docgen.rs` — register the new reference page.
- `crates/cairn-cli/src/verbs/status.rs` — advertise the new capability.
- `crates/cairn-cli/tests/envelope_tests.rs` — assert `status` advertises `policy_trace`.
- `crates/cairn-cli/tests/docgen.rs` — snapshot the new page.

### PR2 — explain machinery, RecordExclusion, IDL

**Create:**
- `crates/cairn-core/src/policy_trace/exclusion.rs` — `RecordExclusion` type.
- `crates/cairn-core/src/pipeline/explain.rs` — `explain_filter` pure function.
- `crates/cairn-core/tests/explain_filter.rs` — fixture tests for the two-tier rule.
- `crates/cairn-idl/schema/common/record_exclusion.json`.

**Modify:**
- `crates/cairn-idl/schema/verbs/search.json` — `args.explain`, `data.excluded?`.
- `crates/cairn-idl/schema/verbs/retrieve.json` — same.
- `crates/cairn-core/src/generated/verbs/{search,retrieve}.rs` — regenerated.
- `crates/cairn-cli/tests/envelope_tests.rs` — byte-identity snapshot for default (no-explain) path.

---

# PR1 — core types, IDL capability, docs

### Task 1: Add `cairn.mcp.v1.policy_trace` to the Capabilities IDL

**Files:**
- Modify: `crates/cairn-idl/schema/capabilities/capabilities.json`
- Regenerate: `crates/cairn-core/src/generated/common/mod.rs`

- [ ] **Step 1: Add the capability constant**

Open `crates/cairn-idl/schema/capabilities/capabilities.json`. Inside the `oneOf` array, add a new entry directly after the `cairn.mcp.v1.extension.sessiontree` line (so it sits at the end of the existing `extension.*` block):

```json
{ "const": "cairn.mcp.v1.policy_trace",   "x-cairn-since": "v0.1" }
```

Make sure the preceding entry's trailing comma is added. Final shape of the new line region:

```json
    { "const": "cairn.mcp.v1.extension.sessiontree", "x-cairn-since": "v0.3" },
    { "const": "cairn.mcp.v1.policy_trace",       "x-cairn-since": "v0.1" }
  ],
```

- [ ] **Step 2: Run the codegen check (expect failure)**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
Expected: FAIL with a diff against `crates/cairn-core/src/generated/common/mod.rs` because the new variant is missing.

- [ ] **Step 3: Regenerate**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked`
Expected: writes the new `CairnMcpV1PolicyTrace` variant into `crates/cairn-core/src/generated/common/mod.rs`.

- [ ] **Step 4: Verify the regenerated file**

Open `crates/cairn-core/src/generated/common/mod.rs`. Confirm the new variant is appended to the `Capabilities` enum:

```rust
    #[serde(rename = "cairn.mcp.v1.policy_trace")]
    CairnMcpV1PolicyTrace,
```

- [ ] **Step 5: Run the codegen check again (expect pass)**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
Expected: PASS — clean diff.

- [ ] **Step 6: Run workspace check**

Run: `cargo check --workspace --all-targets --locked`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-idl/schema/capabilities/capabilities.json \
        crates/cairn-core/src/generated/common/mod.rs
git commit -m "feat(idl): add cairn.mcp.v1.policy_trace capability (#95)"
```

---

### Task 2: Skeleton `policy_trace` module + `PolicyGate` enum

**Files:**
- Create: `crates/cairn-core/src/policy_trace/mod.rs`
- Create: `crates/cairn-core/src/policy_trace/gate.rs`
- Create: `crates/cairn-core/tests/policy_trace_gate.rs`
- Modify: `crates/cairn-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-core/tests/policy_trace_gate.rs`:

```rust
//! `PolicyGate::Display` produces the stable wire vocabulary listed in the
//! design doc §5. A snapshot freezes the full enum so any reorder or rename
//! requires an explicit review.

use cairn_core::policy_trace::PolicyGate;

#[test]
fn display_matches_wire_vocabulary() {
    let cases = [
        (PolicyGate::PresidioRedaction, "presidio_redaction"),
        (PolicyGate::PromptInjectionFence, "prompt_injection_fence"),
        (PolicyGate::FilterShouldMemorize, "filter_should_memorize"),
        (PolicyGate::VisibilityFloor, "visibility_floor"),
        (PolicyGate::ScopeCheck, "scope_check"),
        (PolicyGate::ForgetCapability, "forget_capability"),
        (PolicyGate::ConsentJournalAppend, "consent_journal_append"),
        (PolicyGate::ReadFilterRelevance, "read_filter_relevance"),
        (PolicyGate::ReadFilterStaleness, "read_filter_staleness"),
        (PolicyGate::ReadFilterDedup, "read_filter_dedup"),
    ];
    for (gate, expected) in cases {
        assert_eq!(gate.to_string(), expected, "gate {gate:?}");
    }
}

#[test]
fn debug_is_camel_case() {
    assert_eq!(format!("{:?}", PolicyGate::PresidioRedaction), "PresidioRedaction");
}
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-core --test policy_trace_gate`
Expected: FAIL — `policy_trace` module not found.

- [ ] **Step 3: Add the module to lib.rs**

Edit `crates/cairn-core/src/lib.rs`. Inside the `pub mod` block (after `pub mod pipeline;`), add:

```rust
pub mod policy_trace;
```

- [ ] **Step 4: Create the module file**

Create `crates/cairn-core/src/policy_trace/mod.rs`:

```rust
//! Policy trace — typed gate vocabulary and outcome shapes for the
//! `policy_trace` field on every verb response (brief §8.0.b, §14, §5.2).
//!
//! The module is producer-side only: closed enums for gates and outcomes,
//! body-free metadata for `detail`, and a pure `to_wire` mapping into the
//! generated `ResponsePolicyTrace` shape. Verbs compose gates as today,
//! push `PolicyTraceEntry` values onto a local vector, and call `to_wire`
//! at the envelope boundary.
//!
//! No I/O. No store dependency. No allocations beyond what `Vec<…>` and
//! `BTreeMap<…>` require for the trace itself.

mod gate;

pub use gate::PolicyGate;
```

- [ ] **Step 5: Implement the gate enum**

Create `crates/cairn-core/src/policy_trace/gate.rs`:

```rust
//! Closed gate vocabulary. `Display` emits the stable snake_case wire
//! string used by `PolicyTraceEntry::to_wire` and surfaced verbatim on
//! the `ResponsePolicyTrace.gate` field.

use std::fmt;

/// Every gate that can fire across all eight verbs (brief §5.2, §6.3,
/// §4.2, §14, §8). Adding a variant is a `#[non_exhaustive]` minor
/// change. A vocabulary *break* (rename, semantic shift) travels with
/// the MCP contract version per the design doc §8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PolicyGate {
    /// Pre-persist PII / secret redaction (brief §5.2, §14).
    PresidioRedaction,
    /// Prompt-injection fencing wrap (brief §5.2).
    PromptInjectionFence,
    /// Filter `should_memorize` decision (brief §5.2).
    FilterShouldMemorize,
    /// Default visibility resolution (brief §6.3).
    VisibilityFloor,
    /// Scope tuple check (brief §4.2).
    ScopeCheck,
    /// Forget capability check on `forget` (brief §8).
    ForgetCapability,
    /// Atomic consent journal append (brief §14, §5.6).
    ConsentJournalAppend,
    /// Read-path relevance filter (brief §5.1; --explain only).
    ReadFilterRelevance,
    /// Read-path staleness filter (brief §5.1; --explain only).
    ReadFilterStaleness,
    /// Read-path duplicate filter (brief §5.1; --explain only).
    ReadFilterDedup,
}

impl PolicyGate {
    /// Wire-format identifier (lower snake_case). Stable across surfaces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PresidioRedaction => "presidio_redaction",
            Self::PromptInjectionFence => "prompt_injection_fence",
            Self::FilterShouldMemorize => "filter_should_memorize",
            Self::VisibilityFloor => "visibility_floor",
            Self::ScopeCheck => "scope_check",
            Self::ForgetCapability => "forget_capability",
            Self::ConsentJournalAppend => "consent_journal_append",
            Self::ReadFilterRelevance => "read_filter_relevance",
            Self::ReadFilterStaleness => "read_filter_staleness",
            Self::ReadFilterDedup => "read_filter_dedup",
        }
    }
}

impl fmt::Display for PolicyGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
```

- [ ] **Step 6: Run the test (expect pass)**

Run: `cargo nextest run -p cairn-core --test policy_trace_gate`
Expected: PASS — both tests green.

- [ ] **Step 7: Run workspace check + clippy**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-core/src/lib.rs \
        crates/cairn-core/src/policy_trace/mod.rs \
        crates/cairn-core/src/policy_trace/gate.rs \
        crates/cairn-core/tests/policy_trace_gate.rs
git commit -m "feat(core): policy_trace module skeleton + PolicyGate enum (#95)"
```

---

### Task 3: `PolicyOutcome` enum

**Files:**
- Create: `crates/cairn-core/src/policy_trace/outcome.rs`
- Modify: `crates/cairn-core/src/policy_trace/mod.rs`
- Create: `crates/cairn-core/tests/policy_trace_outcome.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-core/tests/policy_trace_outcome.rs`:

```rust
use cairn_core::policy_trace::PolicyOutcome;

#[test]
fn outcomes_serialize_to_wire_form() {
    let cases = [
        (PolicyOutcome::Pass, "pass"),
        (PolicyOutcome::Deny, "deny"),
        (PolicyOutcome::Error, "error"),
    ];
    for (oc, expected) in cases {
        assert_eq!(oc.as_str(), expected);
        assert_eq!(format!("{oc}"), expected);
    }
}
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-core --test policy_trace_outcome`
Expected: FAIL — `PolicyOutcome` not found.

- [ ] **Step 3: Implement the outcome enum**

Create `crates/cairn-core/src/policy_trace/outcome.rs`:

```rust
//! Outcome of a single gate evaluation. Mirrors
//! `ResponsePolicyTraceResult` on the wire.

use std::fmt;

/// Three-valued outcome: `Pass` (gate ran, allowed), `Deny` (gate ran,
/// blocked), `Error` (gate evaluation itself failed mid-flight). Maps
/// 1:1 to `cairn_core::generated::envelope::ResponsePolicyTraceResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PolicyOutcome {
    Pass,
    Deny,
    Error,
}

impl PolicyOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Deny => "deny",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for PolicyOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
```

- [ ] **Step 4: Re-export from the module**

Edit `crates/cairn-core/src/policy_trace/mod.rs`. Add the new submodule and re-export:

```rust
mod gate;
mod outcome;

pub use gate::PolicyGate;
pub use outcome::PolicyOutcome;
```

- [ ] **Step 5: Run the test (expect pass)**

Run: `cargo nextest run -p cairn-core --test policy_trace_outcome`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/policy_trace/outcome.rs \
        crates/cairn-core/src/policy_trace/mod.rs \
        crates/cairn-core/tests/policy_trace_outcome.rs
git commit -m "feat(core): PolicyOutcome enum (#95)"
```

---

### Task 4: `PolicyDetail` enum (body-free)

**Files:**
- Create: `crates/cairn-core/src/policy_trace/detail.rs`
- Modify: `crates/cairn-core/src/policy_trace/mod.rs`
- Create: `crates/cairn-core/tests/policy_trace_detail.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-core/tests/policy_trace_detail.rs`:

```rust
//! `PolicyDetail` is body-free: every variant carries only metadata
//! (counts, codes, enum tags) and never raw bytes. The wire string
//! produced by `to_wire_string` is short and stable.

use std::collections::BTreeMap;

use cairn_core::domain::MemoryVisibility;
use cairn_core::pipeline::filter::{DiscardReason, RedactionTag};
use cairn_core::policy_trace::PolicyDetail;

#[test]
fn none_is_empty_string() {
    assert_eq!(PolicyDetail::None.to_wire_string(), "");
}

#[test]
fn discard_reason_serializes_to_kind_and_code() {
    let d = PolicyDetail::DiscardReason(DiscardReason::PiiBlocked);
    assert_eq!(d.to_wire_string(), "discard:pii_blocked");
}

#[test]
fn redaction_tag_counts_emit_sorted_pairs() {
    let mut counts = BTreeMap::new();
    counts.insert(RedactionTag::Email, 2);
    counts.insert(RedactionTag::Ssn, 1);
    let d = PolicyDetail::RedactionTagCounts(counts);
    assert_eq!(d.to_wire_string(), "redacted:email=2,ssn=1");
}

#[test]
fn visibility_floor_serializes_to_floor_and_tier() {
    let d = PolicyDetail::VisibilityFloor(MemoryVisibility::Session);
    assert_eq!(d.to_wire_string(), "floor:session");
}

#[test]
fn scope_mismatch_emits_required_tier_only() {
    // Caller's actual scope is never echoed; only the *required* tier.
    let d = PolicyDetail::ScopeMismatch { required_tier: MemoryVisibility::Project };
    assert_eq!(d.to_wire_string(), "scope_required:project");
}

#[test]
fn error_code_uses_static_string() {
    let d = PolicyDetail::ErrorCode("wal_failure");
    assert_eq!(d.to_wire_string(), "error:wal_failure");
}
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-core --test policy_trace_detail`
Expected: FAIL — `PolicyDetail` not found.

- [ ] **Step 3: Implement the detail enum**

Create `crates/cairn-core/src/policy_trace/detail.rs`:

```rust
//! Body-free metadata attached to a [`super::PolicyTraceEntry`]. Every
//! variant carries only counts, codes, or enum tags; raw bytes,
//! source content, and record bodies never appear here.
//!
//! Mirrors the body-free pattern used by
//! [`crate::pipeline::filter::audit::BlockedAuditEntry`].

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::domain::MemoryVisibility;
use crate::pipeline::filter::{DiscardReason, RedactionTag};

/// Body-free metadata for a gate outcome. Serializes via
/// [`Self::to_wire_string`] into a short stable code; the empty string
/// represents [`Self::None`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyDetail {
    /// Gate ran with no extra metadata to surface.
    None,
    /// Filter `should_memorize` returned this discard reason.
    DiscardReason(DiscardReason),
    /// Redactor stripped these tag → count pairs.
    RedactionTagCounts(BTreeMap<RedactionTag, u32>),
    /// Visibility floor resolution chose this tier.
    VisibilityFloor(MemoryVisibility),
    /// Scope check denied; only the *required* tier is echoed (never
    /// the caller's actual scope, which is privileged).
    ScopeMismatch { required_tier: MemoryVisibility },
    /// Stable static error code (never a free message).
    ErrorCode(&'static str),
}

impl PolicyDetail {
    /// Short, stable, human-readable wire form. Body-free by
    /// construction.
    #[must_use]
    pub fn to_wire_string(&self) -> String {
        match self {
            Self::None => String::new(),
            Self::DiscardReason(r) => format!("discard:{}", r.as_str()),
            Self::RedactionTagCounts(counts) => {
                let mut out = String::from("redacted:");
                let mut first = true;
                for (tag, count) in counts {
                    if !first {
                        out.push(',');
                    }
                    let _ = write!(out, "{}={count}", tag.as_str());
                    first = false;
                }
                out
            }
            Self::VisibilityFloor(v) => format!("floor:{}", v.as_str()),
            Self::ScopeMismatch { required_tier } => {
                format!("scope_required:{}", required_tier.as_str())
            }
            Self::ErrorCode(c) => format!("error:{c}"),
        }
    }
}
```

- [ ] **Step 4: Verify `DiscardReason::as_str` exists**

Run: `grep -n "pub.*fn as_str" crates/cairn-core/src/pipeline/filter/decision.rs`
Expected: a `pub const fn as_str(self) -> &'static str` already exists. If not, add one to `decision.rs` mirroring `RedactionTag::as_str` before continuing.

If absent, add to `decision.rs` (after the enum):

```rust
impl DiscardReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Volatile => "volatile",
            Self::ToolLookup => "tool_lookup",
            Self::CompetingSource => "competing_source",
            Self::LowSalience => "low_salience",
            Self::PiiBlocked => "pii_blocked",
            Self::InjectionBlocked => "injection_blocked",
            Self::PolicyBlocked => "policy_blocked",
            Self::Duplicate => "duplicate",
        }
    }
}
```

- [ ] **Step 5: Re-export from the module**

Edit `crates/cairn-core/src/policy_trace/mod.rs`:

```rust
mod detail;
mod gate;
mod outcome;

pub use detail::PolicyDetail;
pub use gate::PolicyGate;
pub use outcome::PolicyOutcome;
```

- [ ] **Step 6: Run the test (expect pass)**

Run: `cargo nextest run -p cairn-core --test policy_trace_detail`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/policy_trace/detail.rs \
        crates/cairn-core/src/policy_trace/mod.rs \
        crates/cairn-core/src/pipeline/filter/decision.rs \
        crates/cairn-core/tests/policy_trace_detail.rs
git commit -m "feat(core): body-free PolicyDetail enum (#95)"
```

---

### Task 5: `PolicyTraceEntry` + constructors + `to_wire`

**Files:**
- Create: `crates/cairn-core/src/policy_trace/entry.rs`
- Modify: `crates/cairn-core/src/policy_trace/mod.rs`
- Create: `crates/cairn-core/tests/policy_trace_entry.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-core/tests/policy_trace_entry.rs`:

```rust
use cairn_core::domain::MemoryVisibility;
use cairn_core::generated::envelope::ResponsePolicyTraceResult;
use cairn_core::policy_trace::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry, to_wire};

#[test]
fn entry_holds_gate_outcome_detail() {
    let e = PolicyTraceEntry::new(
        PolicyGate::ScopeCheck,
        PolicyOutcome::Deny,
        PolicyDetail::ScopeMismatch { required_tier: MemoryVisibility::Project },
    );
    assert_eq!(e.gate, PolicyGate::ScopeCheck);
    assert_eq!(e.outcome, PolicyOutcome::Deny);
    assert!(matches!(e.detail, PolicyDetail::ScopeMismatch { .. }));
}

#[test]
fn pass_constructor_uses_none_detail() {
    let e = PolicyTraceEntry::pass(PolicyGate::PromptInjectionFence);
    assert_eq!(e.outcome, PolicyOutcome::Pass);
    assert_eq!(e.detail, PolicyDetail::None);
}

#[test]
fn to_wire_maps_each_field() {
    let entries = vec![
        PolicyTraceEntry::pass(PolicyGate::PresidioRedaction),
        PolicyTraceEntry::new(
            PolicyGate::FilterShouldMemorize,
            PolicyOutcome::Deny,
            PolicyDetail::ErrorCode("pii_blocked"),
        ),
    ];
    let wire = to_wire(&entries);
    assert_eq!(wire.len(), 2);
    assert_eq!(wire[0].gate, "presidio_redaction");
    assert_eq!(wire[0].result, ResponsePolicyTraceResult::Pass);
    assert_eq!(wire[0].detail, None); // empty wire string maps to None

    assert_eq!(wire[1].gate, "filter_should_memorize");
    assert_eq!(wire[1].result, ResponsePolicyTraceResult::Deny);
    assert_eq!(wire[1].detail.as_deref(), Some("error:pii_blocked"));
}

#[test]
fn to_wire_preserves_order() {
    let entries = vec![
        PolicyTraceEntry::pass(PolicyGate::PresidioRedaction),
        PolicyTraceEntry::pass(PolicyGate::PromptInjectionFence),
        PolicyTraceEntry::pass(PolicyGate::FilterShouldMemorize),
    ];
    let wire = to_wire(&entries);
    let gates: Vec<&str> = wire.iter().map(|e| e.gate.as_str()).collect();
    assert_eq!(gates, vec!["presidio_redaction", "prompt_injection_fence", "filter_should_memorize"]);
}
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-core --test policy_trace_entry`
Expected: FAIL — `PolicyTraceEntry` and `to_wire` not found.

- [ ] **Step 3: Implement entry + to_wire**

Create `crates/cairn-core/src/policy_trace/entry.rs`:

```rust
//! `PolicyTraceEntry` and the `to_wire` mapping into the generated
//! `ResponsePolicyTrace` shape. Pure functions; no I/O.

use crate::generated::envelope::{ResponsePolicyTrace, ResponsePolicyTraceResult};

use super::{PolicyDetail, PolicyGate, PolicyOutcome};

/// One gate outcome. Verbs build a `Vec<PolicyTraceEntry>` and call
/// [`to_wire`] at the envelope boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyTraceEntry {
    pub gate: PolicyGate,
    pub outcome: PolicyOutcome,
    pub detail: PolicyDetail,
}

impl PolicyTraceEntry {
    #[must_use]
    pub const fn new(gate: PolicyGate, outcome: PolicyOutcome, detail: PolicyDetail) -> Self {
        Self { gate, outcome, detail }
    }

    /// `(gate, Pass, None)` — most common shape.
    #[must_use]
    pub const fn pass(gate: PolicyGate) -> Self {
        Self::new(gate, PolicyOutcome::Pass, PolicyDetail::None)
    }

    /// `(gate, Deny, detail)`.
    #[must_use]
    pub const fn deny(gate: PolicyGate, detail: PolicyDetail) -> Self {
        Self::new(gate, PolicyOutcome::Deny, detail)
    }

    /// `(gate, Error, ErrorCode(code))`.
    #[must_use]
    pub const fn error(gate: PolicyGate, code: &'static str) -> Self {
        Self::new(gate, PolicyOutcome::Error, PolicyDetail::ErrorCode(code))
    }
}

/// Pure mapping from producer-side trace entries to the wire shape.
/// Empty `detail` strings collapse to `None` per the IDL's
/// `skip_serializing_if`.
#[must_use]
pub fn to_wire(entries: &[PolicyTraceEntry]) -> Vec<ResponsePolicyTrace> {
    entries.iter().map(to_wire_one).collect()
}

fn to_wire_one(entry: &PolicyTraceEntry) -> ResponsePolicyTrace {
    let result = match entry.outcome {
        PolicyOutcome::Pass => ResponsePolicyTraceResult::Pass,
        PolicyOutcome::Deny => ResponsePolicyTraceResult::Deny,
        PolicyOutcome::Error => ResponsePolicyTraceResult::Error,
    };
    let detail_str = entry.detail.to_wire_string();
    let detail = if detail_str.is_empty() { None } else { Some(detail_str) };
    ResponsePolicyTrace {
        gate: entry.gate.as_str().to_owned(),
        result,
        detail,
    }
}
```

- [ ] **Step 4: Re-export from the module**

Edit `crates/cairn-core/src/policy_trace/mod.rs`:

```rust
mod detail;
mod entry;
mod gate;
mod outcome;

pub use detail::PolicyDetail;
pub use entry::{PolicyTraceEntry, to_wire};
pub use gate::PolicyGate;
pub use outcome::PolicyOutcome;
```

- [ ] **Step 5: Run the test (expect pass)**

Run: `cargo nextest run -p cairn-core --test policy_trace_entry`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/policy_trace/entry.rs \
        crates/cairn-core/src/policy_trace/mod.rs \
        crates/cairn-core/tests/policy_trace_entry.rs
git commit -m "feat(core): PolicyTraceEntry + to_wire mapping (#95)"
```

---

### Task 6: `From<&Decision>` and `From<&RedactedPayload>`

**Files:**
- Create: `crates/cairn-core/src/policy_trace/from_pipeline.rs`
- Modify: `crates/cairn-core/src/policy_trace/mod.rs`
- Create: `crates/cairn-core/tests/policy_trace_from_pipeline.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-core/tests/policy_trace_from_pipeline.rs`:

```rust
use std::collections::BTreeMap;

use cairn_core::pipeline::filter::{
    Decision, DiscardReason, FenceMark, FencedPayload, RedactedPayload, RedactionSpan, RedactionTag,
};
use cairn_core::policy_trace::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry};

#[test]
fn decision_proceed_maps_to_pass() {
    let d = Decision::Proceed;
    let e: PolicyTraceEntry = (&d).into();
    assert_eq!(e.gate, PolicyGate::FilterShouldMemorize);
    assert_eq!(e.outcome, PolicyOutcome::Pass);
    assert_eq!(e.detail, PolicyDetail::None);
}

#[test]
fn decision_discard_maps_to_deny_with_reason() {
    let d = Decision::Discard(DiscardReason::PiiBlocked);
    let e: PolicyTraceEntry = (&d).into();
    assert_eq!(e.gate, PolicyGate::FilterShouldMemorize);
    assert_eq!(e.outcome, PolicyOutcome::Deny);
    assert_eq!(e.detail, PolicyDetail::DiscardReason(DiscardReason::PiiBlocked));
}

#[test]
fn redacted_with_no_spans_is_pass_with_none() {
    let payload = RedactedPayload { masked: String::from("clean"), spans: Vec::new() };
    let e: PolicyTraceEntry = (&payload).into();
    assert_eq!(e.gate, PolicyGate::PresidioRedaction);
    assert_eq!(e.outcome, PolicyOutcome::Pass);
    assert_eq!(e.detail, PolicyDetail::None);
}

#[test]
fn redacted_with_spans_aggregates_counts() {
    let payload = RedactedPayload {
        masked: String::from("***"),
        spans: vec![
            RedactionSpan { start: 0, end: 1, tag: RedactionTag::Email },
            RedactionSpan { start: 2, end: 3, tag: RedactionTag::Email },
            RedactionSpan { start: 4, end: 5, tag: RedactionTag::Ssn },
        ],
    };
    let e: PolicyTraceEntry = (&payload).into();
    let mut expected: BTreeMap<RedactionTag, u32> = BTreeMap::new();
    expected.insert(RedactionTag::Email, 2);
    expected.insert(RedactionTag::Ssn, 1);
    assert_eq!(e.gate, PolicyGate::PresidioRedaction);
    assert_eq!(e.outcome, PolicyOutcome::Pass); // pass; pii_blocked decision is upstream
    assert_eq!(e.detail, PolicyDetail::RedactionTagCounts(expected));
}

#[test]
fn fenced_always_pass() {
    // Fencing is non-blocking by design — it wraps, never rejects.
    let payload = FencedPayload { wrapped: String::from("[FENCE]x[/FENCE]"), marks: vec![FenceMark { start: 0, end: 16 }] };
    let e: PolicyTraceEntry = (&payload).into();
    assert_eq!(e.gate, PolicyGate::PromptInjectionFence);
    assert_eq!(e.outcome, PolicyOutcome::Pass);
    assert_eq!(e.detail, PolicyDetail::None);
}
```

> **Field-name verification.** Before adding the From impls, run:
> `grep -n "pub struct RedactedPayload\|pub struct RedactionSpan\|pub struct FencedPayload\|pub struct FenceMark" crates/cairn-core/src/pipeline/filter/`
> If any field name in the test above differs from the existing struct, fix the test to match — the existing types are the contract; this task adapts to them.

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-core --test policy_trace_from_pipeline`
Expected: FAIL — `From` impls not found.

- [ ] **Step 3: Implement the impls**

Create `crates/cairn-core/src/policy_trace/from_pipeline.rs`:

```rust
//! `From` impls converting existing pipeline outcome types into
//! [`PolicyTraceEntry`]. Verbs compose gates as today and call
//! `(&result).into()` to push a trace entry.
//!
//! New pipeline outcome types that fire a gate must add a `From` impl
//! here so producers stay closed.

use std::collections::BTreeMap;

use crate::pipeline::filter::{Decision, FencedPayload, RedactedPayload, RedactionTag};

use super::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry};

impl From<&Decision> for PolicyTraceEntry {
    fn from(d: &Decision) -> Self {
        match d {
            Decision::Proceed => PolicyTraceEntry::pass(PolicyGate::FilterShouldMemorize),
            Decision::Discard(reason) => PolicyTraceEntry::deny(
                PolicyGate::FilterShouldMemorize,
                PolicyDetail::DiscardReason(*reason),
            ),
        }
    }
}

impl From<&RedactedPayload> for PolicyTraceEntry {
    fn from(p: &RedactedPayload) -> Self {
        if p.spans.is_empty() {
            return PolicyTraceEntry::pass(PolicyGate::PresidioRedaction);
        }
        let mut counts: BTreeMap<RedactionTag, u32> = BTreeMap::new();
        for span in &p.spans {
            *counts.entry(span.tag).or_default() += 1;
        }
        PolicyTraceEntry::new(
            PolicyGate::PresidioRedaction,
            PolicyOutcome::Pass,
            PolicyDetail::RedactionTagCounts(counts),
        )
    }
}

impl From<&FencedPayload> for PolicyTraceEntry {
    fn from(_: &FencedPayload) -> Self {
        // Fencing is non-blocking — it wraps but never rejects.
        PolicyTraceEntry::pass(PolicyGate::PromptInjectionFence)
    }
}
```

- [ ] **Step 4: Re-export the module path**

Edit `crates/cairn-core/src/policy_trace/mod.rs`. Add the new submodule (no public re-exports needed; the impls are auto-discovered via the trait):

```rust
mod detail;
mod entry;
mod from_pipeline;
mod gate;
mod outcome;

pub use detail::PolicyDetail;
pub use entry::{PolicyTraceEntry, to_wire};
pub use gate::PolicyGate;
pub use outcome::PolicyOutcome;
```

- [ ] **Step 5: Run the test (expect pass)**

Run: `cargo nextest run -p cairn-core --test policy_trace_from_pipeline`
Expected: PASS.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/policy_trace/from_pipeline.rs \
        crates/cairn-core/src/policy_trace/mod.rs \
        crates/cairn-core/tests/policy_trace_from_pipeline.rs
git commit -m "feat(core): From<&Decision/&RedactedPayload/&FencedPayload> for PolicyTraceEntry (#95)"
```

---

### Task 7: Body-free property test (JSON-walker invariant)

**Files:**
- Create: `crates/cairn-core/tests/policy_trace_body_free.rs`
- Modify: `crates/cairn-core/Cargo.toml` (verify `proptest` is a dev-dep)

- [ ] **Step 1: Confirm proptest is available**

Run: `grep -n proptest crates/cairn-core/Cargo.toml`
Expected: a `[dev-dependencies]` line for `proptest`. If not, add it under `[dev-dependencies]`:

```toml
proptest = { workspace = true }
```

(The workspace-level pin is in the root `Cargo.toml`. If absent there, add `proptest = "1"` under `[workspace.dependencies]` first.)

- [ ] **Step 2: Write the failing test**

Create `crates/cairn-core/tests/policy_trace_body_free.rs`:

```rust
//! Invariant: the JSON serialization of any `to_wire` output never
//! contains a body-bearing field name regardless of the contained
//! `PolicyDetail`. Same body-free pattern as #94's ConsentEvent walker.
//!
//! Banned key names — even appearing as a substring of a value would
//! be suspicious — match the #94 invariant set:
//! `body | text | raw | command | url | content | payload`.

use std::collections::BTreeMap;

use cairn_core::domain::MemoryVisibility;
use cairn_core::pipeline::filter::{DiscardReason, RedactionTag};
use cairn_core::policy_trace::{PolicyDetail, PolicyGate, PolicyOutcome, PolicyTraceEntry, to_wire};

const BANNED_KEYS: &[&str] = &["body", "text", "raw", "command", "url", "content", "payload"];

fn assert_body_free(json: &str) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    walk(&v);

    fn walk(v: &serde_json::Value) {
        match v {
            serde_json::Value::Object(o) => {
                for (k, child) in o {
                    for banned in BANNED_KEYS {
                        assert_ne!(
                            k.as_str(),
                            *banned,
                            "policy trace JSON must not use field name {banned:?}: {v}"
                        );
                    }
                    walk(child);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(walk),
            _ => {}
        }
    }
}

fn sample_entries() -> Vec<PolicyTraceEntry> {
    let mut counts = BTreeMap::new();
    counts.insert(RedactionTag::Email, 2);
    counts.insert(RedactionTag::Ssn, 1);
    vec![
        PolicyTraceEntry::pass(PolicyGate::PresidioRedaction),
        PolicyTraceEntry::pass(PolicyGate::PromptInjectionFence),
        PolicyTraceEntry::deny(
            PolicyGate::FilterShouldMemorize,
            PolicyDetail::DiscardReason(DiscardReason::PiiBlocked),
        ),
        PolicyTraceEntry::new(
            PolicyGate::PresidioRedaction,
            PolicyOutcome::Pass,
            PolicyDetail::RedactionTagCounts(counts),
        ),
        PolicyTraceEntry::new(
            PolicyGate::VisibilityFloor,
            PolicyOutcome::Pass,
            PolicyDetail::VisibilityFloor(MemoryVisibility::Session),
        ),
        PolicyTraceEntry::deny(
            PolicyGate::ScopeCheck,
            PolicyDetail::ScopeMismatch { required_tier: MemoryVisibility::Project },
        ),
        PolicyTraceEntry::error(PolicyGate::ConsentJournalAppend, "wal_failure"),
    ]
}

#[test]
fn fixed_corpus_is_body_free() {
    let wire = to_wire(&sample_entries());
    let json = serde_json::to_string(&wire).unwrap();
    assert_body_free(&json);
}

proptest::proptest! {
    #[test]
    fn arbitrary_traces_stay_body_free(seed in 0u64..1000) {
        // Deterministic shuffle of the fixed corpus; body-free invariant
        // is purely a function of the variant set we emit.
        let mut entries = sample_entries();
        entries.rotate_left((seed % entries.len() as u64) as usize);
        let wire = to_wire(&entries);
        let json = serde_json::to_string(&wire).unwrap();
        assert_body_free(&json);
    }
}
```

- [ ] **Step 3: Run the test (expect pass — types already done)**

Run: `cargo nextest run -p cairn-core --test policy_trace_body_free`
Expected: PASS — all sample entries are body-free by construction.

- [ ] **Step 4: Verify the test catches a hypothetical violation**

Temporarily add a bad case to confirm the walker is doing real work. Edit the test, replace the `#[test] fn fixed_corpus_is_body_free` body with:

```rust
#[test]
#[should_panic(expected = "must not use field name")]
fn walker_catches_banned_keys() {
    let bad = serde_json::json!([{ "gate": "x", "result": "pass", "body": "leaked" }]);
    assert_body_free(&serde_json::to_string(&bad).unwrap());
}
```

Run: `cargo nextest run -p cairn-core --test policy_trace_body_free walker_catches_banned_keys`
Expected: PASS — the walker panics on the bad case as expected.

- [ ] **Step 5: Restore the original test**

Revert the change from Step 4 so the file matches Step 2.

Run: `cargo nextest run -p cairn-core --test policy_trace_body_free`
Expected: PASS on all three tests (or two, if the proptest is one).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/tests/policy_trace_body_free.rs \
        crates/cairn-core/Cargo.toml \
        Cargo.toml
git commit -m "test(core): policy_trace JSON-walker body-free invariant (#95)"
```

---

### Task 8: Gate vocabulary reference doc

**Files:**
- Create: `docs/site/src/reference/policy-gates.md`
- Modify: `docs/site/src/SUMMARY.md`
- Modify: `crates/cairn-cli/tests/docgen.rs` (snapshot)

- [ ] **Step 1: Locate the SUMMARY.md and confirm reference-section heading**

Run: `grep -n "reference" docs/site/src/SUMMARY.md | head -10`
Expected: a `# Reference` or similar section. Note its line number.

- [ ] **Step 2: Add a SUMMARY entry**

Edit `docs/site/src/SUMMARY.md`. Inside the reference section (alphabetical by title where practical), insert:

```markdown
- [Policy gates](reference/policy-gates.md)
```

- [ ] **Step 3: Write the gate vocabulary page**

Create `docs/site/src/reference/policy-gates.md`:

```markdown
# Policy gates

Cairn populates `policy_trace` on every mutating verb response and on read
verbs when `--explain` is set (brief §8.0.b, §14, §5.1, §5.2). Each entry
names a **gate**, a **result** (`pass`, `deny`, `error`), and an optional
short metadata `detail`. Gate names are stable; the closed producer-side
vocabulary is enumerated below.

## Negotiation

Servers advertise `cairn.mcp.v1.policy_trace` on `status.capabilities`
when they emit traces with this vocabulary. Vocabulary breaks (renames,
semantic shifts) travel with the MCP contract version
(`cairn.mcp.v2.*`) — a fresh closed `Capabilities` enum at that point —
rather than as a `.v2` suffix on this capability, matching the existing
sibling pattern.

## Gate vocabulary

| Gate string                  | Brief         | Fires on         | Typical `result` | Typical `detail`                                |
|------------------------------|---------------|------------------|------------------|--------------------------------------------------|
| `presidio_redaction`         | §5.2, §14     | every write      | `pass`           | `redacted:<tag>=<count>,…` (or absent)          |
| `prompt_injection_fence`     | §5.2          | every write      | `pass`           | (always absent — fencing wraps, never rejects)  |
| `filter_should_memorize`     | §5.2          | every write      | `pass` / `deny`  | `discard:<reason>` on deny                       |
| `visibility_floor`           | §6.3          | every write      | `pass`           | `floor:<tier>`                                   |
| `scope_check`                | §4.2          | every verb       | `pass` / `deny`  | `scope_required:<tier>` on deny                  |
| `forget_capability`          | §8            | `forget`         | `pass` / `deny`  | absent / capability code                         |
| `consent_journal_append`     | §14, §5.6     | every mutation   | `pass` / `error` | `error:<code>` on error                          |
| `read_filter_relevance`      | §5.1          | `search` / `retrieve` `--explain` | `pass`  | (per-record entries in `excluded`)               |
| `read_filter_staleness`      | §5.1          | `search` / `retrieve` `--explain` | `pass`  | (per-record entries in `excluded`)               |
| `read_filter_dedup`          | §5.1          | `search` / `retrieve` `--explain` | `pass`  | (per-record entries in `excluded`)               |

## `detail` shape

`detail` is **always body-free**. Variants in producer code:

- `none` — empty / absent on the wire.
- `discard:<reason>` — one of `volatile | tool_lookup | competing_source | low_salience | pii_blocked | injection_blocked | policy_blocked | duplicate`.
- `redacted:<tag>=<count>[,<tag>=<count>…]` — sorted by tag name.
- `floor:<tier>` — one of `private | session | project | team | org | public`.
- `scope_required:<tier>` — same enum.
- `error:<code>` — short stable static code (e.g. `wal_failure`).

Raw bytes, source content, record bodies, request URLs, and free-form
messages never appear in `detail`.

## Visibility rule

A trace entry mentioning a record (only the `excluded` field on
`--explain` does so) is only present for records the caller already had
visibility to. Tier-1-invisible records are filtered before the
rank-and-filter step that builds exclusions; their existence is never
leaked through `policy_trace`.
```

- [ ] **Step 4: Run docgen check (expect pass — this is a hand-written page, not generated)**

Run: `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`
Expected: PASS — docgen does not regenerate hand-written reference pages.

- [ ] **Step 5: Build the book**

Run: `mdbook build docs/site`
Expected: builds with no broken-link warnings touching the new page. If `mdbook` is not installed, install it: `cargo install mdbook --locked`.

- [ ] **Step 6: Update the docgen snapshot test**

Run: `cargo nextest run -p cairn-cli --test docgen`
Expected: PASS or a single insta-pending diff on the SUMMARY snapshot.

If a diff is reported, run: `cargo insta review` and accept the snapshot. Verify the diff is exactly the new SUMMARY line.

- [ ] **Step 7: Commit**

```bash
git add docs/site/src/reference/policy-gates.md \
        docs/site/src/SUMMARY.md \
        crates/cairn-cli/tests/snapshots
git commit -m "docs(reference): policy-gates v1 vocabulary (#95)"
```

---

### Task 9: Advertise the capability on `status` + integration test

**Files:**
- Modify: `crates/cairn-cli/src/verbs/status.rs`
- Modify: `crates/cairn-cli/tests/envelope_tests.rs`

- [ ] **Step 1: Find the current capabilities block in status**

Run: `grep -n "Capabilities::" crates/cairn-cli/src/verbs/status.rs | head -10`
Expected: a list of `Capabilities::Cairn…` insertions into the response. Note the function and roughly the line number.

- [ ] **Step 2: Write the failing assertion in envelope_tests.rs**

Edit `crates/cairn-cli/tests/envelope_tests.rs`. After the existing capability assertions (or after the existing `is_array` checks if the suite doesn't have one yet), add:

```rust
#[test]
fn status_advertises_policy_trace() {
    use assert_cmd::prelude::*;
    use std::process::Command;

    let out = Command::cargo_bin("cairn")
        .unwrap()
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let caps = v["capabilities"].as_array().expect("capabilities array");
    let strs: Vec<&str> = caps.iter().filter_map(serde_json::Value::as_str).collect();
    assert!(
        strs.contains(&"cairn.mcp.v1.policy_trace"),
        "expected cairn.mcp.v1.policy_trace; got {strs:?}"
    );
}
```

> Verify the test harness already uses `assert_cmd`. If not (look for it in other tests in the file), use whatever existing pattern envelope_tests.rs uses — e.g. constructing a Response via `cairn_cli::verbs::status::run()` directly. The assertion shape is the same: `cairn.mcp.v1.policy_trace` must be in the capabilities array.

- [ ] **Step 3: Run the failing test**

Run: `cargo nextest run -p cairn-cli --test envelope_tests status_advertises_policy_trace`
Expected: FAIL — capability not in the array.

- [ ] **Step 4: Add the capability to status.rs**

Edit `crates/cairn-cli/src/verbs/status.rs`. In the function that builds the `Vec<Capabilities>` (or wherever the capabilities array is composed), append:

```rust
Capabilities::CairnMcpV1PolicyTrace,
```

- [ ] **Step 5: Run the test (expect pass)**

Run: `cargo nextest run -p cairn-cli --test envelope_tests status_advertises_policy_trace`
Expected: PASS.

- [ ] **Step 6: Run the full envelope test suite**

Run: `cargo nextest run -p cairn-cli --test envelope_tests`
Expected: PASS — no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/verbs/status.rs \
        crates/cairn-cli/tests/envelope_tests.rs
git commit -m "feat(cli): advertise cairn.mcp.v1.policy_trace on status (#95)"
```

---

### Task 10: PR1 verification & PR

**Files:** none modified; verification only.

- [ ] **Step 1: Format check**

Run: `cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Workspace check**

Run: `cargo check --workspace --all-targets --locked`
Expected: PASS.

- [ ] **Step 4: Full test suite**

Run: `cargo nextest run --workspace --locked --no-fail-fast`
Expected: PASS — all tests including the new policy_trace_* and envelope_tests.

- [ ] **Step 5: Doctests**

Run: `cargo test --doc --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Core boundary**

Run: `./scripts/check-core-boundary.sh`
Expected: PASS — `cairn-core` has no new adapter dependencies.

- [ ] **Step 7: Codegen check**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
Expected: PASS — no diff.

- [ ] **Step 8: Docgen check**

Run: `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`
Expected: PASS — no diff.

- [ ] **Step 9: Rustdoc**

Run: `RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" cargo doc --workspace --no-deps --document-private-items --locked`
Expected: PASS.

- [ ] **Step 10: Supply chain**

Run: `cargo deny check && cargo audit --deny warnings && cargo machete`
Expected: PASS on all three.

- [ ] **Step 11: Open the PR**

Run:

```bash
git push -u origin HEAD
gh pr create --title "feat(core): policy_trace v1 infrastructure (#95)" --body "$(cat <<'EOF'
## Summary

Lands the typed `policy_trace` infrastructure for issue #95 (brief §14 / §5.2 / §5.1 / §8.0.b). The producer-side vocabulary is closed; the wire stays open.

## Design source

`docs/superpowers/specs/2026-04-29-policy-trace-design.md`

## What lands

- `cairn-core::policy_trace` — `PolicyGate` (closed enum, 10 variants), `PolicyOutcome`, `PolicyDetail` (body-free), `PolicyTraceEntry`, `to_wire`.
- `From<&Decision>`, `From<&RedactedPayload>`, `From<&FencedPayload>` impls.
- `Capabilities::CairnMcpV1PolicyTrace` IDL variant + regenerated.
- `cairn status` advertises `cairn.mcp.v1.policy_trace`.
- `docs/site/src/reference/policy-gates.md` — gate vocabulary.

## Out of scope (verb runtime)

The `ingest`, `capture_trace`, `forget`, `search`, `retrieve` verbs are stubs awaiting #9 / #46. Once those land, each verb populates `policy_trace` by composing existing gates and calling `(&result).into()` per the design doc §6.

## Verification

(paste the output of the cargo commands from Steps 1-10)

## Issue #95 acceptance criteria mapping

| AC | Where |
|----|-------|
| Every write decision has explainable trace | type system + `From` impls; verb runtime in #9/#46 |
| Search/retrieve explain filtered records in debug/lint mode | PR2 |
| Traces obey same visibility rules as records | body-free `PolicyDetail` + JSON-walker test |
EOF
)"
```

---

# PR2 — read-explain machinery

### Task 11: `RecordExclusion` type + ReadFilter From impls

**Files:**
- Create: `crates/cairn-core/src/policy_trace/exclusion.rs`
- Modify: `crates/cairn-core/src/policy_trace/mod.rs`
- Create: `crates/cairn-core/tests/policy_trace_exclusion.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/cairn-core/tests/policy_trace_exclusion.rs`:

```rust
use cairn_core::domain::TargetId;
use cairn_core::policy_trace::{PolicyDetail, PolicyGate, RecordExclusion};

// Valid TargetId: 26-char Crockford base32, leading char 0..=7.
const FIXTURE_ID: &str = "01HQZX9F5N0000000000000000";

#[test]
fn exclusion_holds_target_gate_detail() {
    let id = TargetId::parse(FIXTURE_ID).unwrap();
    let e = RecordExclusion::new(id.clone(), PolicyGate::ReadFilterStaleness, PolicyDetail::None);
    // Fields are private; use the `target_id()`, `gate()`, `detail()`
    // accessors. Direct field access (`e.target_id`) does not compile.
    assert_eq!(e.target_id(), &id);
    assert_eq!(e.gate(), PolicyGate::ReadFilterStaleness);
    assert_eq!(e.detail(), &PolicyDetail::None);
}

#[test]
#[should_panic(expected = "ReadFilter")]
fn exclusion_rejects_non_read_filter_gate() {
    let id = TargetId::parse(FIXTURE_ID).unwrap();
    // ScopeCheck is a Tier-1 gate; per §5.5 of the spec, a Tier-1
    // invisible record's id must never appear in `excluded`.
    let _ = RecordExclusion::new(id, PolicyGate::ScopeCheck, PolicyDetail::None);
}
```

A `compile_fail` rustdoc test on `RecordExclusion` itself locks the
field-privacy invariant — the field-literal bypass (`RecordExclusion {
gate: ScopeCheck, … }`) MUST fail to compile. If a future change makes
any field `pub`, the doctest succeeds and `cargo test --doc` fails.
That guard is the regression contract for round 5/6 of PR #237.

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-core --test policy_trace_exclusion`
Expected: FAIL — `RecordExclusion` not found.

- [ ] **Step 3: Implement the type**

Create `crates/cairn-core/src/policy_trace/exclusion.rs`:

```rust
//! Per-record exclusion for read verbs in `--explain` mode (brief §5.1).
//!
//! Tier-1-invisible records (caller's scope cannot see them) never
//! appear in this list; the store query filters them before
//! rank-and-filter runs. Only Tier-2 read-filter gates
//! (`ReadFilter*`) may construct an exclusion.

use crate::domain::TargetId;

use super::{PolicyDetail, PolicyGate};

/// Fields are PRIVATE — the constructor is the only path. External
/// callers cannot bypass the ReadFilter*-only check via
/// `RecordExclusion { gate: ScopeCheck, … }` struct literal. Read-only
/// accessors `target_id()`, `gate()`, `detail()` expose the data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordExclusion {
    target_id: TargetId,
    gate: PolicyGate,
    detail: PolicyDetail,
}

impl RecordExclusion {
    /// Construct an exclusion. Panics on a non-`ReadFilter*` gate; this
    /// is a programmer error and a fail-closed safety check against
    /// leaking record ids through Tier-1 gates.
    #[must_use]
    pub fn new(target_id: TargetId, gate: PolicyGate, detail: PolicyDetail) -> Self {
        assert!(
            matches!(
                gate,
                PolicyGate::ReadFilterRelevance
                    | PolicyGate::ReadFilterStaleness
                    | PolicyGate::ReadFilterDedup
            ),
            "RecordExclusion only accepts ReadFilter* gates; got {gate:?}"
        );
        Self { target_id, gate, detail }
    }

    pub fn target_id(&self) -> &TargetId { &self.target_id }
    pub const fn gate(&self) -> PolicyGate { self.gate }
    pub const fn detail(&self) -> &PolicyDetail { &self.detail }
}
```

- [ ] **Step 4: Re-export**

Edit `crates/cairn-core/src/policy_trace/mod.rs`:

```rust
mod detail;
mod entry;
mod exclusion;
mod from_pipeline;
mod gate;
mod outcome;

pub use detail::PolicyDetail;
pub use entry::{PolicyTraceEntry, to_wire};
pub use exclusion::RecordExclusion;
pub use gate::PolicyGate;
pub use outcome::PolicyOutcome;
```

- [ ] **Step 5: Run the test (expect pass)**

Run: `cargo nextest run -p cairn-core --test policy_trace_exclusion`
Expected: PASS — both tests green (one expects panic).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/policy_trace/exclusion.rs \
        crates/cairn-core/src/policy_trace/mod.rs \
        crates/cairn-core/tests/policy_trace_exclusion.rs
git commit -m "feat(core): RecordExclusion with ReadFilter*-only invariant (#95)"
```

---

### Task 12: `explain_filter` pure function

**Files:**
- Create: `crates/cairn-core/src/pipeline/explain.rs` (with inline `#[cfg(test)] mod tests`)
- Modify: `crates/cairn-core/src/pipeline/mod.rs`

`Candidate` and `explain_filter` are sealed to `cairn-core`:
`Candidate`'s fields are private, the only constructor
`Candidate::from_scope_filter` is `pub(crate)`, and `explain_filter` is
`pub(crate)`. Together this means a candidate can only be produced
inside `cairn-core` on the verb-runtime path that has already applied
scope/visibility predicates, so `explain_filter` cannot be invoked
with unfiltered store rows. Tests live inline so they can call the
`pub(crate)` constructor; there is no external integration test file.

- [ ] **Step 1: Write the failing tests (inline)**

In `crates/cairn-core/src/pipeline/explain.rs`, add the inline
`#[cfg(test)] mod tests` block alongside the implementation:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy_trace::{PolicyDetail, PolicyGate};

    fn id(suffix: char) -> TargetId {
        let mut s = String::from("01HQZX9F5N0000000000000000");
        s.pop();
        s.push(suffix);
        TargetId::parse(s).unwrap()
    }

    #[test]
    fn empty_candidates_yields_empty_kept_and_excluded() {
        let cfg = ExplainConfig { staleness_threshold_days: 30 };
        let (kept, excluded) = explain_filter(Vec::<Candidate>::new(), cfg);
        assert!(kept.is_empty());
        assert!(excluded.is_empty());
    }

    #[test]
    fn stale_candidate_is_excluded_with_staleness_gate() {
        let cfg = ExplainConfig { staleness_threshold_days: 30 };
        let candidates = vec![
            Candidate::from_scope_filter(id('A'), 90, 0.8, "h1".to_owned()),
        ];
        let (kept, excluded) = explain_filter(candidates, cfg);
        assert!(kept.is_empty());
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].gate(), PolicyGate::ReadFilterStaleness);
        assert_eq!(excluded[0].detail(), &PolicyDetail::None);
    }

    #[test]
    fn duplicate_content_hash_excluded_by_dedup() {
        let cfg = ExplainConfig { staleness_threshold_days: 30 };
        let candidates = vec![
            Candidate::from_scope_filter(id('A'), 1, 0.9, "h".to_owned()),
            Candidate::from_scope_filter(id('B'), 1, 0.8, "h".to_owned()),
        ];
        let (kept, excluded) = explain_filter(candidates, cfg);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].target_id(), &id('A')); // higher relevance wins
        assert_eq!(excluded[0].target_id(), &id('B'));
        assert_eq!(excluded[0].gate(), PolicyGate::ReadFilterDedup);
    }

    // Plus NaN coverage and ReadFilterReason round-trip tests — see the
    // shipped file for the full set.
}
```

- [ ] **Step 2: Run the failing test**

Run: `cargo nextest run -p cairn-core` and look for the `explain`
inline tests.
Expected: FAIL — `pipeline::explain` not found.

- [ ] **Step 3: Implement the explain module**

Create `crates/cairn-core/src/pipeline/explain.rs`:

```rust
//! `explain_filter` — pure partition of caller-visible candidates into
//! kept and excluded subsets (brief §5.1). Tier-1-invisible records
//! are filtered upstream and must not appear in the input.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::domain::TargetId;
use crate::policy_trace::{PolicyDetail, PolicyGate, RecordExclusion};

/// Sealed type: fields private; only constructor `from_scope_filter`
/// is `pub(crate)`. External crates cannot synthesize a candidate.
#[derive(Debug, Clone)]
pub struct Candidate {
    target_id: TargetId,
    age_days: u32,
    relevance_score: f32,
    content_hash: String,
}

impl Candidate {
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn from_scope_filter(
        target_id: TargetId,
        age_days: u32,
        relevance_score: f32,
        content_hash: String,
    ) -> Self {
        Self { target_id, age_days, relevance_score, content_hash }
    }
    pub fn target_id(&self) -> &TargetId { &self.target_id }
    pub const fn age_days(&self) -> u32 { self.age_days }
    pub const fn relevance_score(&self) -> f32 { self.relevance_score }
    pub fn content_hash(&self) -> &str { &self.content_hash }
}

#[derive(Debug, Clone, Copy)]
pub struct ExplainConfig {
    pub staleness_threshold_days: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadFilterReason { Relevance, Staleness, Dedup }

impl ReadFilterReason {
    #[must_use]
    pub const fn as_gate(self) -> PolicyGate {
        match self {
            Self::Relevance => PolicyGate::ReadFilterRelevance,
            Self::Staleness => PolicyGate::ReadFilterStaleness,
            Self::Dedup => PolicyGate::ReadFilterDedup,
        }
    }
}

/// Sealed to cairn-core. `cfg` taken by value (it's `Copy` and small).
/// NaN scores lose to non-NaN; two NaNs resolve to first-seen.
#[allow(dead_code)]
#[must_use]
pub(crate) fn explain_filter(
    candidates: Vec<Candidate>,
    cfg: ExplainConfig,
) -> (Vec<Candidate>, Vec<RecordExclusion>) {
    let mut excluded: Vec<RecordExclusion> = Vec::new();

    // 1. Staleness pass — preserve original order.
    let mut after_stale: Vec<Candidate> = Vec::with_capacity(candidates.len());
    for c in candidates {
        if c.age_days > cfg.staleness_threshold_days {
            excluded.push(RecordExclusion::new(
                c.target_id.clone(),
                PolicyGate::ReadFilterStaleness,
                PolicyDetail::None,
            ));
        } else {
            after_stale.push(c);
        }
    }

    // 2. Dedup by content_hash — keep highest-relevance per hash.
    //    Non-NaN beats NaN; two NaNs resolve to first-seen.
    let mut best: HashMap<String, usize> = HashMap::new();
    for (idx, c) in after_stale.iter().enumerate() {
        match best.get(&c.content_hash) {
            None => { best.insert(c.content_hash.clone(), idx); }
            Some(&prev) => {
                let prev_score = after_stale[prev].relevance_score;
                let cur_score = c.relevance_score;
                let cur_wins = !cur_score.is_nan()
                    && (prev_score.is_nan() || cur_score > prev_score);
                if cur_wins {
                    best.insert(c.content_hash.clone(), idx);
                }
            }
        }
    }
    let kept_indices: HashSet<usize> = best.values().copied().collect();
    let mut kept: Vec<Candidate> = Vec::with_capacity(kept_indices.len());
    for (idx, c) in after_stale.into_iter().enumerate() {
        if kept_indices.contains(&idx) {
            kept.push(c);
        } else {
            excluded.push(RecordExclusion::new(
                c.target_id,
                PolicyGate::ReadFilterDedup,
                PolicyDetail::None,
            ));
        }
    }

    (kept, excluded)
}
```

- [ ] **Step 4: Wire the module**

Edit `crates/cairn-core/src/pipeline/mod.rs`. Add:

```rust
pub mod explain;
```

Verify by running: `grep -n "pub mod" crates/cairn-core/src/pipeline/mod.rs`. Expected: a list including the new `explain` module.

- [ ] **Step 5: Run the test (expect pass)**

Run: `cargo nextest run -p cairn-core --test explain_filter`
Expected: PASS — all five tests green.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/pipeline/explain.rs \
        crates/cairn-core/src/pipeline/mod.rs \
        crates/cairn-core/tests/explain_filter.rs
git commit -m "feat(core): explain_filter pure partition for --explain (#95)"
```

---

### Task 13: IDL — `explain` arg + `excluded` field on search/retrieve

**Files:**
- Create: `crates/cairn-idl/schema/common/record_exclusion.json`
- Modify: `crates/cairn-idl/schema/verbs/search.json`
- Modify: `crates/cairn-idl/schema/verbs/retrieve.json`
- Regenerate: `crates/cairn-core/src/generated/verbs/{search,retrieve}.rs`

- [ ] **Step 1: Author the RecordExclusion schema**

Create `crates/cairn-idl/schema/common/record_exclusion.json`:

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://cairn.dev/schema/cairn.mcp.v1/common/record_exclusion.json",
  "title": "RecordExclusion",
  "description": "Per-record exclusion entry returned on search/retrieve responses when args.explain is true. Records the caller cannot otherwise see (Tier-1 visibility) never appear here.",
  "type": "object",
  "additionalProperties": false,
  "required": ["target_id", "gate", "detail"],
  "properties": {
    "target_id": { "$ref": "../common/primitives.json#/$defs/TargetId" },
    "gate": {
      "type": "string",
      "enum": ["read_filter_relevance", "read_filter_staleness", "read_filter_dedup"]
    },
    "detail": { "type": "string" }
  }
}
```

> Verify the `TargetId` `$ref` path matches an existing definition in `primitives.json`. Run: `grep -n "TargetId" crates/cairn-idl/schema/common/primitives.json`. If `TargetId` is defined elsewhere, update the `$ref` to match.

- [ ] **Step 2: Add `args.explain` and `data.excluded` to search.json**

Edit `crates/cairn-idl/schema/verbs/search.json`. Inside `$defs.Args.properties`, add:

```json
"explain": {
  "type": "boolean",
  "default": false,
  "description": "When true, populate policy_trace and data.excluded with per-record exclusions for Tier-2 read filters. Has no effect on the candidate set the caller could see."
}
```

Inside `$defs.Data.properties`, add:

```json
"excluded": {
  "type": "array",
  "items": { "$ref": "../common/record_exclusion.json" },
  "description": "Per-record exclusions; present only when args.explain is true."
}
```

Do **not** add `excluded` to `$defs.Data.required`.

- [ ] **Step 3: Mirror the changes in retrieve.json**

Edit `crates/cairn-idl/schema/verbs/retrieve.json`. Apply the same two additions: `args.explain` (boolean, default false) and `data.excluded?` (optional array of `record_exclusion`). Place them in the same `$defs.Args.properties` and `$defs.Data.properties` blocks.

- [ ] **Step 4: Run codegen check (expect failure)**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
Expected: FAIL — diff against `crates/cairn-core/src/generated/verbs/search.rs` and `retrieve.rs`.

- [ ] **Step 5: Regenerate**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked`
Expected: writes the new `explain: bool` field on each verb's `Args` and `excluded: Option<Vec<RecordExclusion>>` on each `Data`.

- [ ] **Step 6: Verify the regenerated types**

Run: `grep -n "explain\|excluded" crates/cairn-core/src/generated/verbs/search.rs`
Expected: a `pub explain: bool` field on `SearchArgs` and a `pub excluded: Option<Vec<…>>` on `SearchData`. Same for `retrieve.rs`.

- [ ] **Step 7: Run codegen check again (expect pass)**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
Expected: PASS.

- [ ] **Step 8: Run workspace check**

Run: `cargo check --workspace --all-targets --locked`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-idl/schema/common/record_exclusion.json \
        crates/cairn-idl/schema/verbs/search.json \
        crates/cairn-idl/schema/verbs/retrieve.json \
        crates/cairn-core/src/generated/verbs/search.rs \
        crates/cairn-core/src/generated/verbs/retrieve.rs
git commit -m "feat(idl): args.explain + data.excluded on search/retrieve (#95)"
```

---

### Task 14: Default-path byte-identity snapshot test

**Files:**
- Modify: `crates/cairn-cli/tests/envelope_tests.rs`

- [ ] **Step 1: Add the byte-identity assertion**

Edit `crates/cairn-cli/tests/envelope_tests.rs`. Add:

```rust
#[test]
fn search_default_response_has_no_excluded_field() {
    use cairn_core::generated::envelope::Response;
    use cairn_core::generated::verbs::search::SearchData;

    // Construct the smallest valid SearchData with default fields and assert
    // that `excluded` is absent (None) and that the JSON serialization
    // contains no `"excluded"` key at all.
    let data = SearchData::default();
    let json = serde_json::to_string(&data).unwrap();
    assert!(
        !json.contains("\"excluded\""),
        "default SearchData must not emit excluded field; got {json}"
    );
}

#[test]
fn retrieve_default_response_has_no_excluded_field() {
    use cairn_core::generated::verbs::retrieve::DataRecord;

    let data = DataRecord::default();
    let json = serde_json::to_string(&data).unwrap();
    assert!(
        !json.contains("\"excluded\""),
        "default DataRecord must not emit excluded field; got {json}"
    );
}
```

> If `SearchData::default()` or `DataRecord::default()` doesn't compile (codegen may not derive `Default`), run: `grep -n "pub struct SearchData\|pub struct DataRecord" crates/cairn-core/src/generated/verbs/{search,retrieve}.rs` and adapt the test to construct the struct with explicit fields. The point of the test is the negative `assert!` on the serialized form — the construction shape is incidental.

- [ ] **Step 2: Run the test (expect pass)**

Run: `cargo nextest run -p cairn-cli --test envelope_tests search_default_response_has_no_excluded_field retrieve_default_response_has_no_excluded_field`
Expected: PASS — `excluded` is `Option::None` and `skip_serializing_if` keeps it off the wire.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-cli/tests/envelope_tests.rs
git commit -m "test(cli): default search/retrieve responses omit excluded field (#95)"
```

---

### Task 15: PR2 verification & PR

**Files:** none modified; verification only.

- [ ] **Step 1: Run the full PR1 verification checklist**

Run each of these in turn (same as Task 10 Steps 1–10), expecting PASS on each:

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
cargo deny check && cargo audit --deny warnings && cargo machete
```

- [ ] **Step 2: Open PR2**

Run:

```bash
git push -u origin HEAD
gh pr create --title "feat(core): explain_filter + RecordExclusion for search/retrieve (#95)" --body "$(cat <<'EOF'
## Summary

PR2 of issue #95 — adds the read-explain machinery (`explain_filter` pure function, `RecordExclusion` type, IDL changes for `search` and `retrieve` `--explain`).

## Design source

`docs/superpowers/specs/2026-04-29-policy-trace-design.md`, Sections 6.4, 7.2, 11.3.

## What lands

- `cairn-core::policy_trace::RecordExclusion` — programmer-error panic on non-`ReadFilter*` gates (fail-closed against Tier-1 leak).
- `cairn-core::pipeline::explain::explain_filter` — pure staleness + dedup partition.
- IDL: `args.explain: bool = false` and optional `data.excluded?: RecordExclusion[]` on both `search` and `retrieve`.
- Default-path snapshot test asserts `excluded` is absent when `explain=false`.

## Wire compat

- `explain` defaults to `false` → existing callers see byte-identical responses.
- `excluded` is `skip_serializing_if Option::is_none` → no wire churn unless requested.

## Out of scope

CLI flag wiring on `search` / `retrieve` — those verbs are stubs awaiting #9 / #46. This PR makes the machinery available for #9/#46 to call.

## Verification

(paste the output of the cargo commands from Step 1)
EOF
)"
```

---

## Self-review checklist (run after writing each task)

1. **Spec coverage:** Every spec section maps to a task or is explicitly deferred (verb runtime → #9/#46). ✓
2. **Placeholder scan:** `grep -nE "TBD|TODO|FIXME|implement later|fill in" docs/superpowers/plans/2026-04-29-policy-trace.md` → expect zero matches.
3. **Type consistency:**
   - `PolicyGate::PresidioRedaction` (not `Redaction`/`Presidio`) — used uniformly.
   - `PolicyTraceEntry::pass / deny / error` constructors used in both Tasks 5 and 7.
   - `PolicyDetail::ScopeMismatch { required_tier }` field name matches between Tasks 4 and the body-free walker in Task 7.
   - `RecordExclusion::new` panics on non-`ReadFilter*` (Task 11) — `explain_filter` (Task 12) only constructs with `ReadFilterStaleness` / `ReadFilterDedup`.
4. **No duplicate code:** the `From` impls in Task 6 are the only producer-side mapping; verbs (when wired in #9/#46) will not re-implement.
