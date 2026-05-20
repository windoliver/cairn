# Issue 113 FrontendAdapter Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `FrontendAdapter` stub in `cairn-core` with a real P1 contract surface, reusable frontend conformance runner, and explicit field-mutability policy that satisfies issue #113 and brief §13.5.c / §13.5.d.

**Architecture:** Keep everything inside `cairn-core` as pure contract and conformance code. Add narrowly scoped frontend request/response/error/policy types to `frontend_adapter.rs`, route `ContractKind::FrontendAdapter` through a new conformance module, and update compile-path / registry tests to the richer trait surface without introducing any concrete frontend adapter crate.

**Tech Stack:** Rust 1.95, `async_trait`, existing `cairn-core` contract/conformance framework, `cargo test`, `cargo check`.

---

## File Map

- Modify: `crates/cairn-core/src/contract/frontend_adapter.rs`
  - Replace the forward stub with the richer trait, capability struct, reconcile/project types, field-policy helpers, and typed errors.
- Modify: `crates/cairn-core/src/contract/mod.rs`
  - Re-export any new public frontend contract types.
- Modify: `crates/cairn-core/src/contract/conformance/mod.rs`
  - Add `frontend_adapter` module and route `ContractKind::FrontendAdapter` to it.
- Create: `crates/cairn-core/src/contract/conformance/frontend_adapter.rs`
  - Implement tier-1 and tier-2 frontend conformance cases.
- Modify: `crates/cairn-core/tests/contract_root_exports.rs`
  - Keep compile-path coverage for newly re-exported frontend types.
- Modify: `crates/cairn-core/tests/contract_registry.rs`
  - Update the stub frontend plugin to implement the richer trait methods.
- Modify: `crates/cairn-core/tests/conformance_tier1.rs`
  - Add a well-formed frontend stub and assert frontend tier-1 cases pass.
- Create: `crates/cairn-core/tests/frontend_adapter_contract.rs`
  - Add field-mutability, reconcile, and frontend tier-2 conformance tests focused on issue #113 behavior.
- Modify: `docs/design/traceability.md`
  - Only if implementation meaningfully changes the current coverage note for `#113`.

## Task 1: Add Failing Frontend Contract Tests

**Files:**
- Create: `crates/cairn-core/tests/frontend_adapter_contract.rs`
- Modify: `crates/cairn-core/tests/contract_root_exports.rs`
- Modify: `crates/cairn-core/tests/conformance_tier1.rs`

- [ ] **Step 1: Write the failing field-policy and trait-shape tests**

Add a new integration test file with concrete expectations for mutability defaults and typed frontend errors:

```rust
use cairn_core::contract::frontend_adapter::{
    FrontendAdapterCapabilities, FrontendFieldClass, FrontendFieldPolicy,
    FrontendReconcileError,
};

#[test]
fn frontend_field_policy_allows_user_content_and_metadata_only() {
    assert!(FrontendFieldPolicy::is_mutable_from_frontend("body"));
    assert!(FrontendFieldPolicy::is_mutable_from_frontend("tags"));
    assert!(FrontendFieldPolicy::is_mutable_from_frontend("last_read_at"));

    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("kind"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("operation_id"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("visibility"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("version"));
    assert!(!FrontendFieldPolicy::is_mutable_from_frontend("unknown_future_field"));
}

#[test]
fn frontend_field_policy_classifies_unknown_fields_as_immutable() {
    assert_eq!(
        FrontendFieldPolicy::classify("unknown_future_field"),
        FrontendFieldClass::VersionAudit
    );
}

#[test]
fn frontend_capabilities_default_to_no_projection_features() {
    let caps = FrontendAdapterCapabilities::default();
    assert!(!caps.frontmatter);
    assert!(!caps.sidecar_files);
    assert!(!caps.live_plugin);
    assert!(!caps.graph_view);
    assert_eq!(caps.max_frontmatter_fields, 0);
}

#[test]
fn frontend_reconcile_error_exposes_immutable_field_variant() {
    let err = FrontendReconcileError::ImmutableFieldChanged {
        field: "operation_id".into(),
    };
    let rendered = err.to_string();
    assert!(rendered.contains("operation_id"));
}
```

- [ ] **Step 2: Extend compile-path coverage for the new frontend types**

Add type reachability checks in `crates/cairn-core/tests/contract_root_exports.rs`:

```rust
use cairn_core::contract::{
    FrontendAdapterCapabilities, FrontendFieldClass, FrontendFieldPolicy,
    FrontendReconcileError,
};

#[test]
fn frontend_contract_types_are_reachable() {
    let _: FrontendAdapterCapabilities = FrontendAdapterCapabilities::default();
    let _: FrontendFieldClass = FrontendFieldClass::UserContent;
    let _ = FrontendFieldPolicy::is_mutable_from_frontend("body");
    let _: FrontendReconcileError = FrontendReconcileError::UnsignedIntent;
}
```

