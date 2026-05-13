# Issue 113 FrontendAdapter Contract Design

## Summary

Issue [#113](https://github.com/windoliver/cairn/issues/113) implements the
P1 `FrontendAdapter` contract slice defined by the Cairn design brief in
§13.5.c and §13.5.d. The goal is to replace the current forward stub with a
real core contract surface, a reusable conformance runner, and a single
source of truth for frontend field mutability. This issue does not implement
any concrete frontend adapter and does not include desktop GUI work.

## Design Sources

- `docs/design/design-brief.md` §4.1 contract conformance rules
- `docs/design/design-brief.md` §13.5.c backend to frontend bridge
- `docs/design/design-brief.md` §13.5.d `FrontendAdapter` contract
- `docs/design/traceability.md` rows for §4, §12, and §13

## Scope

### In Scope

- Expand `cairn-core::contract::frontend_adapter` from a registry stub into a
  real contract surface.
- Introduce typed core-only frontend projection and reconcile request types.
- Encode field-level mutability rules from brief §13.5.c in Rust types and
  tests.
- Add a `FrontendAdapter` conformance runner under
  `cairn-core::contract::conformance`.
- Add reusable tests for the issue acceptance criteria.
- Update traceability if the implementation materially changes coverage notes.

### Out of Scope

- Desktop GUI implementation.
- Obsidian, VS Code, Logseq, raw-markdown, or desktop adapter crates.
- File-watcher daemon behavior, OS user detection, keychain prompts, or
  quarantine storage backends.
- New verbs or CLI subcommands.
- Full replay-ledger enforcement for nonce reuse beyond the pure contract test
  model needed by this issue.

## Problem

`FrontendAdapter` currently exists only as a registration placeholder in
`cairn-core`. The trait advertises name, capabilities, and supported versions,
but it cannot express the projection or reconcile flow required by the brief.
The conformance framework also has no `FrontendAdapter` runner, so
`cairn plugins verify` would fail any manifest that declared the contract.

This leaves three gaps relative to issue #113:

1. There is no typed interface for adapter projection and reverse-edit
   translation.
2. There is no reusable conformance suite for safe frontend behavior.
3. Field mutability rules live only in design prose rather than in a core type
   that tests and future adapters can share.

## Goals

- Keep the contract pure and harness-agnostic inside `cairn-core`.
- Express the brief's backend-authoritative reconcile model without pulling
  storage, daemon, or frontend-specific I/O into core.
- Reuse existing plugin manifest and tiered conformance patterns.
- Make the allowed frontend edit surface explicit and testable.

## Non-Goals

- Simulating a full daemon-minted signed-intent flow in this issue.
- Implementing backend apply logic against SQLite or the WAL.
- Solving every future adapter-specific projection format detail.

## Proposed Approach

### 1. Expand the Contract Surface

`crates/cairn-core/src/contract/frontend_adapter.rs` will define the contract
types needed to model the P1 bridge without crossing into adapter I/O:

- `FrontendAdapterCapabilities`
  - Replace the current three booleans with the brief-aligned capability set:
    frontmatter projection, sidecar projection, live plugin channel, graph
    view, and a `max_frontmatter_fields` bound.
- `FrontendProjection`
  - Pure output describing what the backend wants surfaced to the frontend:
    projected frontmatter fields, sidecar documents, optional live-event
    support flags, and the canonical backend content hash the projection was
    derived from.
- `FrontendEdit`
  - Pure description of an editor-originated change. This carries the target
    record id, the observed backend version, a file hash or target hash, and a
    field-level diff broken into mutable and immutable categories.
- `FrontendIdentityContext`
  - Minimal identity wrapper for reconcile requests. It will carry the
    effective principal ids plus a verified signed intent type already present
    in core, rather than inventing a parallel signature model.
- `FrontendReconcileRequest`
  - Pure output of `FrontendAdapter::reconcile`, containing target id,
    expected version, the validated mutable diff, and the identity context.
- `FrontendReconcileError`
  - Typed rejection reasons matching the brief's contract-level outcomes:
    conflict, unsigned intent, replay detected, immutable field changed,
    insufficient capability, and policy denied.

The trait shape will follow the brief closely:

- `capabilities()`
- `project(...) -> Result<FrontendProjection, FrontendAdapterError>`
- `reconcile(...) -> Result<FrontendReconcileRequest, FrontendAdapterError>`
- `subscribe(...)` defaulting to `None`
- `shutdown()` defaulting to no-op

The contract remains in `cairn-core` and remains free of filesystem, daemon,
SQLite, or frontend SDK dependencies.

### 2. Encode Field Mutability as Core Data

The brief's §13.5.c mutability table will become a first-class core model
instead of remaining prose only.

Add a small classification enum:

- `FrontendFieldClass::UserContent`
- `FrontendFieldClass::Metadata`
- `FrontendFieldClass::Classification`
- `FrontendFieldClass::IdentityProvenance`
- `FrontendFieldClass::VisibilityConsent`
- `FrontendFieldClass::VersionAudit`

Add a helper used by tests and adapters:

- `FrontendFieldPolicy::classify(field_name: &str) -> FrontendFieldClass`
- `FrontendFieldPolicy::is_mutable_from_frontend(field_name: &str) -> bool`

This policy will encode the issue-required rules:

- mutable: note body, tags, wikilinks, informational metadata
- immutable: classification, provenance, consent, visibility, version, and
  audit fields

Unknown fields will fail closed as immutable. That matches the repo's
capability and privacy stance and avoids silently widening the edit surface.

### 3. Add a Real Conformance Runner

Add `crates/cairn-core/src/contract/conformance/frontend_adapter.rs` and route
`ContractKind::FrontendAdapter` to it from the conformance entry point.

Tier 1 will mirror the existing contract runners:

- `manifest_matches_host`
- `arc_pointer_stable`
- `capability_self_consistency_floor`
- `manifest_features_match_capabilities`

Tier 2 will encode the brief's frontend-specific invariants as pure cases:

- `rejects_immutable_field_edits`
- `rejects_replayed_operation`
- `rejects_tampered_target_hash`
- `rejects_unrecognized_principal`
- `honors_optimistic_version_check`

These cases will run against test adapters and synthetic fixtures rather than
real I/O. The purpose is contract conformance, not backend integration.

### 4. Define the Test Strategy

Tests will be added in layers:

- Unit tests beside `frontend_adapter.rs`
  - field classification and mutability defaults
  - typed error mapping for immutable-field and conflict paths
- Conformance tests in `crates/cairn-core/tests/`
  - verify a well-formed stub frontend adapter passes tier 1
  - verify tier 2 frontend cases execute and assert the expected statuses
- Fixture-style tests
  - table-driven checks for mutable vs immutable fields named in §13.5.c

This issue will not claim end-to-end daemon or store integration tests because
those belong to the later adapter and backend implementation issues.

## Data Model Sketch

Implementation may refine helper struct names to match existing core naming,
but the trait boundary and responsibilities below are the required shape for
this issue:

```rust
pub trait FrontendAdapter: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> &FrontendAdapterCapabilities;
    fn supported_contract_versions(&self) -> VersionRange;

    fn project(
        &self,
        request: &FrontendProjectionRequest,
    ) -> Result<FrontendProjection, FrontendAdapterError>;

    fn reconcile(
        &self,
        ctx: FrontendIdentityContext,
        edit: FrontendEdit,
    ) -> Result<FrontendReconcileRequest, FrontendAdapterError>;

    fn subscribe(&self, _events: FrontendEventStream) -> Option<FrontendSubscription> {
        None
    }

    fn shutdown(&self) {}
}
```

The request and response types will stay core-native and serializable where
helpful, but they are not a new IDL surface in this issue.

## Error Handling

This issue will follow repo conventions:

- Use `thiserror`-style typed enums in library code.
- No `anyhow` in `cairn-core`.
- No `unwrap()` or `expect()` in production core code.

`FrontendAdapterError` will cover adapter-local projection and reconcile
failures, while `FrontendReconcileError` will model the contract-level reject
reasons that conformance cases assert against.

## Compatibility and Migration

- The contract version in `frontend_adapter.rs` will bump because the trait
  surface materially changes.
- Existing stub registration tests will be updated to implement the richer
  trait.
- There are no bundled frontend plugins yet, so this is a source-compatible
  move for the repo itself but a semver-significant change for future external
  adapters.

## Risks and Mitigations

### Risk: Over-modeling future adapter needs

Mitigation:
Keep the types centered on the issue acceptance criteria and the brief's
contract sketch. Do not introduce filesystem-specific or GUI-specific payloads.

### Risk: Pulling backend apply semantics into the adapter contract

Mitigation:
Stop at `FrontendReconcileRequest`. The adapter translates edits; it does not
apply them. Store and WAL behavior remain outside this issue.

### Risk: Ambiguity around unknown frontend fields

Mitigation:
Fail closed by classifying unknown fields as immutable until a later brief or
issue explicitly widens the policy.

## Verification Plan

At minimum, the implementation for #113 should run:

```bash
cargo test -p cairn-core frontend_adapter
cargo test -p cairn-core conformance_tier1
cargo test -p cairn-core contract_registry
cargo check -p cairn-core --locked
```

If the implementation touches shared conformance routing or generated manifests,
expand verification to the workspace checks required by the repo checklist.

## Expected Outcome

After this issue lands:

- `FrontendAdapter` is a real P1 core contract rather than a placeholder.
- `cairn-core` owns the reusable mutability policy and conformance cases for
  frontend adapters.
- Later adapter issues can implement Obsidian, VS Code, Logseq, raw markdown,
  or desktop-specific behavior without changing the core contract shape again.
