# Issue 106 OpenAI Semantic Degradation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the explicit `semantic_degraded` search response flag required by issue #106 while preserving existing `degraded_legs` diagnostics.

**Architecture:** Extend the search IDL response data with an optional boolean, regenerate the SDK/MCP schemas, derive the flag in `cairn-core::verbs::search` from semantic degraded-leg reasons, and have the CLI envelope mapper serialize the field. The OpenAI adapter remains opt-in and unchanged.

**Tech Stack:** Rust 2024, `cairn-idl` codegen, `cairn-core`, `cairn-cli`, `serde_json`, generated IDL types.

---

### Task 1: IDL Wire Field

**Files:**
- Modify: `crates/cairn-idl/schema/verbs/search.json`
- Regenerate: `crates/cairn-core/src/generated/verbs/search.rs`
- Regenerate: `crates/cairn-core/src/generated/schemas/verbs/search.json`
- Regenerate: `crates/cairn-mcp/src/generated/schemas/verbs/search.json`
- Test: `crates/cairn-cli/tests/envelope_tests.rs`

- [ ] **Step 1: Write failing serialization tests**

Add two tests near the existing `SearchData` degraded-leg tests:

```rust
#[test]
fn search_without_semantic_degraded_omits_field() {
    use cairn_core::generated::verbs::search::SearchData;

    let data = SearchData {
        excluded: None,
        hits: Vec::new(),
        next_cursor: None,
        score_explain: None,
        degraded_legs: None,
        semantic_degraded: None,
    };
    let json = serde_json::to_string(&data).expect("serializable");
    assert!(
        !json.contains("\"semantic_degraded\""),
        "healthy SearchData must not emit semantic_degraded; got {json}"
    );
}

#[test]
fn search_with_semantic_degraded_emits_true() {
    use cairn_core::generated::verbs::search::SearchData;

    let data = SearchData {
        excluded: None,
        hits: Vec::new(),
        next_cursor: None,
        score_explain: None,
        degraded_legs: None,
        semantic_degraded: Some(true),
    };
    let json = serde_json::to_string(&data).expect("serializable");
    assert!(
        json.contains("\"semantic_degraded\":true"),
        "degraded SearchData must emit semantic_degraded=true; got {json}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cairn-cli --test envelope_tests search_without_semantic_degraded_omits_field --features openai`

Expected: FAIL because `SearchData` has no `semantic_degraded` field.

- [ ] **Step 3: Add the IDL field and regenerate**

Add this property under `Data.properties` in `crates/cairn-idl/schema/verbs/search.json`:

```json
"semantic_degraded": {
  "type": "boolean",
  "description": "True when the response succeeded after a transient semantic embedding-provider outage. Absent for healthy responses and for fail-closed capability errors."
}
```

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked`

Expected: generated `SearchData` includes `pub semantic_degraded: Option<bool>` with `skip_serializing_if = "Option::is_none"`.

- [ ] **Step 4: Run serialization tests**

Run: `cargo test -p cairn-cli --test envelope_tests semantic_degraded --features openai`

Expected: PASS.

### Task 2: Core Degradation Semantics

**Files:**
- Modify: `crates/cairn-core/src/search/degraded.rs`
- Modify: `crates/cairn-core/src/verbs/search.rs`

- [ ] **Step 1: Write failing core test**

Extend the existing degraded hybrid store test so the store returns:

```rust
degraded_legs: vec![DegradedLeg::Semantic {
    reason: DegradationReason::TransientProviderOutage,
}],
```

Then assert:

```rust
assert!(
    outcome.semantic_degraded,
    "transient semantic provider outage must set semantic_degraded"
);
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cairn-core verbs::search::tests::hybrid_propagates_degraded_legs_through_search_outcome`

Expected: FAIL because `TransientProviderOutage` and `SearchOutcome.semantic_degraded` do not exist.

- [ ] **Step 3: Implement minimal core support**

Add `TransientProviderOutage` to `DegradationReason`, add `semantic_degraded: bool` to `SearchOutcome`, and set it in `run` with:

```rust
let semantic_degraded = degraded_legs.iter().any(|leg| {
    matches!(
        leg,
        crate::search::DegradedLeg::Semantic {
            reason: crate::search::DegradationReason::TransientProviderOutage
        }
    )
});
```

Keyword and non-transient semantic degradation must leave the flag false.

- [ ] **Step 4: Run core tests**

Run: `cargo test -p cairn-core verbs::search::tests::hybrid_propagates_degraded_legs_through_search_outcome`

Expected: PASS.

### Task 3: CLI Envelope Mapping

**Files:**
- Modify: `crates/cairn-cli/src/verbs/search.rs`
- Modify: `crates/cairn-cli/tests/envelope_tests.rs`

- [ ] **Step 1: Write failing CLI mapping test**

Add or extend an envelope test to build a `SearchOutcome` with `semantic_degraded: true`, call `outcome_envelope`, and assert the serialized search data contains `"semantic_degraded": true`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p cairn-cli --test envelope_tests semantic_degraded --features openai`

Expected: FAIL because `outcome_envelope` does not populate the field.

- [ ] **Step 3: Map the field**

In `outcome_envelope`, set:

```rust
semantic_degraded: outcome.semantic_degraded.then_some(true),
```

- [ ] **Step 4: Run CLI tests**

Run: `cargo test -p cairn-cli --test envelope_tests semantic_degraded --features openai`

Expected: PASS.

### Task 4: Verification

**Files:**
- All changed files

- [ ] **Step 1: Run codegen drift check**

Run: `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`

Expected: clean.

- [ ] **Step 2: Run targeted tests**

Run:

```bash
cargo test -p cairn-core verbs::search::tests::hybrid_propagates_degraded_legs_through_search_outcome
cargo test -p cairn-cli --test envelope_tests semantic_degraded --features openai
cargo test -p cairn-cli --test openai_provider_status --features openai
cargo test -p cairn-embeddings-openai --features openai
```

Expected: all PASS.

- [ ] **Step 3: Run build check**

Run: `cargo build -p cairn-cli --features openai`

Expected: PASS.
