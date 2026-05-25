# Connector Framework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the v0.3 external-connector substrate (`cairn-connectors-core`) — trait, payload envelope, OAuth+webhook contracts, pre-Capture redaction, registry lifecycle, consent gates — without any real adapter, per issue [#130](https://github.com/windoliver/cairn/issues/130).

**Architecture:** New L2 crate sits between adapter crates (#131/#181) and `cairn-core`. Connectors emit `ConnectorEvent`s; the framework validates + redacts + consent-gates + rate-limits, then constructs a `CaptureEvent` with `source_family: External` and hands it to the existing `cairn-core::pipeline` through a `PipelineEmit` trait. Registry owns the per-connector poll task (tokio) and webhook routes (axum). One in-tree `FixtureConnector` drives all contract/payload/disabled/consent tests.

**Tech Stack:** Rust 1.95, `async-trait`, `axum` 0.8, `tokio`, `tokio-util` (`CancellationToken`), `serde`, `toml`, `thiserror`, `tracing`, `arc-swap`, `proptest`, `insta`, `rstest`, `cairn-keychain` (default `CredentialStore` impl), `cairn-test-fixtures` (dev-dep).

**Spec:** [`docs/superpowers/specs/2026-05-24-issue-130-connector-framework-design.md`](../specs/2026-05-24-issue-130-connector-framework-design.md)

---

## File map

Files created or modified by this plan, in execution order:

| Touched in task | Path | Responsibility |
|---|---|---|
| T1 | `crates/cairn-core/src/domain/capture.rs` (modify) | Add `SourceFamily::External` + `CapturePayload::External` variants |
| T1 | `crates/cairn-core/tests/capture_external_payload.rs` (create) | Round-trip + validator coverage of the new variant |
| T2 | `crates/cairn-core/src/contract/manifest.rs` (modify) | Add `ContractKind::Connector` row |
| T2 | `crates/cairn-core/tests/manifest_connector_kind.rs` (create) | Manifest parse coverage for the new contract kind |
| T3 | `crates/cairn-core/src/contract/connector_consent.rs` (create) | `ConnectorConsentJournal` trait (read + write) |
| T3 | `crates/cairn-core/src/contract/mod.rs` (modify) | Re-export new trait + `ConsentGrant` |
| T4 | `Cargo.toml` (modify) | Add `cairn-connectors-core` workspace member |
| T4 | `crates/cairn-connectors-core/Cargo.toml` (create) | Crate manifest, deps, features |
| T4 | `crates/cairn-connectors-core/src/lib.rs` (create) | Module declarations + re-exports |
| T4 | `crates/cairn-connectors-core/tests/smoke.rs` (create) | Compile-only smoke test |
| T5 | `crates/cairn-connectors-core/src/error.rs` (create) | `ConnectorError` enum |
| T6 | `crates/cairn-connectors-core/src/event.rs` (create) | `ConnectorEvent`, `ConnectorPayload`, `ConnectorScope`, `SourceRef`, `DeliveryMode`, `ConnectorEventId` |
| T7 | `crates/cairn-connectors-core/src/manifest.rs` (create) | TOML parser + invariant validator |
| T8 | `crates/cairn-connectors-core/src/connector.rs` (create) | `Connector` trait, `ConnectorCapabilities`, `ConnectorPlugin`, `PollOutcome`, contexts |
| T9 | `crates/cairn-connectors-core/src/credential.rs` (create) | `CredentialStore` trait + `InMemoryCredentialStore` |
| T10 | `crates/cairn-connectors-core/src/credential_keychain.rs` (create) | `KeychainCredentialStore` (default backend) |
| T11 | `crates/cairn-connectors-core/src/redact.rs` (create) | `RedactionPipeline` (JSON walker + binary spool) |
| T12 | `crates/cairn-connectors-core/src/rate_limit.rs` (create) | Per-scope token bucket |
| T13 | `crates/cairn-connectors-core/src/webhook.rs` (create) | `WebhookRouter`, `WebhookRequest`, HMAC verifier |
| T14 | `crates/cairn-connectors-core/src/poll.rs` (create) | `PollScheduler` (tokio task + cursor mgmt) |
| T15 | `crates/cairn-connectors-core/src/emit.rs` (create) | `PipelineEmit` trait + `ConnectorEvent → CaptureEvent` builder |
| T16 | `crates/cairn-connectors-core/src/registry.rs` (create) | `ConnectorRegistry` lifecycle |
| T17 | `crates/cairn-connectors-core/src/fixture.rs` (create) | `FixtureConnector` (cfg test / `feature="fixture"`) |
| T18 | `crates/cairn-connectors-core/tests/contract.rs` (create) | Happy-path register→enable→poll→emit |
| T19 | `crates/cairn-connectors-core/tests/payload_validation.rs` (create) | Proptest: no raw byte leaks through redaction |
| T20a | `crates/cairn-connectors-core/tests/undeclared_label_rejected.rs` (create) | `UndeclaredLabel` gate |
| T20b | `crates/cairn-connectors-core/tests/consent_gate.rs` (create) | `ConsentRevoked` lifecycle |
| T20c | `crates/cairn-connectors-core/tests/disabled_no_emit.rs` (create) | Disabled connectors silent |
| T20d | `crates/cairn-connectors-core/tests/rate_limit.rs` (create) | Budget exceeded → typed error |
| T20e | `crates/cairn-connectors-core/tests/redaction.rs` (create) | PII spans cover original byte ranges |
| T20f | `crates/cairn-connectors-core/tests/oauth_lifecycle.rs` (create) | Token refresh + signature mismatch + replay |
| T20g | `crates/cairn-connectors-core/tests/manifest_validates.rs` (create) | Manifest parser cases + insta snapshot |
| T21 | repo-wide | Verification checklist + docgen |

---

## Task 0 — Workspace readiness

**Files:** none

- [ ] **Step 1: Confirm worktree + clean tree**

Run: `git status --short && pwd`
Expected: clean tree under `/Users/tafeng/cairn/.claude/worktrees/drifting-herding-token` (already an isolated worktree).

- [ ] **Step 2: Verify baseline compiles before any edit**

Run: `cargo check --workspace --locked`
Expected: PASS (no errors).

---

## Task 1 — `SourceFamily::External` + `CapturePayload::External`

**Files:**
- Modify: `crates/cairn-core/src/domain/capture.rs`
- Create: `crates/cairn-core/tests/capture_external_payload.rs`

- [ ] **Step 1: Write the failing test**

`crates/cairn-core/tests/capture_external_payload.rs`:

```rust
//! Coverage for the `External` source family + `CapturePayload::External`
//! variant added for the v0.3 connector framework (issue #130).

use std::collections::BTreeSet;

use cairn_core::domain::capture::{
    CapturePayload, CaptureEvent, SourceFamily, SourceRef,
};
use cairn_core::pipeline::filter::redact::{RedactionSpan, RedactionTag};

#[test]
fn external_payload_reports_external_family() {
    let payload = CapturePayload::External {
        connector: "fixture".into(),
        source_ref: SourceRef::new("issue", "gh:owner/repo#42", None),
        labels: BTreeSet::from(["note".to_string()]),
        mime: "application/json".into(),
        redacted_spans: vec![RedactionSpan {
            start: 0,
            end: 10,
            tag: RedactionTag::Email,
        }],
    };
    assert_eq!(payload.source_family(), SourceFamily::External);
}

#[test]
fn external_payload_round_trips_through_serde() {
    let payload = CapturePayload::External {
        connector: "fixture".into(),
        source_ref: SourceRef::new("issue", "gh:owner/repo#42", None),
        labels: BTreeSet::from(["note".to_string(), "comment".into()]),
        mime: "application/json".into(),
        redacted_spans: vec![],
    };
    let json = serde_json::to_string(&payload).expect("serialize");
    let back: CapturePayload = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(payload, back);
}

#[test]
fn external_source_family_serializes_as_external_string() {
    let serialized = serde_json::to_string(&SourceFamily::External).expect("serialize");
    assert_eq!(serialized, "\"external\"");
}
```

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cargo nextest run -p cairn-core --test capture_external_payload --locked`
Expected: FAIL with `no variant or associated item named 'External'`.

- [ ] **Step 3: Add the `External` variant to `SourceFamily`**

In `crates/cairn-core/src/domain/capture.rs`, find the `SourceFamily` enum (~line 291) and append after the existing `Proactive` variant:

```rust
    /// External source connector (brief §9.1 source sensors / §19 v0.3).
    External,
```

Update `SourceFamily::as_str`, `Display`, the `FromStr`/`try_from_str` path, and any exhaustive `match` (look for `SourceFamily::Proactive => …`) to include:

```rust
    SourceFamily::External => "external",
```

- [ ] **Step 4: Add the `External` payload variant**

In the same file, find the `CapturePayload` enum and add the variant:

```rust
    /// Payload emitted by an external connector (brief §9.1, §19 v0.3).
    /// Bytes have already been redacted before this value exists; the
    /// `redacted_spans` field records what was removed.
    #[serde(rename = "external")]
    External {
        connector: String,
        source_ref: SourceRef,
        labels: std::collections::BTreeSet<String>,
        mime: String,
        redacted_spans: Vec<crate::pipeline::filter::redact::RedactionSpan>,
    },
```

In `CapturePayload::source_family()` (~line 589) add the arm:

```rust
    Self::External { .. } => SourceFamily::External,
```

In the `Debug` impl for `CapturePayload`, add an arm that prints structural metadata only (connector, mime, label count, redacted span count) — no payload body.

If `SourceRef` does not yet exist in `domain::capture`, add it next to the other small newtypes:

```rust
/// Stable upstream-system reference (brief §9.1 source sensors).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub kind: String,        // "issue", "pr", "message", "page", "file"
    pub system_id: String,   // upstream stable id
    pub sub_id: Option<String>,
}

impl SourceRef {
    pub fn new(kind: impl Into<String>, system_id: impl Into<String>, sub_id: Option<String>) -> Self {
        Self { kind: kind.into(), system_id: system_id.into(), sub_id }
    }
}

impl std::fmt::Debug for SourceRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceRef")
            .field("kind", &self.kind)
            .field("system_id", &self.system_id)
            .field("sub_id", &self.sub_id)
            .finish()
    }
}
```

- [ ] **Step 5: Re-run the test**

Run: `cargo nextest run -p cairn-core --test capture_external_payload --locked`
Expected: PASS (3 tests).

- [ ] **Step 6: Workspace check**

Run: `cargo check --workspace --locked && cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-core/src/domain/capture.rs crates/cairn-core/tests/capture_external_payload.rs
git commit -m "feat(core): add External source family and CapturePayload variant (#130)"
```

---

## Task 2 — `ContractKind::Connector`

**Files:**
- Modify: `crates/cairn-core/src/contract/manifest.rs`
- Create: `crates/cairn-core/tests/manifest_connector_kind.rs`

- [ ] **Step 1: Write the failing test**

`crates/cairn-core/tests/manifest_connector_kind.rs`:

```rust
use cairn_core::contract::manifest::{ContractKind, PluginManifest};

const FIXTURE: &str = r#"
name = "fixture"
contract = "Connector"
contract_version_range = ">=0.1.0, <0.2.0"
"#;

#[test]
fn connector_kind_parses_from_manifest() {
    let manifest = PluginManifest::parse_toml(FIXTURE).expect("parse");
    assert_eq!(manifest.contract(), ContractKind::Connector);
}

#[test]
fn connector_kind_has_static_str() {
    assert_eq!(ContractKind::Connector.as_static_str(), "Connector");
}
```

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cargo nextest run -p cairn-core --test manifest_connector_kind --locked`
Expected: FAIL — `Connector` variant missing.

- [ ] **Step 3: Add the variant**

In `crates/cairn-core/src/contract/manifest.rs`, in the `ContractKind` enum (~line 21), add after `AgentProvider`:

```rust
    /// Implements `Connector` contract (brief §9.1 source sensors,
    /// §19 v0.3). See `cairn-connectors-core`.
    Connector,
```

In `ContractKind::as_static_str` add:

```rust
    ContractKind::Connector => "Connector",
```

- [ ] **Step 4: Re-run the test**