- [ ] **Step 3: Add a failing frontend tier-1 conformance test**

Append a frontend stub case to `crates/cairn-core/tests/conformance_tier1.rs`:

```rust
#[test]
fn tier1_cases_pass_for_well_formed_frontend_adapter() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-frontend").expect("valid");
    let manifest = PluginManifest::parse_toml(FRONTEND_MANIFEST).expect("manifest parses");
    reg.register_frontend_adapter_with_manifest(
        name.clone(),
        manifest,
        Arc::new(StubFrontendAdapter::default()),
    )
    .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);
    let tier1: Vec<_> = outcomes.iter().filter(|o| o.tier == Tier::One).collect();
    assert_eq!(tier1.len(), 4, "expect 4 tier-1 cases");
    for outcome in &tier1 {
        assert!(matches!(outcome.status, CaseStatus::Ok));
    }
}
```

- [ ] **Step 4: Run the new tests to verify they fail for the right reason**

Run: `cargo test -p cairn-core frontend_adapter_contract -- --nocapture`

Expected: FAIL with unresolved frontend types or missing trait surface members.

Run: `cargo test -p cairn-core conformance_tier1 -- --nocapture`

Expected: FAIL because `FrontendAdapter` has no conformance runner and the new stub cannot implement the richer contract.

- [ ] **Step 5: Commit the red tests**

```bash
git add crates/cairn-core/tests/frontend_adapter_contract.rs \
  crates/cairn-core/tests/contract_root_exports.rs \
  crates/cairn-core/tests/conformance_tier1.rs
git commit -m "test: add failing frontend adapter contract coverage"
```

## Task 2: Implement the Rich Frontend Contract Surface

**Files:**
- Modify: `crates/cairn-core/src/contract/frontend_adapter.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs`

- [ ] **Step 1: Add the failing registry stub compile case**

Update the existing frontend stub in `crates/cairn-core/tests/contract_registry.rs` to implement the new required methods with `unimplemented!()` placeholders so the build fails on missing core types first:

```rust
fn project(
    &self,
    _request: &FrontendProjectionRequest,
) -> Result<FrontendProjection, FrontendAdapterError> {
    unimplemented!("filled after contract types land")
}

fn reconcile(
    &self,
    _ctx: FrontendIdentityContext,
    _edit: FrontendEdit,
) -> Result<FrontendReconcileRequest, FrontendAdapterError> {
    unimplemented!("filled after contract types land")
}
```

- [ ] **Step 2: Run the targeted registry test to verify compile failure**

Run: `cargo test -p cairn-core contract_registry -- --nocapture`

Expected: FAIL at compile time because the frontend contract types and methods do not exist yet.

- [ ] **Step 3: Implement the minimal frontend contract types**