Run: `cargo nextest run -p cairn-core --test manifest_connector_kind --locked`
Expected: PASS (2 tests).

- [ ] **Step 5: Workspace check + clippy**

Run: `cargo nextest run -p cairn-core --locked && cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS. If `manifest::serde` derives an exhaustive enum mapping, ensure no test snapshots break — update existing `.snap` files with `cargo insta review` if needed and commit the snap changes alongside.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/contract/manifest.rs crates/cairn-core/tests/manifest_connector_kind.rs
# also add any updated insta snapshots if review changed them
git commit -m "feat(core): add Connector contract kind to PluginManifest (#130)"
```

---

## Task 3 — `ConnectorConsentJournal` trait

**Files:**
- Create: `crates/cairn-core/src/contract/connector_consent.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs`

- [ ] **Step 1: Write the failing test**

Append to a new module at the bottom of `crates/cairn-core/src/contract/connector_consent.rs` (we'll write it in step 3). For now create a failing integration test:

`crates/cairn-core/tests/connector_consent_trait.rs`:

```rust
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Mutex;

use cairn_core::contract::connector_consent::{
    ConnectorConsentJournal, ConsentGrant, ConsentGrantId, ConsentLookup,
};
use cairn_core::domain::Identity;

#[derive(Default)]
struct StubJournal {
    grants: Mutex<Vec<ConsentGrant>>,
}

#[async_trait::async_trait]
impl ConnectorConsentJournal for StubJournal {
    async fn put_grant(&self, grant: ConsentGrant) -> Result<ConsentGrantId, String> {
        let id = ConsentGrantId::new(format!("gnt:{}", grant.connector));
        self.grants.lock().unwrap().push(grant);
        Ok(id)
    }
    async fn lookup(&self, connector: &str, _scope_key: &str) -> Result<ConsentLookup, String> {
        let g = self.grants.lock().unwrap();
        Ok(if g.iter().any(|g| g.connector == connector) {
            ConsentLookup::Granted
        } else {
            ConsentLookup::Revoked
        })
    }
    async fn revoke(&self, _id: &ConsentGrantId) -> Result<(), String> {
        self.grants.lock().unwrap().clear();
        Ok(())
    }
}

#[tokio::test]
async fn grant_then_lookup_then_revoke() {
    let journal: Arc<dyn ConnectorConsentJournal> = Arc::new(StubJournal::default());
    let grant = ConsentGrant {
        connector: "fixture".into(),
        manifest_hash: "h1".into(),
        allowed_labels: BTreeSet::from(["note".to_string()]),
        scope_patterns: vec!["project:*".into()],
        granted_at: 0,
        grantor: Identity::test_user("alice"),
    };
    let id = journal.put_grant(grant).await.unwrap();
    assert_eq!(journal.lookup("fixture", "project:any").await.unwrap(), ConsentLookup::Granted);
    journal.revoke(&id).await.unwrap();
    assert_eq!(journal.lookup("fixture", "project:any").await.unwrap(), ConsentLookup::Revoked);
}
```

> If `Identity::test_user` does not exist, look for the test-only constructor pattern already used in `crates/cairn-core/src/domain/identity.rs` and use the actual helper name. Adjust the test accordingly.

- [ ] **Step 2: Run the test and confirm it fails**

Run: `cargo nextest run -p cairn-core --test connector_consent_trait --locked`
Expected: FAIL — module does not exist.

- [ ] **Step 3: Add the trait module**

`crates/cairn-core/src/contract/connector_consent.rs`:

```rust
//! `ConnectorConsentJournal` — write + lookup access to the
//! `connector_consent` portion of the consent journal. The framework
//! in `cairn-connectors-core` calls this on every emit; persistent
//! impls land alongside `cairn-store-sqlite` work in a follow-up.
//!
//! Issue #130, brief §14 + §19 v0.3.

use std::collections::BTreeSet;

use crate::domain::Identity;

/// Opaque consent-grant identifier. Stable across process restarts;
/// formatted as `gnt:<connector>:<ulid>` by the persistent impl.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConsentGrantId(String);

impl ConsentGrantId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self { Self(value.into()) }
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

/// What `lookup` returns. `Revoked` is the closed-fail default: if
/// the journal has no live grant for `(connector, scope_key)` the
/// framework rejects the emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentLookup {
    Granted,
    Revoked,
}

/// Persistent grant record (brief §14). The framework writes this
/// when `ConnectorRegistry::enable` runs; the consent journal stores
/// it; `lookup` resolves against it on every emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentGrant {
    pub connector: String,
    pub manifest_hash: String,
    pub allowed_labels: BTreeSet<String>,
    pub scope_patterns: Vec<String>,
    pub granted_at: i64,           // unix seconds
    pub grantor: Identity,
}

/// Write + lookup surface needed by the connector framework. Read-only
/// `ConsentJournalReader` (issue #257) stays distinct because it is
/// forget-scoped and consumed by `lint`; consolidating later is
/// possible but not in scope for #130.
#[async_trait::async_trait]
pub trait ConnectorConsentJournal: Send + Sync {
    async fn put_grant(&self, grant: ConsentGrant) -> Result<ConsentGrantId, String>;
    async fn lookup(&self, connector: &str, scope_key: &str) -> Result<ConsentLookup, String>;
    async fn revoke(&self, id: &ConsentGrantId) -> Result<(), String>;
}
```

In `crates/cairn-core/src/contract/mod.rs` add:

```rust
pub mod connector_consent;
pub use connector_consent::{ConnectorConsentJournal, ConsentGrant, ConsentGrantId, ConsentLookup};
```

If `cairn-core` does not already depend on `async-trait` in its non-dev deps, add it to `crates/cairn-core/Cargo.toml`:

```toml
async-trait = { workspace = true }
```

- [ ] **Step 4: Re-run the test**

Run: `cargo nextest run -p cairn-core --test connector_consent_trait --locked`
Expected: PASS.

- [ ] **Step 5: Workspace clippy**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-core/src/contract/connector_consent.rs crates/cairn-core/src/contract/mod.rs crates/cairn-core/tests/connector_consent_trait.rs crates/cairn-core/Cargo.toml
git commit -m "feat(core): add ConnectorConsentJournal contract trait (#130)"
```

---

## Task 4 — Scaffold `cairn-connectors-core` crate

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/cairn-connectors-core/Cargo.toml`
- Create: `crates/cairn-connectors-core/src/lib.rs`
- Create: `crates/cairn-connectors-core/tests/smoke.rs`

- [ ] **Step 1: Add workspace member**

The workspace already uses `members = ["crates/*"]`, so creating the crate dir is sufficient. No edit needed to `Cargo.toml` unless `exclude` list grows — confirm with:

Run: `grep -n 'members\|exclude' Cargo.toml | head`
Expected: `members = ["crates/*"]`, no exclusion for the new crate.

- [ ] **Step 2: Create `crates/cairn-connectors-core/Cargo.toml`**

```toml
[package]
name = "cairn-connectors-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
homepage.workspace = true
description = "External-connector substrate: trait, OAuth/webhook payload contracts, redaction, registry."

[lints]
workspace = true

[features]
default = []
## Compile FixtureConnector for downstream test crates.
fixture = []

[dependencies]
cairn-core = { workspace = true }
cairn-keychain = { workspace = true }
arc-swap = "1"
async-trait = { workspace = true }
axum = { workspace = true }
bon = { workspace = true }
hmac = { version = "0.12", default-features = false }
sha2 = { workspace = true }
subtle = { version = "2", default-features = false }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true, features = ["rt", "rt-multi-thread", "macros", "sync", "time"] }
tokio-util = { workspace = true, features = ["rt"] }
toml = { workspace = true }
tracing = { workspace = true }
ulid = { workspace = true }
hex = { version = "0.4", default-features = false, features = ["alloc"] }

[dev-dependencies]
cairn-test-fixtures = { workspace = true }
insta = { workspace = true }
proptest = { workspace = true }
rstest = { workspace = true }
serde_json = { workspace = true }
tempfile = { workspace = true }
tokio = { workspace = true, features = ["test-util"] }
```

If `hmac`, `subtle`, `arc-swap`, or `hex` are not in `[workspace.dependencies]`, add them at the workspace level instead and reference via `{ workspace = true }`. Prefer the workspace path — the convention from CLAUDE.md §6.7.

- [ ] **Step 3: Create `src/lib.rs`**

```rust
//! Cairn connector framework — substrate that lets adapter crates
//! emit `CaptureEvent`s through the cairn-core pipeline behind a
//! validated, redacted, consent-gated boundary.
//!
//! Issue #130, brief §9.1 source sensors, §19 v0.3.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod connector;
pub mod credential;
pub mod credential_keychain;
pub mod emit;
pub mod error;
pub mod event;
pub mod manifest;
pub mod poll;
pub mod rate_limit;
pub mod redact;
pub mod registry;
pub mod webhook;

#[cfg(any(test, feature = "fixture"))]
pub mod fixture;

pub use connector::{Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext, CONTRACT_VERSION};
pub use credential::{CredentialHandle, CredentialStore, InMemoryCredentialStore};
pub use emit::PipelineEmit;
pub use error::ConnectorError;
pub use event::{ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef};
pub use manifest::ConnectorManifest;
pub use rate_limit::RateLimit;
pub use redact::RedactionPipeline;
pub use registry::ConnectorRegistry;
pub use webhook::{WebhookRequest, WebhookRouter};
```

Every module file will start as an empty `pub` stub returning `compile_error!` so this file compiles only after each task lands. To avoid that, comment-out unused `pub use` lines and add them back in later tasks. Simplest: stub each module file with `//! WIP` and a single `pub fn _stub() {}` so the re-exports stay aligned. Start with:

```rust
//! WIP — replaced in task NN.
```

Apply the `WIP` stub to every module file referenced above so this task compiles.

- [ ] **Step 4: Write smoke test**

`crates/cairn-connectors-core/tests/smoke.rs`:

```rust
#[test]
fn crate_compiles() {
    let _v = cairn_connectors_core::CONTRACT_VERSION;
}
```

Comment the body out until T8 lands the real `CONTRACT_VERSION`. For now write:

```rust
#[test]
fn crate_compiles() {}
```

- [ ] **Step 5: Build + smoke test**

Run: `cargo nextest run -p cairn-connectors-core --locked`
Expected: PASS (1 test). Also: `cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings` PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-connectors-core
git commit -m "scaffold(connectors-core): empty crate with module stubs (#130)"
```

---

## Task 5 — `ConnectorError` enum

**Files:**
- Modify: `crates/cairn-connectors-core/src/error.rs`

- [ ] **Step 1: Write the failing test**

Append at the bottom of `error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn display_strings_are_stable() {
        assert_eq!(
            ConnectorError::RateLimited { retry_after: Duration::from_secs(7) }.to_string(),
            "rate limited; retry after 7s",
        );
        assert_eq!(
            ConnectorError::UndeclaredLabel { label: "secret".into() }.to_string(),
            "undeclared label secret",
        );
        assert_eq!(
            ConnectorError::ConsentRevoked { connector: "fixture".into() }.to_string(),
            "consent revoked for fixture",
        );
    }

    #[test]
    fn dyn_compat() {
        let e: Box<dyn std::error::Error + Send + Sync> =
            Box::new(ConnectorError::SignatureMismatch);
        assert!(e.to_string().contains("signature"));
    }
}
```

- [ ] **Step 2: Run + confirm fail**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL — no such enum.

- [ ] **Step 3: Implement the enum**

Replace the WIP stub with:

```rust
//! `ConnectorError` — the closed error surface every framework gate
//! returns. Adapters may wrap their own errors as `Transient` or
//! `Fatal`; everything else is named so the registry can act on it
//! without string-matching.

use std::time::Duration;

/// Errors emitted by `cairn-connectors-core`. `#[non_exhaustive]` so we
/// can add variants without a breaking change.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectorError {
    /// OAuth credentials for the named scope have expired.
    #[error("auth expired for {scope}")]
    AuthExpired { scope: String },

    /// Upstream returned a rate-limit response; retry after the hint.
    #[error("rate limited; retry after {}s", retry_after.as_secs())]
    RateLimited { retry_after: Duration },

    /// Webhook signature failed verification.
    #[error("signature mismatch")]
    SignatureMismatch,

    /// Payload did not match the manifest's structural rules.
    #[error("malformed payload: {0}")]
    MalformedPayload(String),

    /// Per-scope budget from the manifest is exhausted.
    #[error("budget exceeded for {scope}")]
    BudgetExceeded { scope: String },

    /// Connector tried to emit a label outside its manifest's allow-list.
    #[error("undeclared label {label}")]
    UndeclaredLabel { label: String },

    /// No live consent grant for `(connector, scope)`.
    #[error("consent revoked for {connector}")]
    ConsentRevoked { connector: String },

    /// Recoverable upstream / transport error.
    #[error(transparent)]
    Transient(#[source] anyhow::Error),

    /// Non-recoverable error; surfaces in `lint`.
    #[error(transparent)]
    Fatal(#[source] anyhow::Error),
}
```

Add `anyhow = { workspace = true }` to `[dependencies]` of `crates/cairn-connectors-core/Cargo.toml` (yes, this is a substrate crate — but anyhow appears only inside `#[source]` payloads for adapter-owned errors).

- [ ] **Step 4: Re-run tests**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked && cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/error.rs crates/cairn-connectors-core/Cargo.toml
git commit -m "feat(connectors-core): ConnectorError enum (#130)"
```

---

## Task 6 — `ConnectorEvent` + `ConnectorPayload` + envelope types

**Files:**
- Modify: `crates/cairn-connectors-core/src/event.rs`

- [ ] **Step 1: Write the failing test**

Append to `event.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn json_payload_round_trips() {
        let event = ConnectorEvent {
            event_id: ConnectorEventId::new("01HX0000000000000000000000"),
            connector: "fixture".into(),
            source_ref: SourceRef::new("issue", "gh:owner/repo#42", None),
            occurred_at: 1_700_000_000,
            labels: BTreeSet::from(["note".to_string()]),
            scope: ConnectorScope::project("owner/repo"),
            payload: ConnectorPayload::Json {
                mime: "application/json".into(),
                body: serde_json::json!({"text": "hello"}),
            },
            delivery: DeliveryMode::Poll { cursor: Some("c1".into()) },
        };
        let s = serde_json::to_string(&event).unwrap();
        let back: ConnectorEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(event.connector, back.connector);
        assert_eq!(event.labels, back.labels);
    }

    #[test]
    fn rejects_unknown_fields() {
        let bogus = r#"{
          "event_id":"01HX0000000000000000000000","connector":"f","source_ref":{"kind":"x","system_id":"y","sub_id":null},
          "occurred_at":0,"labels":[],"scope":{"kind":"project","value":"x"},
          "payload":{"kind":"text","mime":"text/plain","body":""},
          "delivery":{"kind":"poll","cursor":null},
          "extra":true
        }"#;
        assert!(serde_json::from_str::<ConnectorEvent>(bogus).is_err());
    }
}
```

- [ ] **Step 2: Run + confirm fail**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL.

- [ ] **Step 3: Implement the envelope**

Replace `event.rs`:

```rust
//! `ConnectorEvent` envelope and supporting types (issue #130, brief §9.1).
//!
//! Connectors emit `ConnectorEvent`s; the framework converts them into
//! `CaptureEvent`s after validation + redaction + consent. The two are
//! intentionally distinct so adapter crates never see `CaptureEvent`.

use std::collections::BTreeSet;
use serde::{Deserialize, Serialize};

/// ULID identifying one envelope. Minted by the connector.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectorEventId(String);

impl ConnectorEventId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self { Self(value.into()) }
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

/// Stable reference to the upstream object that produced this event.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub kind: String,
    pub system_id: String,
    pub sub_id: Option<String>,
}

impl SourceRef {
    pub fn new(kind: impl Into<String>, system_id: impl Into<String>, sub_id: Option<String>) -> Self {
        Self { kind: kind.into(), system_id: system_id.into(), sub_id }
    }
}

/// Scope key the manifest matches against. `kind` names a pattern
/// family (`project`, `channel`, `workspace`, `path`); `value` is the
/// concrete instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorScope {
    pub kind: String,
    pub value: String,
}

impl ConnectorScope {
    #[must_use]
    pub fn project(value: impl Into<String>) -> Self {
        Self { kind: "project".into(), value: value.into() }
    }
    /// `"<kind>:<value>"` — the form `ConnectorConsentJournal::lookup`
    /// accepts as `scope_key`.
    #[must_use]
    pub fn lookup_key(&self) -> String { format!("{}:{}", self.kind, self.value) }
}

/// Mode this event was delivered through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeliveryMode {
    Poll { cursor: Option<String> },
    Webhook { signature_id: String },
}

/// Payload variant — keep the body schema closed so the redactor
/// knows how to walk it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConnectorPayload {
    Json   { mime: String, body: serde_json::Value },
    Text   { mime: String, body: String },
    Binary { mime: String, sha256: String, bytes_ref: String },
}

impl ConnectorPayload {
    #[must_use]
    pub fn mime(&self) -> &str {
        match self {
            Self::Json { mime, .. } | Self::Text { mime, .. } | Self::Binary { mime, .. } => mime,
        }
    }
    #[must_use]
    pub fn size_hint(&self) -> usize {
        match self {
            Self::Json { body, .. } => body.to_string().len(),
            Self::Text { body, .. } => body.len(),
            Self::Binary { .. } => 0, // spool path; framework measures via stat
        }
    }
}

/// Unified envelope emitted by every connector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorEvent {
    pub event_id: ConnectorEventId,
    pub connector: String,
    pub source_ref: SourceRef,
    pub occurred_at: i64,                 // unix seconds
    pub labels: BTreeSet<String>,
    pub scope: ConnectorScope,
    pub payload: ConnectorPayload,
    pub delivery: DeliveryMode,
}
```

- [ ] **Step 4: Re-run**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/event.rs
git commit -m "feat(connectors-core): ConnectorEvent envelope + payload types (#130)"
```

---

## Task 7 — `ConnectorManifest` TOML parser

**Files:**
- Modify: `crates/cairn-connectors-core/src/manifest.rs`

- [ ] **Step 1: Write the failing tests**

Append to `manifest.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[connector]
name = "fixture"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:remote:fixture:v1"

[capabilities]
poll = true
webhook = true
backfill = true

[oauth]
required_scopes = ["read:fixture"]
token_lifetime = "1h"
refresh = true

[budget]
max_items_per_hour = 600
max_bytes_per_day = "50MiB"

[labels]
allowed = ["note", "comment"]

[[scopes.declared]]
kind = "project"
pattern = "*"

[webhook]
"signature.algorithm" = "hmac-sha256"
"signature.header" = "X-Fixture-Signature"
allowed_mimes = ["application/json"]

[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"

[payload]
max_bytes = "256KiB"
max_depth = 12
"#;

    #[test]
    fn parses_valid_manifest() {
        let m = ConnectorManifest::parse_toml(VALID).expect("parse");
        assert_eq!(m.name(), "fixture");
        assert!(m.capabilities.poll);
        assert!(m.allowed_label("note"));
        assert!(!m.allowed_label("unknown"));
    }

    #[test]
    fn manifest_hash_stable() {
        let a = ConnectorManifest::parse_toml(VALID).unwrap().hash();
        let b = ConnectorManifest::parse_toml(VALID).unwrap().hash();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_dup_labels() {
        let bad = VALID.replace(r#"["note", "comment"]"#, r#"["note", "note"]"#);
        assert!(ConnectorManifest::parse_toml(&bad).is_err());
    }
}
```

- [ ] **Step 2: Run + confirm fail**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! `ConnectorManifest` — adapter-shipped TOML describing labels,
//! scopes, OAuth requirements, budgets, and payload limits. Hashed
//! into the consent journal on enable; drift triggers `ConsentRevoked`.