In `crates/cairn-core/src/contract/frontend_adapter.rs`, add:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrontendAdapterCapabilities {
    pub frontmatter: bool,
    pub sidecar_files: bool,
    pub live_plugin: bool,
    pub graph_view: bool,
    pub max_frontmatter_fields: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrontendFieldClass {
    UserContent,
    Metadata,
    Classification,
    IdentityProvenance,
    VisibilityConsent,
    VersionAudit,
}

pub struct FrontendFieldPolicy;

impl FrontendFieldPolicy {
    pub fn classify(field_name: &str) -> FrontendFieldClass { /* match fields */ }
    pub fn is_mutable_from_frontend(field_name: &str) -> bool { /* bool match */ }
}
```

Also add narrow request/response types:

```rust
pub struct FrontendProjectionRequest {
    pub target_id: String,
    pub expected_version: u64,
}

pub struct FrontendProjection {
    pub body: String,
    pub frontmatter: Vec<(String, String)>,
    pub sidecars: Vec<(String, String)>,
    pub target_hash: String,
}

pub struct FrontendIdentityContext {
    pub principal: String,
    pub agent: Option<String>,
    pub signed_intent_present: bool,
}

pub struct FrontendEdit {
    pub target_id: String,
    pub expected_version: u64,
    pub target_hash: String,
    pub changed_fields: Vec<String>,
}

pub struct FrontendReconcileRequest {
    pub target_id: String,
    pub expected_version: u64,
    pub target_hash: String,
    pub mutable_fields: Vec<String>,
    pub ctx: FrontendIdentityContext,
}
```

Add typed errors:

```rust
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum FrontendReconcileError {
    #[error("frontend reconcile conflict at version {current_version}")]
    Conflict { current_version: u64 },
    #[error("frontend reconcile requires a signed intent")]
    UnsignedIntent,
    #[error("frontend reconcile detected a replayed operation")]
    ReplayDetected,
    #[error("frontend reconcile attempted to change immutable field {field}")]
    ImmutableFieldChanged { field: String },
    #[error("frontend reconcile policy denied: {gate}: {reason}")]
    PolicyDenied { gate: String, reason: String },
    #[error("frontend reconcile requires capability {required}")]
    InsufficientCapability { required: String },
}
```

Wrap projection/reconcile failures:

```rust
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum FrontendAdapterError {
    #[error(transparent)]
    Reconcile(#[from] FrontendReconcileError),
    #[error("frontend projection failed: {message}")]
    Projection { message: String },
}
```

Extend the trait:

```rust
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
```

- [ ] **Step 4: Re-export the new frontend types**

In `crates/cairn-core/src/contract/mod.rs`, extend the `pub use frontend_adapter::{ ... }` line so it exports the new types:

```rust
pub use frontend_adapter::{
    FrontendAdapter, FrontendAdapterCapabilities, FrontendAdapterError,
    FrontendAdapterPlugin, FrontendEdit, FrontendFieldClass, FrontendFieldPolicy,
    FrontendIdentityContext, FrontendProjection, FrontendProjectionRequest,
    FrontendReconcileError, FrontendReconcileRequest, FrontendSubscription,
    FrontendEventStream,
};
```

- [ ] **Step 5: Run the focused tests to verify they pass**

Run: `cargo test -p cairn-core frontend_adapter_contract -- --nocapture`

Expected: PASS

Run: `cargo test -p cairn-core contract_root_exports -- --nocapture`

Expected: PASS

- [ ] **Step 6: Commit the contract surface**

```bash
git add crates/cairn-core/src/contract/frontend_adapter.rs \
  crates/cairn-core/src/contract/mod.rs \
  crates/cairn-core/tests/contract_root_exports.rs \
  crates/cairn-core/tests/frontend_adapter_contract.rs \
  crates/cairn-core/tests/contract_registry.rs
git commit -m "feat: expand frontend adapter contract surface"
```

## Task 3: Implement the Frontend Conformance Runner

**Files:**
- Create: `crates/cairn-core/src/contract/conformance/frontend_adapter.rs`
- Modify: `crates/cairn-core/src/contract/conformance/mod.rs`
- Modify: `crates/cairn-core/tests/conformance_tier1.rs`

- [ ] **Step 1: Write the failing frontend conformance assertions**

In `crates/cairn-core/tests/frontend_adapter_contract.rs`, add a targeted runner assertion:

```rust
#[test]
fn frontend_conformance_runner_reports_tier2_cases() {
    let mut reg = PluginRegistry::new();
    let name = register_stub_frontend(&mut reg);
    let outcomes = run_conformance_for_plugin(&reg, &name);

    let ids: Vec<_> = outcomes.iter().map(|o| o.id).collect();
    assert!(ids.contains(&"rejects_immutable_field_edits"));
    assert!(ids.contains(&"rejects_replayed_operation"));
    assert!(ids.contains(&"rejects_tampered_target_hash"));
    assert!(ids.contains(&"rejects_unrecognized_principal"));
    assert!(ids.contains(&"honors_optimistic_version_check"));
}
```

- [ ] **Step 2: Run the conformance tests to verify they fail**

Run: `cargo test -p cairn-core frontend_conformance_runner_reports_tier2_cases -- --nocapture`

Expected: FAIL because `ContractKind::FrontendAdapter` still routes to `no_conformance_runner`.

- [ ] **Step 3: Implement the frontend conformance module**

Create `crates/cairn-core/src/contract/conformance/frontend_adapter.rs` with a runner shaped like the existing modules:

```rust
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let adapter = match registry.frontend_adapter(name) {
        Some(adapter) => adapter,
        None => return Vec::new(),
    };

    let caps = adapter.capabilities();
    let mut out = vec![
        tier1_manifest_matches_host(registry, name, CONTRACT_VERSION),
        tier1_arc_pointer_stable_frontend(registry, name),
        tier1_capability_self_consistency_floor_frontend(caps),
        tier1_manifest_features_match_capabilities(
            registry,
            name,
            &[
                ("frontmatter", caps.frontmatter),
                ("sidecar_files", caps.sidecar_files),
                ("live_plugin", caps.live_plugin),
                ("graph_view", caps.graph_view),
            ],
        ),
    ];

    out.extend([
        tier2_rejects_immutable_field_edits(adapter.as_ref()),
        tier2_rejects_replayed_operation(adapter.as_ref()),
        tier2_rejects_tampered_target_hash(adapter.as_ref()),
        tier2_rejects_unrecognized_principal(adapter.as_ref()),
        tier2_honors_optimistic_version_check(adapter.as_ref()),
    ]);
    out
}
```

Keep tier-2 cases pure by driving `adapter.reconcile(...)` with synthetic inputs and matching on `FrontendReconcileError`.

- [ ] **Step 4: Route `ContractKind::FrontendAdapter` to the new runner**

In `crates/cairn-core/src/contract/conformance/mod.rs`:

```rust
pub mod frontend_adapter;
```

and in `run_conformance_for_plugin(...)`:

```rust
ContractKind::FrontendAdapter => frontend_adapter::run(registry, name),
```

Remove `FrontendAdapter` from the fallback `no_conformance_runner` branch.

- [ ] **Step 5: Run focused conformance tests to verify they pass**

Run: `cargo test -p cairn-core conformance_tier1 -- --nocapture`

Expected: PASS

Run: `cargo test -p cairn-core frontend_adapter_contract -- --nocapture`

Expected: PASS, including the frontend tier-2 id checks.

- [ ] **Step 6: Commit the conformance runner**

```bash
git add crates/cairn-core/src/contract/conformance/mod.rs \
  crates/cairn-core/src/contract/conformance/frontend_adapter.rs \
  crates/cairn-core/tests/conformance_tier1.rs \
  crates/cairn-core/tests/frontend_adapter_contract.rs
git commit -m "feat: add frontend adapter conformance runner"
```

## Task 4: Update Registry Stubs and Final Verification

**Files:**
- Modify: `crates/cairn-core/tests/contract_registry.rs`
- Modify: `docs/design/traceability.md` (only if needed)

- [ ] **Step 1: Implement the frontend stub plugin minimally**

Replace the temporary `unimplemented!()` calls in `crates/cairn-core/tests/contract_registry.rs` with minimal green implementations:

```rust
fn project(
    &self,
    request: &FrontendProjectionRequest,
) -> Result<FrontendProjection, FrontendAdapterError> {
    Ok(FrontendProjection {
        body: String::new(),
        frontmatter: Vec::new(),
        sidecars: Vec::new(),
        target_hash: request.target_id.clone(),
    })
}

fn reconcile(
    &self,
    ctx: FrontendIdentityContext,
    edit: FrontendEdit,
) -> Result<FrontendReconcileRequest, FrontendAdapterError> {
    if !ctx.signed_intent_present {
        return Err(FrontendReconcileError::UnsignedIntent.into());
    }
    for field in &edit.changed_fields {
        if !FrontendFieldPolicy::is_mutable_from_frontend(field) {
            return Err(FrontendReconcileError::ImmutableFieldChanged {
                field: field.clone(),
            }
            .into());
        }
    }
    Ok(FrontendReconcileRequest {
        target_id: edit.target_id,
        expected_version: edit.expected_version,
        target_hash: edit.target_hash,
        mutable_fields: edit.changed_fields,
        ctx,
    })
}
```

- [ ] **Step 2: Run registry and compile-path tests**

Run: `cargo test -p cairn-core contract_registry -- --nocapture`

Expected: PASS

Run: `cargo test -p cairn-core contract_root_exports -- --nocapture`

Expected: PASS

- [ ] **Step 3: Decide whether traceability needs an update**

Inspect `docs/design/traceability.md`. If the coverage note for §4 / §12 / §13 already accurately says `#113` covers the contract and conformance slice, leave it unchanged. If the implementation adds meaningful specificity, update the note in the same commit.

- [ ] **Step 4: Run final issue verification**

Run:

```bash
cargo test -p cairn-core frontend_adapter_contract -- --nocapture
cargo test -p cairn-core conformance_tier1 -- --nocapture
cargo test -p cairn-core contract_registry -- --nocapture
cargo test -p cairn-core contract_root_exports -- --nocapture
cargo check -p cairn-core --locked
```

Expected: all PASS.

- [ ] **Step 5: Commit the final polish**

```bash
git add crates/cairn-core/tests/contract_registry.rs docs/design/traceability.md
git commit -m "test: finish frontend adapter contract verification"
```

## Self-Review

- Spec coverage:
  - contract surface: Task 2
  - mutability policy: Tasks 1 and 2
  - conformance runner: Task 3
  - registry/compile-path updates: Tasks 2 and 4
  - verification: Task 4
- Placeholder scan:
  - No `TODO` or `TBD` markers remain in the execution steps.
- Type consistency:
  - The same frontend type names are used across contract, conformance, and test tasks.