use std::collections::BTreeSet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::ConnectorError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorManifest {
    pub connector: ConnectorMeta,
    pub capabilities: CapabilitiesBlock,
    pub oauth: OauthBlock,
    pub budget: BudgetBlock,
    pub labels: LabelsBlock,
    pub scopes: ScopesBlock,
    pub webhook: WebhookBlock,
    pub poll: PollBlock,
    pub payload: PayloadBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorMeta {
    pub name: String,
    pub contract: String,                 // must equal "Connector"
    pub contract_version: String,
    pub sensor_identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesBlock {
    pub poll: bool,
    pub webhook: bool,
    pub backfill: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OauthBlock {
    pub required_scopes: Vec<String>,
    pub token_lifetime: String,           // parsed by adapters (e.g. "1h")
    pub refresh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetBlock {
    pub max_items_per_hour: u32,
    pub max_bytes_per_day: String,        // parsed lazily ("50MiB")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelsBlock {
    pub allowed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopesBlock {
    pub declared: Vec<ScopePattern>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopePattern {
    pub kind: String,
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookBlock {
    #[serde(rename = "signature.algorithm")]
    pub signature_algorithm: String,
    #[serde(rename = "signature.header")]
    pub signature_header: String,
    pub allowed_mimes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PollBlock {
    pub cursor_kind: String,
    pub min_interval: String,
    pub default_interval: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadBlock {
    pub max_bytes: String,
    pub max_depth: u32,
}

impl ConnectorManifest {
    pub fn parse_toml(src: &str) -> Result<Self, ConnectorError> {
        let parsed: Self = toml::from_str(src)
            .map_err(|e| ConnectorError::MalformedPayload(format!("manifest: {e}")))?;
        parsed.validate()?;
        Ok(parsed)
    }

    fn validate(&self) -> Result<(), ConnectorError> {
        if self.connector.contract != "Connector" {
            return Err(ConnectorError::MalformedPayload(
                format!("contract must be \"Connector\", got {}", self.connector.contract),
            ));
        }
        let mut seen = BTreeSet::new();
        for l in &self.labels.allowed {
            if !seen.insert(l) {
                return Err(ConnectorError::MalformedPayload(
                    format!("duplicate label {l}"),
                ));
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn name(&self) -> &str { &self.connector.name }

    #[must_use]
    pub fn allowed_label(&self, label: &str) -> bool {
        self.labels.allowed.iter().any(|l| l == label)
    }

    /// Stable hash recorded in the consent journal.
    #[must_use]
    pub fn hash(&self) -> String {
        let canonical = toml::to_string(self).expect("infallible: derived Serialize");
        let mut h = Sha256::new();
        h.update(canonical.as_bytes());
        format!("sha256:{}", hex::encode(h.finalize()))
    }
}
```

- [ ] **Step 4: Re-run**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/manifest.rs
git commit -m "feat(connectors-core): ConnectorManifest parser + stable hash (#130)"
```

---

## Task 8 — `Connector` trait + capabilities + plugin marker

**Files:**
- Modify: `crates/cairn-connectors-core/src/connector.rs`

- [ ] **Step 1: Write the failing test**

Append to `connector.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct Stub;
    #[async_trait::async_trait]
    impl Connector for Stub {
        fn name(&self) -> &str { "stub" }
        fn manifest(&self) -> &crate::manifest::ConnectorManifest { unimplemented!() }
        fn capabilities(&self) -> &ConnectorCapabilities {
            static C: ConnectorCapabilities = ConnectorCapabilities { poll: true, webhook: false, backfill: false };
            &C
        }
        fn sensor_identity(&self) -> &cairn_core::domain::Identity { unimplemented!() }
        fn supported_contract_versions(&self) -> cairn_core::contract::version::VersionRange {
            Self::SUPPORTED_VERSIONS
        }
        async fn poll(&self, _: &PollContext) -> Result<PollOutcome, crate::ConnectorError> {
            Ok(PollOutcome::default())
        }
        async fn ingest_webhook(&self, _: &crate::webhook::WebhookRequest, _: &WebhookContext) -> Result<Vec<crate::event::ConnectorEvent>, crate::ConnectorError> {
            Ok(vec![])
        }
    }
    impl ConnectorPlugin for Stub {
        const NAME: &'static str = "stub";
        const SUPPORTED_VERSIONS: cairn_core::contract::version::VersionRange =
            cairn_core::contract::version::VersionRange::new(
                cairn_core::contract::version::ContractVersion::new(0, 1, 0),
                cairn_core::contract::version::ContractVersion::new(0, 2, 0),
            );
    }

    #[test]
    fn dyn_compatible() {
        let _b: Box<dyn Connector> = Box::new(Stub);
    }
}
```

- [ ] **Step 2: Run + confirm fail**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! `Connector` trait + plugin marker (issue #130).

use std::sync::Arc;
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;

use crate::credential::CredentialHandle;
use crate::error::ConnectorError;
use crate::event::ConnectorEvent;
use crate::manifest::ConnectorManifest;
use crate::webhook::WebhookRequest;

/// Contract version for `Connector`. Bumps when the trait surface changes.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)] // three independent capability dims
pub struct ConnectorCapabilities {
    pub poll: bool,
    pub webhook: bool,
    pub backfill: bool,
}

/// Per-call context handed to `Connector::poll`.
pub struct PollContext {
    pub credentials: Arc<CredentialHandle>,
    pub last_cursor: Option<String>,
    pub budget_remaining_items: u32,
}

/// Per-call context handed to `Connector::ingest_webhook`.
pub struct WebhookContext {
    pub credentials: Arc<CredentialHandle>,
    pub budget_remaining_items: u32,
}

/// Result of one `poll` invocation. `next_cursor` is persisted by the
/// framework into the registry entry's state for the subsequent call.
#[derive(Debug, Default)]
pub struct PollOutcome {
    pub events: Vec<ConnectorEvent>,
    pub next_cursor: Option<String>,
    pub rate_limit_hint: Option<std::time::Duration>,
}

#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    fn name(&self) -> &str;
    fn manifest(&self) -> &ConnectorManifest;
    fn capabilities(&self) -> &ConnectorCapabilities;
    fn sensor_identity(&self) -> &Identity;
    fn supported_contract_versions(&self) -> VersionRange;

    async fn poll(&self, cx: &PollContext) -> Result<PollOutcome, ConnectorError>;
    async fn ingest_webhook(
        &self,
        req: &WebhookRequest,
        cx: &WebhookContext,
    ) -> Result<Vec<ConnectorEvent>, ConnectorError>;
}

pub trait ConnectorPlugin: Connector + Sized {
    const NAME: &'static str;
    const SUPPORTED_VERSIONS: VersionRange;
}
```

- [ ] **Step 4: Re-run**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked && cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings`
Expected: PASS. (The smoke test referenced `CONTRACT_VERSION`; the `pub use` in `lib.rs` already wires it.)

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/connector.rs
git commit -m "feat(connectors-core): Connector trait + capabilities + plugin marker (#130)"
```

---

## Task 9 — `CredentialStore` trait + in-memory impl

**Files:**
- Modify: `crates/cairn-connectors-core/src/credential.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_round_trip() {
        let store = InMemoryCredentialStore::default();
        store.put("fixture", b"tok-1".to_vec()).await.unwrap();
        let handle = store.get("fixture").await.unwrap();
        assert_eq!(handle.bytes(), b"tok-1");
    }

    #[tokio::test]
    async fn missing_returns_auth_expired() {
        let store = InMemoryCredentialStore::default();
        match store.get("absent").await {
            Err(crate::ConnectorError::AuthExpired { scope }) => assert_eq!(scope, "absent"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run + confirm fail.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! `CredentialStore` — opaque secret vault behind a trait so adapters
//! never see raw bytes outside the borrow handed to them per-call.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::error::ConnectorError;

/// Borrowed credential handed to a connector on one call. The bytes
/// live for the duration of the borrow; the store may rotate or
/// invalidate the underlying value afterwards.
pub struct CredentialHandle(Vec<u8>);

impl CredentialHandle {
    #[must_use]
    pub fn bytes(&self) -> &[u8] { &self.0 }
}

impl std::fmt::Debug for CredentialHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialHandle").field("bytes", &"<redacted>").finish()
    }
}

#[async_trait::async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get(&self, scope: &str) -> Result<Arc<CredentialHandle>, ConnectorError>;
    async fn put(&self, scope: &str, value: Vec<u8>) -> Result<(), ConnectorError>;
    async fn delete(&self, scope: &str) -> Result<(), ConnectorError>;
}

/// Process-local store for tests + in-memory adapters.
#[derive(Default)]
pub struct InMemoryCredentialStore {
    inner: RwLock<HashMap<String, Vec<u8>>>,
}

#[async_trait::async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn get(&self, scope: &str) -> Result<Arc<CredentialHandle>, ConnectorError> {
        let map = self.inner.read().await;
        match map.get(scope) {
            Some(v) => Ok(Arc::new(CredentialHandle(v.clone()))),
            None => Err(ConnectorError::AuthExpired { scope: scope.to_string() }),
        }
    }
    async fn put(&self, scope: &str, value: Vec<u8>) -> Result<(), ConnectorError> {
        self.inner.write().await.insert(scope.to_string(), value);
        Ok(())
    }
    async fn delete(&self, scope: &str) -> Result<(), ConnectorError> {
        self.inner.write().await.remove(scope);
        Ok(())
    }
}
```

- [ ] **Step 4: Re-run + clippy.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked && cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/credential.rs
git commit -m "feat(connectors-core): CredentialStore trait + InMemoryCredentialStore (#130)"
```

---

## Task 10 — `KeychainCredentialStore`

**Files:**
- Modify: `crates/cairn-connectors-core/src/credential_keychain.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cairn_keychain::keystore_for_discovery;

    // Keychain integration is feature-gated upstream; here we use
    // the file-backed keystore via cairn-keychain's discovery API.
    #[tokio::test]
    async fn keychain_round_trip_with_file_backend() {
        let temp = tempfile::tempdir().unwrap();
        let backend = keystore_for_discovery(temp.path()).expect("backend");
        let store = KeychainCredentialStore::new(backend, "test-conn".into());
        store.put("scope-a", b"tok".to_vec()).await.unwrap();
        let handle = store.get("scope-a").await.unwrap();
        assert_eq!(handle.bytes(), b"tok");
    }
}
```

> Adjust `keystore_for_discovery` to the real `cairn-keychain` function — `keystore_for_vault` may be the right call. Verify with `grep -n 'pub fn' crates/cairn-keychain/src/lib.rs` before writing the impl. The test must use whatever the keychain crate exposes for a non-OS-keychain backed store (file backend used in `cairn-keychain` itself).

- [ ] **Step 2: Run + confirm fail.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! Default `CredentialStore` backed by `cairn-keychain`.

use std::sync::Arc;
use async_trait::async_trait;

use crate::credential::{CredentialHandle, CredentialStore};
use crate::error::ConnectorError;

/// Stores secrets in the configured `cairn-keychain` backend. Keys are
/// namespaced under `connector/<name>/<scope>` so multiple connectors
/// can share one keystore safely.
pub struct KeychainCredentialStore {
    backend: cairn_keychain::Keystore, // adjust type to whatever cairn-keychain returns
    connector: String,
}

impl KeychainCredentialStore {
    #[must_use]
    pub fn new(backend: cairn_keychain::Keystore, connector: String) -> Self {
        Self { backend, connector }
    }
    fn key(&self, scope: &str) -> String {
        format!("connector/{}/{scope}", self.connector)
    }
}

#[async_trait]
impl CredentialStore for KeychainCredentialStore {
    async fn get(&self, scope: &str) -> Result<Arc<CredentialHandle>, ConnectorError> {
        let key = self.key(scope);
        let bytes = self.backend.get(&key)
            .map_err(|_| ConnectorError::AuthExpired { scope: scope.into() })?
            .ok_or_else(|| ConnectorError::AuthExpired { scope: scope.into() })?;
        Ok(Arc::new(CredentialHandle::from_bytes(bytes)))
    }
    async fn put(&self, scope: &str, value: Vec<u8>) -> Result<(), ConnectorError> {
        self.backend.put(&self.key(scope), &value)
            .map_err(|e| ConnectorError::Fatal(anyhow::anyhow!(e)))
    }
    async fn delete(&self, scope: &str) -> Result<(), ConnectorError> {
        self.backend.delete(&self.key(scope))
            .map_err(|e| ConnectorError::Fatal(anyhow::anyhow!(e)))
    }
}
```

Add a public `CredentialHandle::from_bytes(Vec<u8>) -> Self` constructor to `credential.rs` so this crate-internal callsite has a non-tuple way to build the handle. Keep the field private.

Verify `cairn-keychain` API names with `grep -n 'pub' crates/cairn-keychain/src/lib.rs`. The trait method signatures (`backend.get`, `backend.put`, `backend.delete`) here are illustrative — match the real surface (likely `Keystore::load_secret` / `store_secret` etc.). Update the calls accordingly.

- [ ] **Step 4: Re-run.**

Run: `cargo nextest run -p cairn-connectors-core --locked && cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/credential.rs crates/cairn-connectors-core/src/credential_keychain.rs
git commit -m "feat(connectors-core): KeychainCredentialStore default backend (#130)"
```

---

## Task 11 — `RedactionPipeline`

**Files:**
- Modify: `crates/cairn-connectors-core/src/redact.rs`

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::*;
    use std::collections::BTreeSet;

    fn evt(payload: ConnectorPayload) -> ConnectorEvent {
        ConnectorEvent {
            event_id: ConnectorEventId::new("01HX0000000000000000000000"),
            connector: "fixture".into(),
            source_ref: SourceRef::new("issue", "x", None),
            occurred_at: 0,
            labels: BTreeSet::new(),
            scope: ConnectorScope::project("p"),
            payload,
            delivery: DeliveryMode::Poll { cursor: None },
        }
    }

    #[test]
    fn email_in_text_is_redacted() {
        let pipeline = RedactionPipeline::new();
        let event = evt(ConnectorPayload::Text {
            mime: "text/plain".into(),
            body: "reach me at alice@example.com please".into(),
        });
        let out = pipeline.redact(event).unwrap();
        assert!(!out.spans.is_empty(), "must record at least one span");
        if let ConnectorPayload::Text { body, .. } = &out.event.payload {
            assert!(!body.contains("alice@example.com"));
        } else {
            panic!("expected text payload");
        }
    }

    #[test]
    fn email_in_json_leaf_is_redacted() {
        let pipeline = RedactionPipeline::new();
        let event = evt(ConnectorPayload::Json {
            mime: "application/json".into(),
            body: serde_json::json!({"author": "alice@example.com", "body": "hi"}),
        });
        let out = pipeline.redact(event).unwrap();
        let json = serde_json::to_string(&out.event).unwrap();
        assert!(!json.contains("alice@example.com"));
    }
}
```

- [ ] **Step 2: Run + confirm fail.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! Pre-Capture redaction. Walks `ConnectorPayload` leaves, applies
//! `cairn-core::pipeline::filter::redact::redact`, and emits the redacted
//! event plus the span list that the framework copies into
//! `CapturePayload::External.redacted_spans`.

use cairn_core::pipeline::filter::redact::{self, RedactionSpan};
use serde_json::Value;

use crate::error::ConnectorError;
use crate::event::{ConnectorEvent, ConnectorPayload};

pub struct RedactionPipeline { /* config slots reserved for later */ }

impl Default for RedactionPipeline { fn default() -> Self { Self::new() } }

impl RedactionPipeline {
    #[must_use]
    pub fn new() -> Self { Self {} }

    /// Redact the event in place and return the post-redaction event
    /// plus the full span list (positions reference the original
    /// pre-redaction bytes of each leaf).
    pub fn redact(&self, mut event: ConnectorEvent) -> Result<Redacted, ConnectorError> {
        let mut spans = Vec::new();
        match &mut event.payload {
            ConnectorPayload::Text { body, .. } => {
                let r = redact::redact(body);
                spans.extend(r.spans);
                *body = r.text;
            }
            ConnectorPayload::Json { body, .. } => {
                Self::walk_json(body, &mut spans);
            }
            ConnectorPayload::Binary { .. } => {
                // Bytes already spooled by the caller; nothing to redact in
                // the envelope itself.
            }
        }
        Ok(Redacted { event, spans })
    }

    fn walk_json(value: &mut Value, spans: &mut Vec<RedactionSpan>) {
        match value {
            Value::String(s) => {
                let r = redact::redact(s);
                spans.extend(r.spans);
                *s = r.text;
            }
            Value::Array(items) => items.iter_mut().for_each(|v| Self::walk_json(v, spans)),
            Value::Object(map) => map.values_mut().for_each(|v| Self::walk_json(v, spans)),
            _ => {}
        }
    }
}

#[derive(Debug)]
pub struct Redacted {
    pub event: ConnectorEvent,
    pub spans: Vec<RedactionSpan>,
}
```

- [ ] **Step 4: Re-run + clippy.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked && cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/redact.rs
git commit -m "feat(connectors-core): RedactionPipeline over text + JSON leaves (#130)"
```

---

## Task 12 — `RateLimit` token bucket

**Files:**
- Modify: `crates/cairn-connectors-core/src/rate_limit.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charges_within_budget() {
        let rl = RateLimit::per_hour("p1".into(), 3);
        for _ in 0..3 {
            rl.charge("p1", 1).expect("under budget");
        }
        assert!(matches!(
            rl.charge("p1", 1),
            Err(crate::ConnectorError::BudgetExceeded { .. }),
        ));
    }

    #[test]
    fn separate_scopes_have_separate_budgets() {
        let rl = RateLimit::per_hour("p1".into(), 1);
        rl.add_scope("p2".into(), 1);
        rl.charge("p1", 1).unwrap();
        rl.charge("p2", 1).unwrap();
        assert!(rl.charge("p1", 1).is_err());
    }
}
```

- [ ] **Step 2: Run + confirm fail.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! Token bucket keyed by scope. One bucket per `(connector, scope)`;
//! refill uses wall-clock — no async, no shared runtime state beyond
//! a `Mutex<HashMap>`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::ConnectorError;

#[derive(Debug)]
pub struct RateLimit {
    inner: Mutex<HashMap<String, Bucket>>,
    refill_interval: Duration,
}

#[derive(Debug)]
struct Bucket {
    remaining: u32,
    capacity: u32,
    last_refill: Instant,
}

impl RateLimit {
    #[must_use]
    pub fn per_hour(scope: String, capacity: u32) -> Self {
        let mut map = HashMap::new();
        map.insert(scope, Bucket { remaining: capacity, capacity, last_refill: Instant::now() });
        Self { inner: Mutex::new(map), refill_interval: Duration::from_secs(3600) }
    }

    pub fn add_scope(&self, scope: String, capacity: u32) {
        let mut map = self.inner.lock().unwrap();
        map.insert(scope, Bucket { remaining: capacity, capacity, last_refill: Instant::now() });
    }

    pub fn charge(&self, scope: &str, amount: u32) -> Result<(), ConnectorError> {
        let mut map = self.inner.lock().unwrap();
        let bucket = map.get_mut(scope)
            .ok_or_else(|| ConnectorError::BudgetExceeded { scope: scope.into() })?;
        if bucket.last_refill.elapsed() >= self.refill_interval {
            bucket.remaining = bucket.capacity;
            bucket.last_refill = Instant::now();
        }
        if bucket.remaining < amount {
            return Err(ConnectorError::BudgetExceeded { scope: scope.into() });
        }
        bucket.remaining -= amount;
        Ok(())
    }
}
```

- [ ] **Step 4: Re-run.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked && cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/rate_limit.rs
git commit -m "feat(connectors-core): per-scope token-bucket RateLimit (#130)"
```

---

## Task 13 — `WebhookRouter` + HMAC verifier

**Files:**
- Modify: `crates/cairn-connectors-core/src/webhook.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_hmac_sha256_signature() {
        let secret = b"shh";
        let body = b"{\"x\":1}";
        let sig = hex_hmac_sha256(secret, body);
        let req = WebhookRequest {
            connector: "fixture".into(),
            body: body.to_vec(),
            headers: vec![("X-Fixture-Signature".into(), sig.clone())],
        };
        let res = verify_hmac_sha256(&req, "X-Fixture-Signature", secret);
        assert!(matches!(res, Ok(SignatureId(s)) if s == sig));
    }

    #[test]
    fn rejects_wrong_signature() {
        let req = WebhookRequest {
            connector: "fixture".into(),
            body: b"{}".to_vec(),
            headers: vec![("X-Fixture-Signature".into(), "deadbeef".into())],
        };
        assert!(matches!(
            verify_hmac_sha256(&req, "X-Fixture-Signature", b"shh"),
            Err(crate::ConnectorError::SignatureMismatch),
        ));
    }
}
```

- [ ] **Step 2: Run + confirm fail.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! Webhook request envelope + HMAC verifier. The framework owns the
//! HTTP layer (`axum::Router`), but signature verification is pure
//! and lives here so connectors / tests can call it directly.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::error::ConnectorError;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct WebhookRequest {
    pub connector: String,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl WebhookRequest {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureId(pub String);

/// Verify an HMAC-SHA256 signature whose hex form is carried in `header`.
pub fn verify_hmac_sha256(
    req: &WebhookRequest,
    header: &str,
    secret: &[u8],
) -> Result<SignatureId, ConnectorError> {
    let sig_hex = req.header(header).ok_or(ConnectorError::SignatureMismatch)?;
    let provided = hex::decode(sig_hex).map_err(|_| ConnectorError::SignatureMismatch)?;
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| ConnectorError::SignatureMismatch)?;
    mac.update(&req.body);
    let computed = mac.finalize().into_bytes();
    if computed.ct_eq(&provided).into() {
        Ok(SignatureId(sig_hex.to_string()))
    } else {
        Err(ConnectorError::SignatureMismatch)
    }
}

/// Helper used by tests + adapters to produce a canonical signature.
#[must_use]
pub fn hex_hmac_sha256(secret: &[u8], body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

/// Composes per-connector axum routes the registry mounts under
/// `/webhooks`. Empty until `WebhookRouter::register` is called; left as
/// a thin shell here so the registry can compose it without a circular
/// dep on adapter crates.
#[derive(Default)]
pub struct WebhookRouter {
    routes: Vec<axum::Router>,
}

impl WebhookRouter {
    #[must_use]
    pub fn new() -> Self { Self::default() }

    pub fn mount(&mut self, route: axum::Router) {
        self.routes.push(route);
    }

    #[must_use]
    pub fn into_router(self) -> axum::Router {
        let mut r = axum::Router::new();
        for sub in self.routes { r = r.merge(sub); }
        r
    }
}
```

- [ ] **Step 4: Re-run + clippy.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked && cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/webhook.rs
git commit -m "feat(connectors-core): WebhookRouter + HMAC-SHA256 verifier (#130)"
```

---

## Task 14 — `PollScheduler`

**Files:**
- Modify: `crates/cairn-connectors-core/src/poll.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn poll_loop_invokes_callback_and_stops_on_cancel() {
        let count = Arc::new(AtomicUsize::new(0));
        let token = CancellationToken::new();
        let c = count.clone();
        let scheduler = PollScheduler::new(token.clone(), Duration::from_millis(20));
        scheduler.spawn("fixture".into(), move |_cursor| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
                Ok::<_, crate::ConnectorError>(PollTick::done(None))
            }
        });
        tokio::time::sleep(Duration::from_millis(80)).await;
        token.cancel();
        scheduler.shutdown().await;
        let observed = count.load(Ordering::SeqCst);
        assert!(observed >= 2, "expected ≥2 ticks, got {observed}");
    }
}
```

- [ ] **Step 2: Run + confirm fail.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! Per-connector polling. `PollScheduler::spawn` registers a
//! callback that the scheduler invokes every `interval` until the
//! shared `CancellationToken` fires.

use std::future::Future;
use std::time::Duration;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::error::ConnectorError;

/// What one `poll` callback returns.
#[derive(Debug)]
pub struct PollTick { pub next_cursor: Option<String> }

impl PollTick {
    #[must_use]
    pub fn done(next_cursor: Option<String>) -> Self { Self { next_cursor } }
}

pub struct PollScheduler {
    token: CancellationToken,
    interval: Duration,
    tasks: JoinSet<()>,
}

impl PollScheduler {
    #[must_use]
    pub fn new(token: CancellationToken, interval: Duration) -> Self {
        Self { token, interval, tasks: JoinSet::new() }
    }

    pub fn spawn<F, Fut>(&mut self, name: String, mut tick: F)
    where
        F: FnMut(Option<String>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<PollTick, ConnectorError>> + Send + 'static,
    {
        let token = self.token.clone();
        let interval = self.interval;
        self.tasks.spawn(async move {
            let mut cursor: Option<String> = None;
            loop {
                tokio::select! {
                    () = token.cancelled() => break,
                    () = tokio::time::sleep(interval) => {
                        match tick(cursor.clone()).await {
                            Ok(t) => cursor = t.next_cursor,
                            Err(err) => {
                                tracing::warn!(connector = %name, ?err, "poll error");
                            }
                        }
                    }
                }
            }
        });
    }

    pub async fn shutdown(mut self) {
        while self.tasks.join_next().await.is_some() {}
    }
}
```

`spawn` takes `&mut self`; the test must hold the scheduler in a `let mut` binding. Update the test accordingly:

```rust
let mut scheduler = PollScheduler::new(token.clone(), Duration::from_millis(20));
```

- [ ] **Step 4: Re-run + clippy.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked && cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/poll.rs
git commit -m "feat(connectors-core): PollScheduler with cancel + cursor (#130)"
```

---

## Task 15 — `PipelineEmit` trait + `CaptureEvent` builder

**Files:**
- Modify: `crates/cairn-connectors-core/src/emit.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::*;
    use cairn_core::domain::Identity;
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Recorder(Mutex<Vec<cairn_core::domain::capture::CaptureEvent>>);
    #[async_trait::async_trait]
    impl PipelineEmit for Recorder {
        async fn emit(&self, event: cairn_core::domain::capture::CaptureEvent)
            -> Result<(), crate::ConnectorError> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
    }

    #[tokio::test]
    async fn build_and_emit_external_capture_event() {
        let recorder = Arc::new(Recorder::default());
        let sensor = Identity::test_sensor("fixture");
        let event = ConnectorEvent {
            event_id: ConnectorEventId::new("01HX0000000000000000000000"),
            connector: "fixture".into(),
            source_ref: SourceRef::new("issue", "gh:1", None),
            occurred_at: 1_700_000_000,
            labels: BTreeSet::from(["note".to_string()]),
            scope: ConnectorScope::project("owner/repo"),
            payload: ConnectorPayload::Json { mime: "application/json".into(), body: serde_json::json!({"k":"v"}) },
            delivery: DeliveryMode::Poll { cursor: None },
        };
        let captured = build_capture_event(&event, &sensor, vec![], "spool/fixture/abc")
            .expect("build");
        recorder.emit(captured).await.unwrap();
        assert_eq!(recorder.0.lock().unwrap().len(), 1);
    }
}
```

> If `Identity::test_sensor` is not the actual helper name, grep `crates/cairn-core/src/domain/identity.rs` and substitute.

- [ ] **Step 2: Run + confirm fail.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
//! `PipelineEmit` — the single outbound entrypoint from
//! cairn-connectors-core into the cairn-core ingestion pipeline.
//! The framework constructs a `CaptureEvent` (external-payload
//! variant) after redaction + consent, then calls `emit`.

use std::collections::BTreeSet;

use cairn_core::domain::capture::{
    ActorChainEntry, CaptureEvent, CaptureEventId, CaptureMode, CapturePayload,
    PayloadHash, Rfc3339Timestamp, SourceFamily,
};
use cairn_core::domain::Identity;
use cairn_core::pipeline::filter::redact::RedactionSpan;

use crate::error::ConnectorError;
use crate::event::ConnectorEvent;

#[async_trait::async_trait]
pub trait PipelineEmit: Send + Sync {
    async fn emit(&self, event: CaptureEvent) -> Result<(), ConnectorError>;
}

/// Convert a redacted [`ConnectorEvent`] into a [`CaptureEvent`] with
/// `source_family: External`. The framework calls this between the
/// redaction stage and the `PipelineEmit::emit` call.
pub fn build_capture_event(
    event: &ConnectorEvent,
    sensor: &Identity,
    redacted_spans: Vec<RedactionSpan>,
    payload_ref: &str,
) -> Result<CaptureEvent, ConnectorError> {
    let payload = CapturePayload::External {
        connector: event.connector.clone(),
        source_ref: cairn_core::domain::capture::SourceRef::new(
            &event.source_ref.kind,
            &event.source_ref.system_id,
            event.source_ref.sub_id.clone(),
        ),
        labels: event.labels.clone(),
        mime: event.payload.mime().to_string(),
        redacted_spans,
    };

    let actor_chain = vec![ActorChainEntry::sensor(sensor.clone())];
    let payload_hash = PayloadHash::for_bytes(payload_ref.as_bytes());

    CaptureEvent::try_new(
        CaptureEventId::new(event.event_id.as_str()),
        sensor.clone(),
        CaptureMode::Auto,
        actor_chain,
        None,
        payload_hash,
        payload_ref.to_string(),
        Rfc3339Timestamp::from_unix(event.occurred_at),
        payload,
        SourceFamily::External,
    )
    .map_err(|e| ConnectorError::MalformedPayload(e.to_string()))
}

#[allow(dead_code)]
fn _suppress_unused(_b: BTreeSet<String>) {}
```

> The exact constructors for `CaptureEventId`, `ActorChainEntry::sensor`, `PayloadHash::for_bytes`, `Rfc3339Timestamp::from_unix` may be named differently. Open `crates/cairn-core/src/domain/capture.rs` and `crates/cairn-core/src/domain/identity.rs` first; substitute the actual helpers. If a helper does not yet exist, add it as a small typed constructor — never inline raw strings into `CaptureEvent`.

- [ ] **Step 4: Re-run + clippy.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked && cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/emit.rs
git commit -m "feat(connectors-core): PipelineEmit + ConnectorEvent→CaptureEvent builder (#130)"
```

---

## Task 16 — `ConnectorRegistry` lifecycle

**Files:**
- Modify: `crates/cairn-connectors-core/src/registry.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::FixtureConnector;
    use crate::credential::InMemoryCredentialStore;
    use crate::emit::PipelineEmit;
    use std::sync::Arc;

    struct NoopEmit;
    #[async_trait::async_trait]
    impl PipelineEmit for NoopEmit {
        async fn emit(&self, _: cairn_core::domain::capture::CaptureEvent) -> Result<(), crate::ConnectorError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn register_then_enable_then_disable() {
        let mut reg = ConnectorRegistry::builder()
            .credentials(Arc::new(InMemoryCredentialStore::default()))
            .consent(Arc::new(crate::fixture::AcceptAllConsent::default()))
            .emit(Arc::new(NoopEmit))
            .build();
        reg.register(FixtureConnector::with_default_manifest()).unwrap();
        reg.enable("fixture", crate::fixture::default_grant()).await.unwrap();
        reg.disable("fixture").await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_register_rejected() {
        let mut reg = ConnectorRegistry::builder()
            .credentials(Arc::new(InMemoryCredentialStore::default()))
            .consent(Arc::new(crate::fixture::AcceptAllConsent::default()))
            .emit(Arc::new(NoopEmit))
            .build();
        reg.register(FixtureConnector::with_default_manifest()).unwrap();
        assert!(reg.register(FixtureConnector::with_default_manifest()).is_err());
    }
}
```

This test references `FixtureConnector`, `AcceptAllConsent`, and `default_grant` — those come in T17. Mark T16 + T17 as a coordinated pair: write T16's impl with `FixtureConnector` stubbed to `unimplemented!()`, then T17 lands the fixture and re-runs T16's tests.

- [ ] **Step 2: Run + confirm fail.**

Run: `cargo nextest run -p cairn-connectors-core --lib --locked`
Expected: FAIL (unresolved imports).

- [ ] **Step 3: Implement**

```rust
//! `ConnectorRegistry` — central lifecycle: register → enable →
//! poll/webhook → disable → shutdown.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use bon::Builder;
use tokio_util::sync::CancellationToken;

use cairn_core::contract::connector_consent::{ConnectorConsentJournal, ConsentGrant, ConsentGrantId};

use crate::connector::{Connector, ConnectorPlugin};
use crate::credential::CredentialStore;
use crate::emit::PipelineEmit;
use crate::error::ConnectorError;
use crate::poll::{PollScheduler, PollTick};

#[derive(Debug, Clone)]
enum ConnectorState {
    Disabled,
    Enabled { grant_id: ConsentGrantId },
}

struct Entry {
    connector: Arc<dyn Connector>,
    state: ArcSwap<ConnectorState>,
}

#[derive(Builder)]
pub struct ConnectorRegistry {
    credentials: Arc<dyn CredentialStore>,
    consent: Arc<dyn ConnectorConsentJournal>,
    emit: Arc<dyn PipelineEmit>,
    #[builder(default = CancellationToken::new())]
    shutdown: CancellationToken,
    #[builder(skip)]
    entries: HashMap<String, Entry>,
    #[builder(skip)]
    scheduler: Option<PollScheduler>,
}

impl ConnectorRegistry {
    pub fn register<P: ConnectorPlugin + 'static>(&mut self, plugin: P) -> Result<(), ConnectorError> {
        let name = plugin.name().to_string();
        if self.entries.contains_key(&name) {
            return Err(ConnectorError::Fatal(anyhow::anyhow!("duplicate connector {name}")));
        }
        self.entries.insert(name, Entry {
            connector: Arc::new(plugin),
            state: ArcSwap::from_pointee(ConnectorState::Disabled),
        });
        Ok(())
    }

    pub async fn enable(&mut self, name: &str, grant: ConsentGrant) -> Result<(), ConnectorError> {
        let entry = self.entries.get(name)
            .ok_or_else(|| ConnectorError::Fatal(anyhow::anyhow!("unknown connector {name}")))?;
        let grant_id = self.consent.put_grant(grant).await
            .map_err(|e| ConnectorError::Fatal(anyhow::anyhow!(e)))?;
        entry.state.store(Arc::new(ConnectorState::Enabled { grant_id }));

        // Spawn poll task if connector advertises the capability.
        if entry.connector.capabilities().poll {
            let sched = self.scheduler.get_or_insert_with(|| {
                PollScheduler::new(self.shutdown.clone(), Duration::from_secs(300))
            });
            let connector = entry.connector.clone();
            let emit = self.emit.clone();
            let consent = self.consent.clone();
            let _credentials = self.credentials.clone();
            sched.spawn(name.to_string(), move |cursor| {
                let connector = connector.clone();
                let emit = emit.clone();
                let consent = consent.clone();
                async move {
                    let cx = crate::connector::PollContext {
                        credentials: Arc::new(crate::credential::CredentialHandle::empty()),
                        last_cursor: cursor.clone(),
                        budget_remaining_items: u32::MAX,
                    };
                    let outcome = connector.poll(&cx).await?;
                    for event in outcome.events {
                        let lookup_key = event.scope.lookup_key();
                        if !matches!(
                            consent.lookup(&event.connector, &lookup_key).await,
                            Ok(cairn_core::contract::connector_consent::ConsentLookup::Granted),
                        ) {
                            return Err(ConnectorError::ConsentRevoked { connector: event.connector });
                        }
                        let pipeline = crate::redact::RedactionPipeline::new();
                        let redacted = pipeline.redact(event)?;
                        let captured = crate::emit::build_capture_event(
                            &redacted.event,
                            connector.sensor_identity(),
                            redacted.spans,
                            "spool/connector/placeholder",
                        )?;
                        emit.emit(captured).await?;
                    }
                    Ok(PollTick::done(outcome.next_cursor))
                }
            });
        }
        Ok(())
    }

    pub async fn disable(&mut self, name: &str) -> Result<(), ConnectorError> {
        let entry = self.entries.get(name)
            .ok_or_else(|| ConnectorError::Fatal(anyhow::anyhow!("unknown connector {name}")))?;
        if let ConnectorState::Enabled { grant_id } = (**entry.state.load()).clone() {
            self.consent.revoke(&grant_id).await
                .map_err(|e| ConnectorError::Fatal(anyhow::anyhow!(e)))?;
        }
        entry.state.store(Arc::new(ConnectorState::Disabled));
        Ok(())
    }

    pub async fn shutdown(self) {
        self.shutdown.cancel();
        if let Some(sched) = self.scheduler { sched.shutdown().await; }
    }
}
```

Add `pub fn empty() -> Self { Self::from_bytes(Vec::new()) }` to `CredentialHandle` in `credential.rs` so the closure has something to construct without an OAuth handshake. (Adapters in #131 will replace this with a real lookup.)

- [ ] **Step 4: Re-run** (will still fail until T17 lands fixture)

Run: `cargo check -p cairn-connectors-core --locked`
Expected: PASS (compiles), unit tests still failing because fixture missing.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/registry.rs crates/cairn-connectors-core/src/credential.rs
git commit -m "feat(connectors-core): ConnectorRegistry lifecycle (#130)"
```

---

## Task 17 — `FixtureConnector` + helpers

**Files:**
- Modify: `crates/cairn-connectors-core/src/fixture.rs`

- [ ] **Step 1: Write the failing test**

(Already covered by T16's tests + T18's contract test; just confirm compilation here.)

- [ ] **Step 2: Implement fixture**

```rust
//! In-tree connector + consent journal used by every framework test.
//! Cfg-gated to keep the substrate crate runtime-clean for downstream
//! consumers that opt in with `features = ["fixture"]`.

use std::collections::BTreeSet;
use std::sync::Mutex;

use cairn_core::contract::connector_consent::{
    ConnectorConsentJournal, ConsentGrant, ConsentGrantId, ConsentLookup,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;

use crate::connector::{Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext};
use crate::error::ConnectorError;
use crate::event::{ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef};
use crate::manifest::ConnectorManifest;
use crate::webhook::WebhookRequest;

pub struct FixtureConnector {
    manifest: ConnectorManifest,
    sensor: Identity,
}

impl FixtureConnector {
    #[must_use]
    pub fn with_default_manifest() -> Self {
        let manifest = ConnectorManifest::parse_toml(DEFAULT_MANIFEST).expect("valid manifest");
        let sensor = Identity::test_sensor("fixture"); // adjust to actual helper
        Self { manifest, sensor }
    }
}

#[async_trait::async_trait]
impl Connector for FixtureConnector {
    fn name(&self) -> &str { self.manifest.name() }
    fn manifest(&self) -> &ConnectorManifest { &self.manifest }
    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities = ConnectorCapabilities { poll: true, webhook: true, backfill: false };
        &C
    }
    fn sensor_identity(&self) -> &Identity { &self.sensor }
    fn supported_contract_versions(&self) -> VersionRange { Self::SUPPORTED_VERSIONS }

    async fn poll(&self, _cx: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome {
            events: vec![sample_event()],
            next_cursor: Some("c1".into()),
            rate_limit_hint: None,
        })
    }
    async fn ingest_webhook(&self, _req: &WebhookRequest, _cx: &WebhookContext) -> Result<Vec<ConnectorEvent>, ConnectorError> {
        Ok(vec![sample_event()])
    }
}

impl ConnectorPlugin for FixtureConnector {
    const NAME: &'static str = "fixture";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

#[must_use]
fn sample_event() -> ConnectorEvent {
    ConnectorEvent {
        event_id: ConnectorEventId::new("01HX0000000000000000000000"),
        connector: "fixture".into(),
        source_ref: SourceRef::new("issue", "gh:owner/repo#1", None),
        occurred_at: 1_700_000_000,
        labels: BTreeSet::from(["note".to_string()]),
        scope: ConnectorScope::project("owner/repo"),
        payload: ConnectorPayload::Json {
            mime: "application/json".into(),
            body: serde_json::json!({"body": "hello"}),
        },
        delivery: DeliveryMode::Poll { cursor: Some("c0".into()) },
    }
}

#[must_use]
pub fn default_grant() -> ConsentGrant {
    ConsentGrant {
        connector: "fixture".into(),
        manifest_hash: "h0".into(),
        allowed_labels: BTreeSet::from(["note".to_string()]),
        scope_patterns: vec!["project:*".into()],
        granted_at: 0,
        grantor: Identity::test_user("alice"),
    }
}

#[derive(Default)]
pub struct AcceptAllConsent { grants: Mutex<Vec<ConsentGrant>> }

#[async_trait::async_trait]
impl ConnectorConsentJournal for AcceptAllConsent {
    async fn put_grant(&self, g: ConsentGrant) -> Result<ConsentGrantId, String> {
        let id = ConsentGrantId::new(format!("gnt:{}", g.connector));
        self.grants.lock().unwrap().push(g);
        Ok(id)
    }
    async fn lookup(&self, connector: &str, _scope_key: &str) -> Result<ConsentLookup, String> {
        Ok(if self.grants.lock().unwrap().iter().any(|g| g.connector == connector) {
            ConsentLookup::Granted
        } else {
            ConsentLookup::Revoked
        })
    }
    async fn revoke(&self, _: &ConsentGrantId) -> Result<(), String> {
        self.grants.lock().unwrap().clear();
        Ok(())
    }
}

const DEFAULT_MANIFEST: &str = r#"
[connector]
name = "fixture"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:remote:fixture:v1"

[capabilities]
poll = true
webhook = true
backfill = false

[oauth]
required_scopes = ["read:fixture"]
token_lifetime = "1h"
refresh = true

[budget]
max_items_per_hour = 600
max_bytes_per_day = "50MiB"

[labels]
allowed = ["note", "comment"]

[[scopes.declared]]
kind = "project"
pattern = "*"

[webhook]
"signature.algorithm" = "hmac-sha256"
"signature.header" = "X-Fixture-Signature"
allowed_mimes = ["application/json"]

[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"

[payload]
max_bytes = "256KiB"
max_depth = 12
"#;
```

- [ ] **Step 3: Re-run all unit tests + clippy.**

Run: `cargo nextest run -p cairn-connectors-core --locked && cargo clippy -p cairn-connectors-core --all-targets --locked -- -D warnings`
Expected: PASS (including T16's registry tests).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-connectors-core/src/fixture.rs
git commit -m "feat(connectors-core): FixtureConnector + AcceptAllConsent helpers (#130)"
```

---

## Task 18 — `contract.rs` integration test

**Files:**
- Modify: `crates/cairn-connectors-core/tests/contract.rs`

- [ ] **Step 1: Write the test**

```rust
//! Happy-path contract: register → enable → poll → emit yields a
//! CaptureEvent with source_family == External.

use std::sync::{Arc, Mutex};

use cairn_connectors_core::{
    ConnectorRegistry, InMemoryCredentialStore, PipelineEmit,
    fixture::{AcceptAllConsent, FixtureConnector, default_grant},
};
use cairn_core::domain::capture::{CaptureEvent, SourceFamily};

#[derive(Default)]
struct Capturer(Mutex<Vec<CaptureEvent>>);

#[async_trait::async_trait]
impl PipelineEmit for Capturer {
    async fn emit(&self, event: CaptureEvent) -> Result<(), cairn_connectors_core::ConnectorError> {
        self.0.lock().unwrap().push(event);
        Ok(())
    }
}

#[tokio::test]
async fn happy_path_emits_external_capture_event() {
    let capturer = Arc::new(Capturer::default());
    let mut registry = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(capturer.clone() as Arc<dyn PipelineEmit>)
        .build();

    registry.register(FixtureConnector::with_default_manifest()).unwrap();
    registry.enable("fixture", default_grant()).await.unwrap();

    // The default scheduler interval is 5 minutes; force one poll tick for the test.
    // The registry exposes `poll_now` for tests via `cfg(test)`.
    registry.poll_now("fixture").await.unwrap();

    registry.shutdown().await;
    let events = capturer.0.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].source_family, SourceFamily::External);
}
```

- [ ] **Step 2: Run + confirm fail (missing `poll_now`).**

Run: `cargo nextest run -p cairn-connectors-core --test contract --locked`
Expected: FAIL.

- [ ] **Step 3: Add `ConnectorRegistry::poll_now`**

In `registry.rs`, add behind `#[cfg(any(test, feature = "fixture"))]`:

```rust
#[cfg(any(test, feature = "fixture"))]
impl ConnectorRegistry {
    /// Tests: trigger one poll cycle without waiting for the scheduler.
    pub async fn poll_now(&self, name: &str) -> Result<(), ConnectorError> {
        let entry = self.entries.get(name)
            .ok_or_else(|| ConnectorError::Fatal(anyhow::anyhow!("unknown connector {name}")))?;
        let cx = crate::connector::PollContext {
            credentials: Arc::new(crate::credential::CredentialHandle::empty()),
            last_cursor: None,
            budget_remaining_items: u32::MAX,
        };
        let outcome = entry.connector.poll(&cx).await?;
        for event in outcome.events {
            let lookup_key = event.scope.lookup_key();
            if !matches!(
                self.consent.lookup(&event.connector, &lookup_key).await,
                Ok(cairn_core::contract::connector_consent::ConsentLookup::Granted),
            ) {
                return Err(ConnectorError::ConsentRevoked { connector: event.connector });
            }
            // Manifest label gate.
            for label in &event.labels {
                if !entry.connector.manifest().allowed_label(label) {
                    return Err(ConnectorError::UndeclaredLabel { label: label.clone() });
                }
            }
            let pipeline = crate::redact::RedactionPipeline::new();
            let redacted = pipeline.redact(event)?;
            let captured = crate::emit::build_capture_event(
                &redacted.event,
                entry.connector.sensor_identity(),
                redacted.spans,
                "spool/test/abc",
            )?;
            self.emit.emit(captured).await?;
        }
        Ok(())
    }
}
```

Hoist the same body into the production poll closure (replace the inline copy in `enable`) so tests + scheduler share one code path. The label gate and consent gate must live in the framework — never inside the fixture.

- [ ] **Step 4: Re-run.**

Run: `cargo nextest run -p cairn-connectors-core --locked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-connectors-core/src/registry.rs crates/cairn-connectors-core/tests/contract.rs
git commit -m "test(connectors-core): happy-path register→enable→poll→emit (#130)"
```

---

## Task 19 — `payload_validation.rs` proptest

**Files:**
- Modify: `crates/cairn-connectors-core/tests/payload_validation.rs`

- [ ] **Step 1: Write the proptest**

```rust
//! Proptest: no raw byte from a webhook body can reach the emitted
//! CaptureEvent unredacted if the cairn-core redactor would have caught it.

use cairn_connectors_core::redact::RedactionPipeline;
use cairn_connectors_core::event::{ConnectorEvent, ConnectorEventId, ConnectorPayload, ConnectorScope, DeliveryMode, SourceRef};
use cairn_core::pipeline::filter::redact::redact as core_redact;
use proptest::prelude::*;
use std::collections::BTreeSet;

proptest! {
    #[test]
    fn redaction_removes_all_pii_the_core_detector_finds(
        body in r"[a-z0-9 ]{0,40}(alice@example\.com)?[a-z0-9 ]{0,40}",
    ) {
        let event = ConnectorEvent {
            event_id: ConnectorEventId::new("01HX0000000000000000000000"),
            connector: "fixture".into(),
            source_ref: SourceRef::new("issue", "x", None),
            occurred_at: 0,
            labels: BTreeSet::new(),
            scope: ConnectorScope::project("p"),
            payload: ConnectorPayload::Text { mime: "text/plain".into(), body: body.clone() },
            delivery: DeliveryMode::Poll { cursor: None },
        };
        let r = RedactionPipeline::new().redact(event).unwrap();
        let expected_spans = core_redact(&body).spans.len();
        prop_assert_eq!(r.spans.len(), expected_spans);
        if let ConnectorPayload::Text { body: out, .. } = &r.event.payload {
            prop_assert!(!out.contains("alice@example.com"));
        } else {
            unreachable!();
        }
    }
}
```

- [ ] **Step 2: Run.**

Run: `cargo nextest run -p cairn-connectors-core --test payload_validation --locked`
Expected: PASS. If a counter-example fires, the proptest will write a regression file under `proptest-regressions/` — commit it alongside.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-connectors-core/tests/payload_validation.rs crates/cairn-connectors-core/proptest-regressions
git commit -m "test(connectors-core): payload redaction proptest (#130)"
```

---

## Task 20 — Remaining acceptance-criteria tests

Each test below follows the same shape: build a registry with `AcceptAllConsent`, register a fixture, drive one cycle, assert. Code stubs given in full.

### Task 20a — `undeclared_label_rejected.rs`

**File:** `crates/cairn-connectors-core/tests/undeclared_label_rejected.rs`

```rust
use std::sync::Arc;
use std::collections::BTreeSet;

use cairn_connectors_core::{
    ConnectorRegistry, InMemoryCredentialStore, PipelineEmit, ConnectorError,
    fixture::{AcceptAllConsent, default_grant},
};
use cairn_connectors_core::connector::{Connector, ConnectorCapabilities, ConnectorPlugin, PollContext, PollOutcome, WebhookContext};
use cairn_connectors_core::event::*;
use cairn_connectors_core::manifest::ConnectorManifest;
use cairn_connectors_core::webhook::WebhookRequest;
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::domain::Identity;
use cairn_core::domain::capture::CaptureEvent;

struct EvilConnector { manifest: ConnectorManifest, sensor: Identity }
impl EvilConnector {
    fn new() -> Self {
        Self {
            manifest: ConnectorManifest::parse_toml(MANIFEST).unwrap(),
            sensor: Identity::test_sensor("evil"),
        }
    }
}
#[async_trait::async_trait]
impl Connector for EvilConnector {
    fn name(&self) -> &str { "evil" }
    fn manifest(&self) -> &ConnectorManifest { &self.manifest }
    fn capabilities(&self) -> &ConnectorCapabilities {
        static C: ConnectorCapabilities = ConnectorCapabilities { poll: true, webhook: false, backfill: false };
        &C
    }
    fn sensor_identity(&self) -> &Identity { &self.sensor }
    fn supported_contract_versions(&self) -> VersionRange { Self::SUPPORTED_VERSIONS }
    async fn poll(&self, _: &PollContext) -> Result<PollOutcome, ConnectorError> {
        Ok(PollOutcome {
            events: vec![ConnectorEvent {
                event_id: ConnectorEventId::new("01HX0000000000000000000000"),
                connector: "evil".into(),
                source_ref: SourceRef::new("issue", "x", None),
                occurred_at: 0,
                labels: BTreeSet::from(["forbidden".to_string()]),
                scope: ConnectorScope::project("p"),
                payload: ConnectorPayload::Text { mime: "text/plain".into(), body: "".into() },
                delivery: DeliveryMode::Poll { cursor: None },
            }],
            next_cursor: None,
            rate_limit_hint: None,
        })
    }
    async fn ingest_webhook(&self, _: &WebhookRequest, _: &WebhookContext) -> Result<Vec<ConnectorEvent>, ConnectorError> { Ok(vec![]) }
}
impl ConnectorPlugin for EvilConnector {
    const NAME: &'static str = "evil";
    const SUPPORTED_VERSIONS: VersionRange =
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0));
}

const MANIFEST: &str = r#"
[connector]
name = "evil"
contract = "Connector"
contract_version = "0.1.0"
sensor_identity = "snr:remote:evil:v1"
[capabilities]
poll = true
webhook = false
backfill = false
[oauth]
required_scopes = []
token_lifetime = "1h"
refresh = false
[budget]
max_items_per_hour = 100
max_bytes_per_day = "1MiB"
[labels]
allowed = ["note"]
[[scopes.declared]]
kind = "project"
pattern = "*"
[webhook]
"signature.algorithm" = "hmac-sha256"
"signature.header" = "X-Sig"
allowed_mimes = ["application/json"]
[poll]
cursor_kind = "opaque-string"
min_interval = "30s"
default_interval = "5m"
[payload]
max_bytes = "1KiB"
max_depth = 4
"#;

struct PanicEmit;
#[async_trait::async_trait]
impl PipelineEmit for PanicEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        panic!("emit must NOT be called for undeclared-label event");
    }
}

#[tokio::test]
async fn undeclared_label_rejected_at_framework_boundary() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(PanicEmit) as Arc<dyn PipelineEmit>)
        .build();
    reg.register(EvilConnector::new()).unwrap();
    reg.enable("evil", default_grant()).await.unwrap();
    let err = reg.poll_now("evil").await.unwrap_err();
    assert!(matches!(err, ConnectorError::UndeclaredLabel { label } if label == "forbidden"));
    reg.shutdown().await;
}
```

Run: `cargo nextest run -p cairn-connectors-core --test undeclared_label_rejected --locked` → PASS.
Commit: `test(connectors-core): undeclared-label gate (#130)`.

### Task 20b — `consent_gate.rs`

```rust
use std::sync::Arc;

use cairn_connectors_core::{
    ConnectorRegistry, InMemoryCredentialStore, PipelineEmit, ConnectorError,
    fixture::{FixtureConnector, default_grant},
};
use cairn_core::contract::connector_consent::{ConnectorConsentJournal, ConsentGrant, ConsentGrantId, ConsentLookup};
use cairn_core::domain::capture::CaptureEvent;
use std::sync::Mutex;

#[derive(Default)]
struct DenyAllJournal;
#[async_trait::async_trait]
impl ConnectorConsentJournal for DenyAllJournal {
    async fn put_grant(&self, _: ConsentGrant) -> Result<ConsentGrantId, String> { Ok(ConsentGrantId::new("gnt:x")) }
    async fn lookup(&self, _: &str, _: &str) -> Result<ConsentLookup, String> { Ok(ConsentLookup::Revoked) }
    async fn revoke(&self, _: &ConsentGrantId) -> Result<(), String> { Ok(()) }
}

struct PanicEmit;
#[async_trait::async_trait]
impl PipelineEmit for PanicEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        panic!("emit must NOT be called when consent is revoked");
    }
}

#[tokio::test]
async fn consent_revoked_blocks_emit() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(DenyAllJournal))
        .emit(Arc::new(PanicEmit) as Arc<dyn PipelineEmit>)
        .build();
    reg.register(FixtureConnector::with_default_manifest()).unwrap();
    reg.enable("fixture", default_grant()).await.unwrap();
    let err = reg.poll_now("fixture").await.unwrap_err();
    assert!(matches!(err, ConnectorError::ConsentRevoked { connector } if connector == "fixture"));
    reg.shutdown().await;
}
```

Run + commit (`test(connectors-core): consent gate (#130)`).

### Task 20c — `disabled_no_emit.rs`

```rust
use std::sync::Arc;

use cairn_connectors_core::{
    ConnectorRegistry, InMemoryCredentialStore, PipelineEmit, ConnectorError,
    fixture::{AcceptAllConsent, FixtureConnector},
};
use cairn_core::domain::capture::CaptureEvent;

struct PanicEmit;
#[async_trait::async_trait]
impl PipelineEmit for PanicEmit {
    async fn emit(&self, _: CaptureEvent) -> Result<(), ConnectorError> {
        panic!("emit must NOT be called for a disabled connector");
    }
}

#[tokio::test]
async fn disabled_connector_does_not_emit() {
    let mut reg = ConnectorRegistry::builder()
        .credentials(Arc::new(InMemoryCredentialStore::default()))
        .consent(Arc::new(AcceptAllConsent::default()))
        .emit(Arc::new(PanicEmit) as Arc<dyn PipelineEmit>)
        .build();
    reg.register(FixtureConnector::with_default_manifest()).unwrap();
    // Intentionally do NOT call enable.
    // Drive a poll cycle directly — registry should refuse because state is Disabled.
    let err = reg.poll_now("fixture").await.unwrap_err();
    assert!(matches!(err, ConnectorError::Fatal(_)));
    reg.shutdown().await;
}
```

This requires `poll_now` to check state. Update `poll_now` body:

```rust
if matches!(**entry.state.load(), ConnectorState::Disabled) {
    return Err(ConnectorError::Fatal(anyhow::anyhow!("connector {name} disabled")));
}
```

Run + commit (`test(connectors-core): disabled connectors do not emit (#130)`).

### Task 20d — `rate_limit.rs`

```rust
use cairn_connectors_core::{ConnectorError, RateLimit};

#[test]
fn budget_exceeded_returns_typed_error() {
    let rl = RateLimit::per_hour("p1".into(), 2);
    rl.charge("p1", 1).unwrap();
    rl.charge("p1", 1).unwrap();
    assert!(matches!(
        rl.charge("p1", 1),
        Err(ConnectorError::BudgetExceeded { scope }) if scope == "p1",
    ));
}
```

Run + commit (`test(connectors-core): rate-limit budget exceeded (#130)`).

### Task 20e — `redaction.rs`

```rust
use std::collections::BTreeSet;

use cairn_connectors_core::event::*;
use cairn_connectors_core::redact::RedactionPipeline;

#[test]
fn email_in_json_leaf_records_span_and_masks_body() {
    let event = ConnectorEvent {
        event_id: ConnectorEventId::new("01HX0000000000000000000000"),
        connector: "fixture".into(),
        source_ref: SourceRef::new("issue", "x", None),
        occurred_at: 0,
        labels: BTreeSet::new(),
        scope: ConnectorScope::project("p"),
        payload: ConnectorPayload::Json {
            mime: "application/json".into(),
            body: serde_json::json!({"author": "alice@example.com"}),
        },
        delivery: DeliveryMode::Poll { cursor: None },
    };
    let r = RedactionPipeline::new().redact(event).unwrap();
    assert!(!r.spans.is_empty(), "expected redaction spans");
    let json = serde_json::to_string(&r.event).unwrap();
    assert!(!json.contains("alice@example.com"));
}
```

Run + commit (`test(connectors-core): redaction span coverage (#130)`).

### Task 20f — `oauth_lifecycle.rs`

```rust
use cairn_connectors_core::{ConnectorError, InMemoryCredentialStore, CredentialStore};
use cairn_connectors_core::webhook::{hex_hmac_sha256, verify_hmac_sha256, WebhookRequest};

#[tokio::test]
async fn missing_credential_returns_auth_expired() {
    let store = InMemoryCredentialStore::default();
    let err = store.get("absent").await.unwrap_err();
    assert!(matches!(err, ConnectorError::AuthExpired { scope } if scope == "absent"));
}

#[tokio::test]
async fn credential_round_trip_then_delete() {
    let store = InMemoryCredentialStore::default();
    store.put("s", b"v".to_vec()).await.unwrap();
    assert_eq!(store.get("s").await.unwrap().bytes(), b"v");
    store.delete("s").await.unwrap();
    assert!(store.get("s").await.is_err());
}

#[test]
fn signature_round_trip() {
    let secret = b"k";
    let body = b"payload";
    let sig = hex_hmac_sha256(secret, body);
    let req = WebhookRequest {
        connector: "f".into(),
        body: body.to_vec(),
        headers: vec![("X-Sig".into(), sig.clone())],
    };
    assert!(verify_hmac_sha256(&req, "X-Sig", secret).is_ok());

    let req_bad = WebhookRequest {
        connector: "f".into(),
        body: b"tampered".to_vec(),
        headers: vec![("X-Sig".into(), sig)],
    };
    assert!(matches!(
        verify_hmac_sha256(&req_bad, "X-Sig", secret),
        Err(ConnectorError::SignatureMismatch),
    ));
}
```

Run + commit (`test(connectors-core): OAuth + webhook signature lifecycle (#130)`).

### Task 20g — `manifest_validates.rs`

```rust
use cairn_connectors_core::manifest::ConnectorManifest;

const MANIFEST: &str = include_str!("../tests/fixtures/manifest.toml");

#[test]
fn snapshot_parsed_manifest() {
    let m = ConnectorManifest::parse_toml(MANIFEST).unwrap();
    insta::assert_yaml_snapshot!(m);
}
```

Create the fixture file `crates/cairn-connectors-core/tests/fixtures/manifest.toml` containing the same TOML used in T7's tests. Run `cargo insta test --accept -p cairn-connectors-core` once to materialize `.snap`, then commit both `.snap` and fixture (`test(connectors-core): manifest parse snapshot (#130)`).

---

## Task 21 — Verification sweep + docgen + supply-chain

**Files:** repo-wide

- [ ] **Step 1: Run the full verification checklist**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
cargo run -p cairn-cli --bin cairn-docgen --locked -- --check
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: all PASS. If `cargo-codegen --check` reports drift (likely from the new `ContractKind::Connector` or `SourceFamily::External`), rerun without `--check` to regenerate, then commit:

```bash
cargo run -p cairn-idl --bin cairn-codegen --locked
git add -- $(git status --short | awk '/^( M|A )/ {print $2}')
git commit -m "chore(idl): regenerate after Connector contract kind (#130)"
```

If `cargo-docgen --check` reports drift, rerun with `--write` and commit:

```bash
cargo run -p cairn-cli --bin cairn-docgen --locked -- --write
git add docs/site/src/reference/generated
git commit -m "docs(generated): refresh after connector framework (#130)"
```

- [ ] **Step 2: Push branch + open PR**

```bash
git push -u origin HEAD
gh pr create --title "feat: connector framework + OAuth/webhook contracts (#130)" --body "$(cat <<'EOF'
## Summary
- New `cairn-connectors-core` crate: `Connector` trait, `ConnectorEvent` envelope, `ConnectorManifest` (TOML + stable hash), `CredentialStore` (in-memory + keychain), `RedactionPipeline`, per-scope `RateLimit`, `WebhookRouter` (HMAC-SHA256), `PollScheduler` (tokio + cancel), `ConnectorRegistry` lifecycle, in-tree `FixtureConnector`.
- `cairn-core`: add `SourceFamily::External` + `CapturePayload::External`, `ContractKind::Connector`, `ConnectorConsentJournal` trait.
- Tests cover every acceptance criterion in issue #130 (contract, manifest snapshot, payload validation proptest, undeclared-label gate, consent gate, disabled-no-emit, rate-limit, redaction, OAuth lifecycle).

Closes #130. Sibling work: #131 (real adapters), #181 (Slack).

Brief sources: §9 Sensors, §19 v0.3 source connectors, §4.2 SensorIdentity, §14 consent.
Design spec: `docs/superpowers/specs/2026-05-24-issue-130-connector-framework-design.md`.

## Test plan
- [x] `cargo nextest run -p cairn-connectors-core --locked`
- [x] `cargo nextest run --workspace --locked --no-fail-fast`
- [x] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [x] `./scripts/check-core-boundary.sh`
- [x] `cargo run -p cairn-idl --bin cairn-codegen --locked -- --check`
- [x] `cargo run -p cairn-cli --bin cairn-docgen --locked -- --check`
- [x] `cargo deny check && cargo audit --deny warnings && cargo machete`
EOF
)"
```

---

## Self-review notes (post-write)

Coverage cross-check:

| Spec section | Plan task |
|---|---|
| §3 Crate layout & deps | T4 (scaffold), T1/T2/T3 (core edits) |
| §4 `Connector` trait | T8 |
| §5 `ConnectorEvent` + `ConnectorManifest` | T6, T7 |
| §6.1 pre-Capture pipeline diagram | T11 (redact) + T15 (emit) + T16/T18 (registry glue) |
| §6.3 `CapturePayload::External` | T1 |
| §6.4 registry lifecycle | T16 + T17 |
| §6.5 test plan (9 files) | T17 + T18 + T19 + T20a..g |
| §7 verification checklist | T21 |
| §8 out-of-scope items | Not implemented (correct) |

Open items to resolve during execution (already flagged inline, not blockers):

- Exact `cairn-keychain` API names — verify before T10.
- Exact `Identity::test_*` / `Rfc3339Timestamp::from_unix` / `PayloadHash::for_bytes` helper names — verify before T15. If a helper does not yet exist, add a small typed constructor in `cairn-core::domain` (preferred over inline string formatting).
- Whether `axum::Router` is the right router primitive for the registry — defer to first CLI integration in a follow-up issue; current substrate just stores axum::Routers without using them, which is fine for the issue's acceptance criteria.

No placeholders, no "implement appropriately," no "similar to Task N" without showing the code.
