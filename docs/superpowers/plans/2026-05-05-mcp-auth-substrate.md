# MCP Authorization Substrate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the prerequisite authorization substrate that issue #190 (MCP graph tools) blocks on — config schema, scope-resolution trait, capability-availability predicate, store-wired stdio entry point, CLI wiring, and `cairn status` integration — without exposing any new tools. The 8-verb manifest must be byte-identical for any deployment that has not opted into `single_tenant = true`.

**Architecture:** A new `cairn-core::mcp_auth` module defines `McpAuthContext`, `McpSessionScope`, `ScopeResolutionError`, `McpTransport`, `McpGraphAvailability`, and the `ConfigBackedScope` default impl — all pure (no I/O), so they live in `cairn-core` without breaching the dep boundary. A new `[mcp.stdio]` config block in `CairnConfig` carries `single_tenant: bool` + `principal: Option<ScopeTuple>` with a fail-closed validator. `CairnConfig::mcp_graph_tools_available(scope, transport, store_caps)` is the **single shared predicate** both `cairn status` and the (future) MCP `tools/list` consult — no split-brain. `cairn-mcp` gains `serve_stdio_with_store(store, scope, config, principal)`; the legacy `serve_stdio()` is deprecated to construct an unwired handler that returns the 8-verb manifest only. `cairn-cli::mcp` resolves config + opens the SQLite store + builds a `ConfigBackedScope` and calls the new entry point. `cairn-cli::verbs::status` reads the predicate and prints one of four states.

**Tech Stack:** Rust 1.95 (edition 2024, resolver 3), tokio multi-thread runtime for the long-lived MCP process, `serde` for config (de)serialization, `schemars` is **not** required for this PR (no new MCP tool input schemas land), `rmcp 1.6.0` (already pinned), `rusqlite`/`tokio_rusqlite` via `cairn-store-sqlite`, `thiserror` for library errors, `insta` for snapshot tests, `cargo nextest` as the test runner.

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `crates/cairn-core/src/mcp_auth/mod.rs` | **Create** | `McpAuthContext`, `McpTransport`, `McpGraphAvailability`, re-exports |
| `crates/cairn-core/src/mcp_auth/scope.rs` | **Create** | `McpSessionScope` trait, `ScopeResolutionError`, `ConfigBackedScope` impl |
| `crates/cairn-core/src/lib.rs` | **Modify** | add `pub mod mcp_auth;` |
| `crates/cairn-core/src/config/mod.rs` | **Modify** | add `McpConfig` + `McpStdioConfig` to `CairnConfig`; add `mcp_graph_tools_available` method; extend `ConfigError` with `McpStdioMissingPrincipal` |
| `crates/cairn-core/src/config/mcp.rs` | **Create** | `McpConfig`, `McpStdioConfig`, `validate_mcp_config()` |
| `crates/cairn-mcp/src/lib.rs` | **Modify** | add `serve_stdio_with_store`; deprecate `serve_stdio` (8-verb only) |
| `crates/cairn-mcp/src/handler.rs` | **Modify** | take `Arc<dyn McpSessionScope>` + `ScopeTuple` principal in `with_store`; `tools/list` always returns `TOOLS` (8 verbs) since no graph tools exist yet |
| `crates/cairn-mcp/Cargo.toml` | **Modify** | add `cairn-store-sqlite` as dev-dep for integration tests |
| `crates/cairn-mcp/tests/manifest_unconfigured.rs` | **Create** | integration test: unconfigured stdio → 8-verb manifest only |
| `crates/cairn-cli/src/mcp.rs` | **Modify** | open store, build `ConfigBackedScope`, dispatch to `serve_stdio_with_store` |
| `crates/cairn-cli/src/verbs/status.rs` | **Modify** | call `mcp_graph_tools_available` and emit one of the four states |
| `crates/cairn-cli/src/verbs/snapshots/status_mcp_graph_*.snap` | **Created by insta** | one snapshot per `McpGraphAvailability` variant |
| `crates/cairn-cli/Cargo.toml` | **Modify** | depend on `cairn-store-sqlite` (already present — verify) |

---

## Task 1: Add `mcp_auth` module skeleton in `cairn-core` (types only)

**Files:**
- Create: `crates/cairn-core/src/mcp_auth/mod.rs`
- Create: `crates/cairn-core/src/mcp_auth/scope.rs`
- Modify: `crates/cairn-core/src/lib.rs`
- Test: `crates/cairn-core/src/mcp_auth/mod.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing test**

Create `crates/cairn-core/src/mcp_auth/mod.rs` with the test block at the bottom (we will fill in the types in Step 3). For now, write the test against types that don't exist yet:

```rust
//! MCP authorization substrate: per-request auth context, scope-resolution
//! trait, transport tag, and the shared `mcp_graph_tools_available` enum.
//!
//! Pure types only — no I/O. Living in `cairn-core` keeps the dep boundary
//! intact: every other workspace crate may depend on these types.

pub mod scope;

pub use scope::{ConfigBackedScope, McpSessionScope, ScopeResolutionError};

use crate::domain::ScopeTuple;

/// Transport the MCP server is running. Drives the
/// `mcp_graph_tools_available` predicate's transport-precondition row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpTransport {
    /// Process-pair stdio. The only transport this PR ships.
    Stdio,
}

/// Per-request authorization context handed to a [`McpSessionScope`].
///
/// On stdio (this PR) `principal` is fixed at server-construction time and
/// `request_id` varies per call. Future SSE / HTTP transports will extend
/// the struct (still `#[non_exhaustive]`) with per-request principal
/// extraction from `RequestContext::extensions`.
#[derive(Debug)]
#[non_exhaustive]
pub struct McpAuthContext<'a> {
    /// The principal asserted for this request. On stdio, the
    /// server-construction-time scope tuple from `cairn.toml::[mcp.stdio]
    /// principal`.
    pub principal: &'a ScopeTuple,
    /// Opaque MCP request id, for diagnostics. Empty when called from
    /// `tools/list` pre-flight discovery.
    pub request_id: &'a str,
}

impl<'a> McpAuthContext<'a> {
    /// Construct a context. `request_id` may be empty for pre-flight
    /// discovery (e.g. `tools/list` invoked before any
    /// `tools/call`).
    #[must_use]
    pub fn new(principal: &'a ScopeTuple, request_id: &'a str) -> Self {
        Self {
            principal,
            request_id,
        }
    }
}

/// Result of asking "should this deployment list/call MCP graph tools?"
///
/// `cairn status` reports the discriminant; the (future) MCP handler reuses
/// the same enum to gate `tools/list` and `tools/call`. Both surfaces share
/// one code path so they cannot drift.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpGraphAvailability {
    /// Graph tools available; `tool_count` is the number of `graph.*`
    /// tools the manifest would expose. Plan A ships zero graph tools,
    /// so this variant is **not produced** by the current
    /// `mcp_graph_tools_available` body — Plan C will flip the body to
    /// return it once it lands the actual tools.
    Available {
        /// Number of `graph.*` tools listed when this state holds.
        tool_count: u8,
    },
    /// Stdio is not in `single_tenant = true` mode.
    UnavailableSingleTenantOff,
    /// Store does not advertise `graph_edges` capability.
    UnavailableNoStoreCapability,
    /// No `McpSessionScope` resolver is wired into the handler.
    UnavailableNoScopeResolver,
}

impl McpGraphAvailability {
    /// Stable kebab-case label for status output / snapshot tests.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Available { .. } => "available",
            Self::UnavailableSingleTenantOff => "unavailable:single-tenant-off",
            Self::UnavailableNoStoreCapability => "unavailable:no-store-capability",
            Self::UnavailableNoScopeResolver => "unavailable:no-scope-resolver",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ScopeTuple;

    #[test]
    fn auth_context_carries_principal_and_request_id() {
        let principal = ScopeTuple {
            tenant: Some("acme".into()),
            ..ScopeTuple::default()
        };
        let ctx = McpAuthContext::new(&principal, "req-42");
        assert_eq!(ctx.principal.tenant.as_deref(), Some("acme"));
        assert_eq!(ctx.request_id, "req-42");
    }

    #[test]
    fn graph_availability_labels_are_stable() {
        assert_eq!(
            McpGraphAvailability::Available { tool_count: 5 }.label(),
            "available",
        );
        assert_eq!(
            McpGraphAvailability::UnavailableSingleTenantOff.label(),
            "unavailable:single-tenant-off",
        );
        assert_eq!(
            McpGraphAvailability::UnavailableNoStoreCapability.label(),
            "unavailable:no-store-capability",
        );
        assert_eq!(
            McpGraphAvailability::UnavailableNoScopeResolver.label(),
            "unavailable:no-scope-resolver",
        );
    }
}
```

Also create the empty `scope.rs` placeholder (Task 2 fills it):

```rust
//! Scope resolution trait + default config-backed impl. Filled in Task 2.
```

Wire the module into `crates/cairn-core/src/lib.rs`. Find the existing `pub mod` declarations (e.g. `pub mod config;`, `pub mod contract;`) and add:

```rust
pub mod mcp_auth;
```

- [ ] **Step 2: Run tests; confirm failure**

```bash
cargo nextest run -p cairn-core --locked mcp_auth::tests
```

Expected: compile errors — `ScopeResolutionError`, `McpSessionScope`, `ConfigBackedScope` are referenced in `pub use` but not defined yet (placeholder `scope.rs` is empty).

- [ ] **Step 3: Make tests compile by stubbing the trait re-exports**

Edit `crates/cairn-core/src/mcp_auth/scope.rs`:

```rust
//! Scope resolution trait + default config-backed impl.

use crate::domain::ScopeTuple;
use crate::mcp_auth::McpAuthContext;

/// Errors a [`McpSessionScope`] resolver may surface.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ScopeResolutionError {
    /// The resolver was unable to identify the caller (e.g. no
    /// principal extractable from a transport that requires one).
    #[error("scope resolver could not identify caller")]
    Unidentified,
    /// The deployment is configured but the active configuration
    /// does not authorise any record scope for this caller.
    #[error("scope resolver returned an empty allowed-scope set")]
    EmptyScope,
    /// Resolver-specific failure (e.g. registry lookup failed).
    #[error("scope resolver failed: {reason}")]
    Other {
        /// Human-readable description of the failure mode.
        reason: String,
    },
}

/// Resolve the requesting MCP session's allowed record scope ids.
///
/// Stdio implementations may ignore `ctx.request_id` and return a fixed
/// vector — that is honest single-tenant behaviour, not a per-caller
/// isolation claim. Future per-request-identity transports extract the
/// principal from `ctx` and key the resolution off it.
pub trait McpSessionScope: Send + Sync {
    /// Resolve allowed scopes for this request.
    ///
    /// # Errors
    /// Returns [`ScopeResolutionError`] when the caller cannot be
    /// identified or when resolution itself fails. **An empty `Vec` is
    /// fail-closed: the (future) tool layer treats it the same as
    /// `Err(_)` — `CapabilityUnavailable`.**
    fn allowed_scopes(
        &self,
        ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError>;
}

/// Default scope resolver: returns the deployment's configured principal
/// as the single allowed scope on every call.
///
/// Honest single-tenant: every request gets the same `Vec<ScopeTuple>`
/// (length 1). The MCP handler still re-invokes the resolver per request,
/// so swapping in a per-caller resolver later is a drop-in change.
#[derive(Debug, Clone)]
pub struct ConfigBackedScope {
    principal: ScopeTuple,
}

impl ConfigBackedScope {
    /// Construct from the principal carried in
    /// `cairn.toml::[mcp.stdio] principal`.
    #[must_use]
    pub fn new(principal: ScopeTuple) -> Self {
        Self { principal }
    }

    /// Borrow the configured principal.
    #[must_use]
    pub fn principal(&self) -> &ScopeTuple {
        &self.principal
    }
}

impl McpSessionScope for ConfigBackedScope {
    fn allowed_scopes(
        &self,
        _ctx: &McpAuthContext<'_>,
    ) -> Result<Vec<ScopeTuple>, ScopeResolutionError> {
        Ok(vec![self.principal.clone()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_backed_scope_returns_single_principal_per_call() {
        let principal = ScopeTuple {
            tenant: Some("acme".into()),
            workspace: Some("eng".into()),
            ..ScopeTuple::default()
        };
        let scope = ConfigBackedScope::new(principal.clone());
        let ctx_principal = principal.clone();
        let ctx = McpAuthContext::new(&ctx_principal, "req-1");

        let resolved = scope
            .allowed_scopes(&ctx)
            .expect("ConfigBackedScope never errors");
        assert_eq!(resolved, vec![principal]);
    }

    #[test]
    fn config_backed_scope_ignores_request_id() {
        // Honest single-tenant: request_id varies, scope does not.
        let principal = ScopeTuple {
            tenant: Some("a".into()),
            ..ScopeTuple::default()
        };
        let scope = ConfigBackedScope::new(principal.clone());
        let ctx_a = McpAuthContext::new(&principal, "req-a");
        let ctx_b = McpAuthContext::new(&principal, "req-b");
        assert_eq!(
            scope.allowed_scopes(&ctx_a).unwrap(),
            scope.allowed_scopes(&ctx_b).unwrap(),
        );
    }
}
```

- [ ] **Step 4: Run tests; confirm pass**

```bash
cargo nextest run -p cairn-core --locked mcp_auth
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

Expected: 4 tests pass (2 in `mod.rs::tests`, 2 in `scope.rs::tests`); clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/mcp_auth/ crates/cairn-core/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(core): add mcp_auth substrate types (issue #190 prereq)

McpAuthContext, McpSessionScope, ScopeResolutionError, ConfigBackedScope,
McpTransport, McpGraphAvailability. Pure types — no I/O — so they live
in cairn-core without breaching the dep boundary. Plan A scope: no graph
tools land yet; this is the substrate the (future) handler will read.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Add `[mcp.stdio]` config schema with fail-closed validation

**Files:**
- Create: `crates/cairn-core/src/config/mcp.rs`
- Modify: `crates/cairn-core/src/config/mod.rs`
- Test: `crates/cairn-core/src/config/mcp.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write failing test**

Create `crates/cairn-core/src/config/mcp.rs`:

```rust
//! `[mcp]` config block (issue #190 prereq).
//!
//! `[mcp.stdio]` carries the single-tenant opt-in flag and the principal
//! that `ConfigBackedScope` returns. Defaults are fail-closed:
//! `single_tenant = false` means MCP graph tools are unavailable on this
//! deployment, no principal needed.

use serde::{Deserialize, Serialize};

use crate::domain::ScopeTuple;

/// `[mcp]` section. Currently only the stdio sub-block exists.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Per-transport stdio configuration.
    pub stdio: McpStdioConfig,
}

/// `[mcp.stdio]` section.
///
/// Defaults: `single_tenant = false`, `principal = None`. A deployment that
/// does not enable single-tenant mode never advertises graph tools and
/// never needs a principal — the configuration round-trips cleanly with no
/// `[mcp.stdio]` block at all.
///
/// Validation: if `single_tenant = true` then `principal` MUST be `Some`.
/// `validate_mcp_config` returns
/// [`ConfigError::McpStdioMissingPrincipal`](super::ConfigError::McpStdioMissingPrincipal)
/// otherwise.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpStdioConfig {
    /// Operator opt-in: this stdio process serves exactly one principal
    /// for its entire lifetime. The construction-time `principal` is the
    /// only configuration where it is a faithful authorization key
    /// (spec §2.1.1).
    pub single_tenant: bool,
    /// Principal returned by `ConfigBackedScope::allowed_scopes` on every
    /// request. Required when `single_tenant = true`; ignored otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<ScopeTuple>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_fail_closed() {
        let cfg = McpStdioConfig::default();
        assert!(!cfg.single_tenant, "default: deny");
        assert!(cfg.principal.is_none(), "default: no principal");
    }

    #[test]
    fn deserialize_omitted_block_yields_default() {
        let yaml = "";
        let cfg: McpConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg, McpConfig::default());
    }

    #[test]
    fn deserialize_single_tenant_with_principal_round_trips() {
        let yaml = r#"
stdio:
  single_tenant: true
  principal:
    tenant: acme
    workspace: eng
"#;
        let cfg: McpConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(cfg.stdio.single_tenant);
        let p = cfg.stdio.principal.as_ref().unwrap();
        assert_eq!(p.tenant.as_deref(), Some("acme"));
        assert_eq!(p.workspace.as_deref(), Some("eng"));
    }

    #[test]
    fn deserialize_rejects_unknown_keys() {
        let yaml = r#"
stdio:
  single_tenant: true
  unknown_field: oops
"#;
        let err = serde_yaml::from_str::<McpConfig>(yaml).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown_field") || msg.contains("unknown field"),
            "expected unknown-field rejection, got: {msg}",
        );
    }
}
```

Now extend `crates/cairn-core/src/config/mod.rs`. Find the `ConfigError` enum (around line 17) and add a new variant:

```rust
    /// `[mcp.stdio] single_tenant = true` was set but no `principal` was
    /// provided.
    #[error(
        "[mcp.stdio] single_tenant = true requires a `principal` scope tuple"
    )]
    McpStdioMissingPrincipal,
```

Then add a module declaration near the top of `config/mod.rs` (next to `pub mod vault_registry;`):

```rust
pub mod mcp;
pub use mcp::{McpConfig, McpStdioConfig};
```

Add a field to `CairnConfig` (around line 391). Locate the struct and append:

```rust
    /// MCP transport configuration (issue #190).
    pub mcp: McpConfig,
```

Add a free function `validate_mcp_config` next to the new module use (in `config/mod.rs` after the `pub use mcp::{...};` line):

```rust
/// Validate `[mcp.*]` invariants beyond what serde alone enforces.
///
/// # Errors
/// Returns [`ConfigError::McpStdioMissingPrincipal`] when
/// `[mcp.stdio] single_tenant = true` is set without a `principal`.
pub fn validate_mcp_config(cfg: &McpConfig) -> Result<(), ConfigError> {
    if cfg.stdio.single_tenant && cfg.stdio.principal.is_none() {
        return Err(ConfigError::McpStdioMissingPrincipal);
    }
    Ok(())
}
```

Add a `validate` method on `CairnConfig` (or extend the existing one — search for `impl CairnConfig` around line 715). Append to the `impl CairnConfig` block:

```rust
    /// Run cross-section invariants that serde alone cannot express.
    ///
    /// Currently checks:
    /// - `[mcp.stdio] single_tenant + principal` consistency
    ///   ([`validate_mcp_config`]).
    ///
    /// Existing validators (pipeline, retention, etc.) keep their own
    /// entry points; this method composes the new MCP check without
    /// disturbing them.
    ///
    /// # Errors
    /// Returns the first [`ConfigError`] encountered.
    pub fn validate_mcp(&self) -> Result<(), ConfigError> {
        validate_mcp_config(&self.mcp)
    }
```

In the same file, add a unit test next to the existing config tests (find `#[cfg(test)] mod tests`):

```rust
    #[test]
    fn validate_mcp_rejects_single_tenant_without_principal() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = None;
        let err = cfg.validate_mcp().unwrap_err();
        assert!(
            matches!(err, ConfigError::McpStdioMissingPrincipal),
            "got: {err:?}",
        );
    }

    #[test]
    fn validate_mcp_accepts_single_tenant_with_principal() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(crate::domain::ScopeTuple {
            tenant: Some("acme".into()),
            ..crate::domain::ScopeTuple::default()
        });
        cfg.validate_mcp().expect("valid config");
    }

    #[test]
    fn validate_mcp_accepts_default_config() {
        // Default: single_tenant = false, principal = None — cleanly valid.
        let cfg = CairnConfig::default();
        cfg.validate_mcp().expect("default config is valid");
    }
```

- [ ] **Step 2: Run tests; confirm failure**

```bash
cargo nextest run -p cairn-core --locked config::mcp config::tests::validate_mcp
```

Expected: compile failure on first run if module wiring is incomplete; once compiling, the four `config::mcp::tests::*` tests run plus the three new `config::tests::validate_mcp_*` tests. The first invocation of the new validator will produce the right behaviour, but if anything is mis-wired (e.g. forgetting `pub use mcp::...`) the test run fails to build — fix compile errors before claiming pass.

- [ ] **Step 3: Implementation already provided in Step 1**

Step 1 contains the complete implementation. Re-read `crates/cairn-core/src/config/mod.rs` to verify all four edits landed (module decl, `pub use`, `CairnConfig` field, free function, validator method, tests).

- [ ] **Step 4: Run tests; confirm pass**

```bash
cargo nextest run -p cairn-core --locked config
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

Expected: all `config::mcp::tests::*` and the three new `config::tests::validate_mcp_*` tests pass; clippy clean. **Existing config tests must also still pass** — adding `mcp: McpConfig` defaults to `McpConfig::default()` (empty), so no existing serde round-trip is disturbed.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/config/
git commit -m "$(cat <<'EOF'
feat(config): add [mcp.stdio] section with fail-closed validation

Adds CairnConfig::mcp: McpConfig and McpStdioConfig with
single_tenant + principal fields. Defaults are deny: single_tenant=false
needs no principal. validate_mcp_config rejects single_tenant=true
with no principal (ConfigError::McpStdioMissingPrincipal).

Issue #190 prerequisite (Plan A).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Add `mcp_graph_tools_available` predicate to `CairnConfig`

**Files:**
- Modify: `crates/cairn-core/src/config/mod.rs`
- Test: `crates/cairn-core/src/config/mod.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write failing test**

Append to the existing `#[cfg(test)] mod tests` block in `crates/cairn-core/src/config/mod.rs`:

```rust
    use crate::contract::memory_store::MemoryStoreCapabilities;
    use crate::mcp_auth::{
        ConfigBackedScope, McpGraphAvailability, McpSessionScope, McpTransport,
    };

    fn store_caps_with_graph(graph: bool) -> MemoryStoreCapabilities {
        MemoryStoreCapabilities {
            fts: true,
            vector: false,
            graph_edges: graph,
            transactions: true,
            per_record_consent_model: true,
        }
    }

    fn principal_acme() -> crate::domain::ScopeTuple {
        crate::domain::ScopeTuple {
            tenant: Some("acme".into()),
            ..crate::domain::ScopeTuple::default()
        }
    }

    #[test]
    fn graph_tools_unavailable_when_single_tenant_off() {
        let cfg = CairnConfig::default(); // single_tenant defaults to false
        let scope = ConfigBackedScope::new(principal_acme());
        let caps = store_caps_with_graph(true);
        let s: &dyn McpSessionScope = &scope;
        let avail = cfg.mcp_graph_tools_available(Some(s), McpTransport::Stdio, &caps);
        assert_eq!(avail, McpGraphAvailability::UnavailableSingleTenantOff);
    }

    #[test]
    fn graph_tools_unavailable_when_no_scope_resolver() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal_acme());
        let caps = store_caps_with_graph(true);
        let avail = cfg.mcp_graph_tools_available(None, McpTransport::Stdio, &caps);
        assert_eq!(avail, McpGraphAvailability::UnavailableNoScopeResolver);
    }

    #[test]
    fn graph_tools_unavailable_when_store_lacks_graph_capability() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal_acme());
        let scope = ConfigBackedScope::new(principal_acme());
        let caps = store_caps_with_graph(false);
        let s: &dyn McpSessionScope = &scope;
        let avail = cfg.mcp_graph_tools_available(Some(s), McpTransport::Stdio, &caps);
        assert_eq!(avail, McpGraphAvailability::UnavailableNoStoreCapability);
    }

    #[test]
    fn graph_tools_substrate_ready_state_does_not_emit_available_in_plan_a() {
        // Plan A ships zero graph tools. Even with every precondition met,
        // the predicate must NOT return Available — Plan C will flip the
        // body once it lands the actual tools. This test pins the
        // contract-version invariant: do not over-advertise.
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal_acme());
        let scope = ConfigBackedScope::new(principal_acme());
        let caps = store_caps_with_graph(true);
        let s: &dyn McpSessionScope = &scope;
        let avail = cfg.mcp_graph_tools_available(Some(s), McpTransport::Stdio, &caps);
        // Plan A: deliberately falls through to a non-Available variant.
        // The exact variant is `UnavailableNoStoreCapability` — see
        // §6 of the design spec: "until graph tool wiring lands, the
        // predicate falls through to the most specific unavailable
        // state." We pin that here so Plan C's flip is a one-line
        // change with a single failing test.
        assert!(
            !matches!(avail, McpGraphAvailability::Available { .. }),
            "Plan A must not emit Available; got {:?}",
            avail,
        );
    }
```

- [ ] **Step 2: Run tests; confirm failure**

```bash
cargo nextest run -p cairn-core --locked config::tests::graph_tools
```

Expected: compile errors — `CairnConfig::mcp_graph_tools_available` does not exist.

- [ ] **Step 3: Implement the predicate**

Append to `impl CairnConfig` in `crates/cairn-core/src/config/mod.rs` (after `validate_mcp`):

```rust
    /// Single shared predicate that gates `cairn status` MCP-graph
    /// reporting today and (in Plan C) the MCP `tools/list` /
    /// `tools/call` graph-tool advertisement. Both surfaces read the same
    /// function so they cannot drift.
    ///
    /// **Plan A behaviour:** zero graph tools exist, so the predicate
    /// never returns [`McpGraphAvailability::Available`]. The deliberate
    /// fall-through order (most-specific reason wins) is:
    ///
    /// 1. `single_tenant == false` → `UnavailableSingleTenantOff`
    /// 2. else if `scope.is_none()` → `UnavailableNoScopeResolver`
    /// 3. else if `!store_caps.graph_edges` → `UnavailableNoStoreCapability`
    /// 4. else (Plan C will return `Available { tool_count: 5 }`) →
    ///    Plan A returns `UnavailableNoStoreCapability` as the
    ///    fall-through, because Plan A's store wiring does not advertise
    ///    graph tools to MCP yet — see the design spec §6.
    ///
    /// The `transport` argument is currently always `Stdio` and the body
    /// branches only on `Stdio`. Future SSE / HTTP transports add their
    /// own branches with their own per-transport preconditions.
    #[must_use]
    pub fn mcp_graph_tools_available(
        &self,
        scope: Option<&dyn crate::mcp_auth::McpSessionScope>,
        transport: crate::mcp_auth::McpTransport,
        store_caps: &crate::contract::memory_store::MemoryStoreCapabilities,
    ) -> crate::mcp_auth::McpGraphAvailability {
        use crate::mcp_auth::{McpGraphAvailability, McpTransport};

        match transport {
            McpTransport::Stdio => {
                if !self.mcp.stdio.single_tenant {
                    return McpGraphAvailability::UnavailableSingleTenantOff;
                }
                if scope.is_none() {
                    return McpGraphAvailability::UnavailableNoScopeResolver;
                }
                if !store_caps.graph_edges {
                    return McpGraphAvailability::UnavailableNoStoreCapability;
                }
                // Plan A: no graph tools land in this PR. The substrate
                // is wired but the manifest stays at 8 verbs. Plan C
                // flips this fall-through to
                // `Available { tool_count: 5 }` in a one-line change.
                McpGraphAvailability::UnavailableNoStoreCapability
            }
        }
    }
```

- [ ] **Step 4: Run tests; confirm pass**

```bash
cargo nextest run -p cairn-core --locked config::tests::graph_tools
cargo clippy -p cairn-core --all-targets --locked -- -D warnings
```

Expected: 4 tests pass; clippy clean (the `match transport` on a single-variant enum may warn — `McpTransport` is `#[non_exhaustive]`, so the match stays exhaustive-by-design and clippy will not raise `single_match_else`).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/config/mod.rs
git commit -m "$(cat <<'EOF'
feat(config): add CairnConfig::mcp_graph_tools_available predicate

Single shared predicate that cairn status (today) and the future MCP
tools/list dispatch will consult. Plan A returns one of the three
Unavailable variants; Plan C flips the fall-through to Available when
graph tools actually land. Fall-through order: single_tenant -> scope
resolver presence -> store capability.

Issue #190 prerequisite (Plan A).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add `serve_stdio_with_store`; deprecate `serve_stdio`

**Files:**
- Modify: `crates/cairn-mcp/src/lib.rs`
- Modify: `crates/cairn-mcp/src/handler.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-mcp/src/handler.rs` (in the existing `#[cfg(test)] mod tests` block — if none exists, create it at the bottom of the file):

```rust
#[cfg(test)]
mod tests_plan_a {
    use super::*;
    use std::sync::Arc;

    use cairn_core::config::CairnConfig;
    use cairn_core::domain::ScopeTuple;
    use cairn_core::mcp_auth::{ConfigBackedScope, McpSessionScope};
    use cairn_test_fixtures::FixtureStore;

    fn principal() -> ScopeTuple {
        ScopeTuple {
            tenant: Some("acme".into()),
            ..ScopeTuple::default()
        }
    }

    #[test]
    fn handler_with_store_carries_scope_and_principal() {
        let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
            Arc::new(FixtureStore::default());
        let scope: Arc<dyn McpSessionScope> =
            Arc::new(ConfigBackedScope::new(principal()));
        let cfg = CairnConfig::default();

        let handler =
            CairnMcpHandler::with_store_and_scope(store, scope, cfg, principal());
        // store + scope + principal all wired
        assert!(handler.has_store(), "store wired");
        assert!(handler.has_scope(), "scope wired");
        assert_eq!(handler.principal().tenant.as_deref(), Some("acme"));
    }

    #[test]
    fn manifest_without_graph_tools_in_plan_a() {
        // Even with every precondition met, Plan A must not list graph
        // tools — the manifest must equal the 8-verb TOOLS slice exactly.
        let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
            Arc::new(FixtureStore::default());
        let scope: Arc<dyn McpSessionScope> =
            Arc::new(ConfigBackedScope::new(principal()));
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal());

        let handler =
            CairnMcpHandler::with_store_and_scope(store, scope, cfg, principal());
        let listed = handler.listed_tool_names();
        assert_eq!(
            listed.len(),
            crate::generated::TOOLS.len(),
            "Plan A: no graph tools added to the manifest",
        );
        for tool in listed {
            assert!(
                !tool.starts_with("graph."),
                "Plan A must not list any graph.* tool, got `{tool}`",
            );
        }
    }
}
```

- [ ] **Step 2: Run tests; confirm failure**

```bash
cargo nextest run -p cairn-mcp --locked handler::tests_plan_a
```

Expected: compile errors — `with_store_and_scope`, `has_store`, `has_scope`, `principal`, `listed_tool_names` do not exist.

- [ ] **Step 3: Extend the handler and entry point**

Edit `crates/cairn-mcp/src/handler.rs`. Replace the existing `pub struct CairnMcpHandler { store, config }` with:

```rust
pub struct CairnMcpHandler {
    store: Option<Arc<dyn MemoryStore>>,
    scope: Option<Arc<dyn cairn_core::mcp_auth::McpSessionScope>>,
    config: CairnConfig,
    principal: cairn_core::domain::ScopeTuple,
}
```

Update `Default::default` and `new` to construct with `scope = None` and `principal = ScopeTuple::default()`:

```rust
impl CairnMcpHandler {
    /// Create an unwired handler. `tools/list` returns the 8-verb manifest;
    /// `tools/call` falls back to the stub. No scope resolver, no
    /// principal — graph-tool gating consequently stays in the
    /// `UnavailableNoScopeResolver` state in `cairn status`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            store: None,
            scope: None,
            config: CairnConfig::default(),
            principal: cairn_core::domain::ScopeTuple::default(),
        }
    }

    /// Create a handler wired to a real store but **no** scope resolver.
    /// Retained for back-compat with callers that have not been migrated
    /// to `with_store_and_scope` yet.
    #[must_use]
    pub fn with_store(store: Arc<dyn MemoryStore>, config: CairnConfig) -> Self {
        Self {
            store: Some(store),
            scope: None,
            config,
            principal: cairn_core::domain::ScopeTuple::default(),
        }
    }

    /// Plan A constructor: wires store + scope resolver + principal.
    ///
    /// `principal` is the same `ScopeTuple` the resolver was constructed
    /// from (typically `ConfigBackedScope`). It is stored separately so
    /// `tools/call` can build an `McpAuthContext` without down-casting the
    /// `dyn McpSessionScope`.
    #[must_use]
    pub fn with_store_and_scope(
        store: Arc<dyn MemoryStore>,
        scope: Arc<dyn cairn_core::mcp_auth::McpSessionScope>,
        config: CairnConfig,
        principal: cairn_core::domain::ScopeTuple,
    ) -> Self {
        Self {
            store: Some(store),
            scope: Some(scope),
            config,
            principal,
        }
    }

    /// True when a `MemoryStore` has been wired in.
    #[must_use]
    pub fn has_store(&self) -> bool {
        self.store.is_some()
    }

    /// True when an `McpSessionScope` resolver has been wired in.
    #[must_use]
    pub fn has_scope(&self) -> bool {
        self.scope.is_some()
    }

    /// Borrow the configured principal (or `ScopeTuple::default()` for
    /// unwired handlers).
    #[must_use]
    pub fn principal(&self) -> &cairn_core::domain::ScopeTuple {
        &self.principal
    }

    /// Names of every tool currently listed by `tools/list`.
    ///
    /// Plan A: always equals the 8-verb `TOOLS` slice. Plan C will
    /// concat graph tools when `mcp_graph_tools_available` returns
    /// `Available { tool_count }`.
    #[must_use]
    pub fn listed_tool_names(&self) -> Vec<String> {
        crate::generated::TOOLS
            .iter()
            .map(|t| t.name.to_string())
            .collect()
    }
}
```

In `Cargo.toml` (`crates/cairn-mcp/Cargo.toml`) the `[dev-dependencies]` already include `cairn-test-fixtures`. No change needed.

Now edit `crates/cairn-mcp/src/lib.rs` to add the new entry point and deprecate the old one:

```rust
/// Plan A entry point: serve MCP over stdio with a wired store, scope
/// resolver, and principal.
///
/// This is the entry point `cairn mcp` uses in Plan A. It blocks until
/// stdin closes, the same as [`serve_stdio`].
///
/// # Errors
/// Same shape as [`serve_stdio`]: returns
/// [`TransportError::Service`] on rmcp init / shutdown failure. Domain
/// errors surface inside the protocol as `CallToolResult { is_error: true }`.
pub async fn serve_stdio_with_store(
    store: std::sync::Arc<dyn cairn_core::contract::memory_store::MemoryStore>,
    scope: std::sync::Arc<dyn cairn_core::mcp_auth::McpSessionScope>,
    config: cairn_core::config::CairnConfig,
    principal: cairn_core::domain::ScopeTuple,
) -> Result<(), TransportError> {
    let handler =
        CairnMcpHandler::with_store_and_scope(store, scope, config, principal);
    let transport = rmcp::transport::io::stdio();
    let service = handler
        .serve(transport)
        .await
        .map_err(|e| TransportError::Service(e.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|e| TransportError::Service(e.to_string()))?;
    Ok(())
}
```

Mark the old `serve_stdio` deprecated (do **not** remove — Plan A keeps it as the unwired fallback used by integration tests and any caller that has not been migrated):

```rust
/// Start an MCP server on the process's own stdin / stdout — UNWIRED.
///
/// Returns the 8-verb manifest only. No store, no scope, no principal:
/// `tools/call` falls back to the stub for every verb. New callers
/// MUST use [`serve_stdio_with_store`].
///
/// # Errors
/// Same shape as [`serve_stdio_with_store`].
#[deprecated(
    since = "0.1.0",
    note = "use serve_stdio_with_store; serve_stdio returns the 8-verb manifest \
            with no store wiring and is retained only for unwired-fallback callers"
)]
pub async fn serve_stdio() -> Result<(), TransportError> {
    let handler = CairnMcpHandler::new();
    let transport = rmcp::transport::io::stdio();
    let service = handler
        .serve(transport)
        .await
        .map_err(|e| TransportError::Service(e.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|e| TransportError::Service(e.to_string()))?;
    Ok(())
}
```

- [ ] **Step 4: Run tests; confirm pass**

```bash
cargo nextest run -p cairn-mcp --locked
cargo clippy -p cairn-mcp --all-targets --locked -- -D warnings
```

Expected: the two new `tests_plan_a` tests pass plus existing handler tests; clippy clean. The `#[deprecated]` on `serve_stdio` will trigger a warning **at the call site**, not in `cairn-mcp` itself — Task 5 fixes the call site in `cairn-cli`.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-mcp/src/lib.rs crates/cairn-mcp/src/handler.rs
git commit -m "$(cat <<'EOF'
feat(mcp): add serve_stdio_with_store; deprecate unwired serve_stdio

CairnMcpHandler::with_store_and_scope wires a MemoryStore, a
McpSessionScope, and the principal. Plan A keeps tools/list at the
8-verb TOOLS slice — listed_tool_names() asserts no graph.* tools
land yet. serve_stdio retains its unwired body but is #[deprecated];
new callers route through serve_stdio_with_store.

Issue #190 prerequisite (Plan A).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: CLI wiring — open store, build scope, dispatch

**Files:**
- Modify: `crates/cairn-cli/src/mcp.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-cli/src/mcp.rs` an inline test that exercises the resolver-construction logic without actually starting the rmcp server (the actual stdio loop is tested via the integration test in Task 7):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::config::CairnConfig;
    use cairn_core::domain::ScopeTuple;

    fn principal() -> ScopeTuple {
        ScopeTuple {
            tenant: Some("acme".into()),
            ..ScopeTuple::default()
        }
    }

    #[test]
    fn resolves_no_scope_when_single_tenant_off() {
        let cfg = CairnConfig::default();
        let resolved = resolve_scope_components(&cfg);
        assert!(
            resolved.is_none(),
            "single_tenant = false (default): no scope resolver",
        );
    }

    #[test]
    fn resolves_scope_when_single_tenant_on_with_principal() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal());
        let resolved = resolve_scope_components(&cfg)
            .expect("single_tenant + principal: resolver constructed");
        assert_eq!(resolved.principal.tenant.as_deref(), Some("acme"));
    }

    #[test]
    fn refuses_to_resolve_when_validation_fails() {
        // single_tenant = true but no principal → validate_mcp rejects.
        // resolve_scope_components must surface None (the caller maps
        // validation failure to EX_CONFIG before reaching this fn, but
        // test the branch defensively).
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = None;
        assert!(resolve_scope_components(&cfg).is_none());
    }
}
```

- [ ] **Step 2: Run tests; confirm failure**

```bash
cargo nextest run -p cairn-cli --locked mcp::tests
```

Expected: compile errors — `resolve_scope_components`, `ResolvedMcpScope` do not exist.

- [ ] **Step 3: Rewrite `crates/cairn-cli/src/mcp.rs`**

Replace the entire file:

```rust
//! `cairn mcp` subcommand — drives the MCP stdio transport with a wired
//! `MemoryStore`, scope resolver, and principal (issue #190 Plan A).
//!
//! Creates a dedicated multi-thread tokio runtime (the MCP server is
//! long-lived) and blocks until stdin closes or the client sends a
//! shutdown notification.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::domain::ScopeTuple;
use cairn_core::mcp_auth::{ConfigBackedScope, McpSessionScope};

/// Outcome of resolving the `[mcp.stdio]` block into runtime components.
///
/// `None` means "no scope resolver" — graph tools stay
/// `UnavailableNoScopeResolver`. The CLI passes the resolver as `Option`
/// directly; this struct is just a convenient pair when one IS available.
pub struct ResolvedMcpScope {
    /// Scope resolver to feed into `serve_stdio_with_store`.
    pub resolver: Arc<dyn McpSessionScope>,
    /// Principal (cloned from config) for the auth context.
    pub principal: ScopeTuple,
}

/// Build the scope-resolver pair from config, if `[mcp.stdio]` opts in.
///
/// Returns `None` when:
/// - `single_tenant = false` (default), or
/// - `single_tenant = true` but `principal` is missing (the caller should
///   have already failed config validation, but we mirror the
///   fail-closed branch here for defense in depth).
#[must_use]
pub fn resolve_scope_components(config: &CairnConfig) -> Option<ResolvedMcpScope> {
    if !config.mcp.stdio.single_tenant {
        return None;
    }
    let principal = config.mcp.stdio.principal.clone()?;
    let resolver: Arc<dyn McpSessionScope> =
        Arc::new(ConfigBackedScope::new(principal.clone()));
    Some(ResolvedMcpScope {
        resolver,
        principal,
    })
}

/// Run the MCP stdio server.
///
/// Blocks until the MCP client closes stdin or sends a shutdown
/// notification. Exit codes:
/// - `0` — clean shutdown
/// - `69` (`EX_UNAVAILABLE`) — transport startup or I/O failure
/// - `78` (`EX_CONFIG`) — `[mcp.stdio]` validation failed
///
/// `vault_root` and `config` come from the CLI's resolution layer
/// (see `main.rs`). When the store cannot be opened (`vault_root` is
/// unbound or the SQLite file is missing), the function logs and exits
/// `EX_UNAVAILABLE` — matching `cairn search`/`cairn ingest` behaviour.
#[must_use]
pub fn run(vault_root: &Path, config: CairnConfig) -> ExitCode {
    // Fail closed on bad config before touching the store.
    if let Err(e) = config.validate_mcp() {
        eprintln!("cairn mcp: config error — {e}");
        return ExitCode::from(78); // EX_CONFIG
    }

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn mcp: failed to build tokio runtime: {e}");
            return ExitCode::from(69); // EX_UNAVAILABLE
        }
    };

    let store = match cairn_store_sqlite::SqliteMemoryStore::open(
        &vault_root.join(".cairn/cairn.db"),
    ) {
        Ok(s) => Arc::new(s)
            as Arc<dyn cairn_core::contract::memory_store::MemoryStore>,
        Err(e) => {
            eprintln!("cairn mcp: failed to open SQLite store: {e}");
            return ExitCode::from(69); // EX_UNAVAILABLE
        }
    };

    let result = match resolve_scope_components(&config) {
        Some(ResolvedMcpScope {
            resolver,
            principal,
        }) => rt.block_on(cairn_mcp::serve_stdio_with_store(
            store, resolver, config, principal,
        )),
        None => {
            // No `[mcp.stdio] single_tenant = true` opt-in: serve the
            // 8-verb manifest only via the deprecated unwired path.
            // This is intentional — see Plan A scope.
            #[allow(deprecated)]
            {
                rt.block_on(cairn_mcp::serve_stdio())
            }
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cairn mcp: {e:#}");
            ExitCode::from(69) // EX_UNAVAILABLE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::config::CairnConfig;
    use cairn_core::domain::ScopeTuple;

    fn principal() -> ScopeTuple {
        ScopeTuple {
            tenant: Some("acme".into()),
            ..ScopeTuple::default()
        }
    }

    #[test]
    fn resolves_no_scope_when_single_tenant_off() {
        let cfg = CairnConfig::default();
        let resolved = resolve_scope_components(&cfg);
        assert!(resolved.is_none());
    }

    #[test]
    fn resolves_scope_when_single_tenant_on_with_principal() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(principal());
        let resolved = resolve_scope_components(&cfg).unwrap();
        assert_eq!(resolved.principal.tenant.as_deref(), Some("acme"));
    }

    #[test]
    fn refuses_to_resolve_when_validation_fails() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = None;
        assert!(resolve_scope_components(&cfg).is_none());
    }
}
```

The CLI's `main.rs` currently calls `mcp::run()` (no args) — find the dispatch site (around line 204 where `run_status` lives, or grep for `mcp::run`). Update the call site to thread `vault_root` and the loaded `CairnConfig`. Locate the existing match arm for the `mcp` subcommand (search for `Some(("mcp",`); adjust it to:

```rust
        Some(("mcp", _sub)) => {
            // vault_root + config come from the same resolution path
            // status uses (see run_status). Reuse the same helpers so
            // operator-facing diagnostics match.
            let vault_root = match resolve_vault_root_for_mcp(explicit_vault.as_deref()) {
                Some(v) => v,
                None => return ExitCode::from(78), // EX_CONFIG
            };
            let config = match load_config_for_vault(&vault_root) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("cairn mcp: config error — {e:#}");
                    return ExitCode::from(78);
                }
            };
            return mcp::run(&vault_root, config);
        }
```

If helpers `resolve_vault_root_for_mcp` / `load_config_for_vault` do not already exist, mirror the existing `run_status` body — it already does the same vault-binding probe + config load and returns `(vault_root, config)`. The simplest concrete change: in `main.rs` near line 204, find the existing `run_status` body, factor the vault-resolution + config-load steps into a helper called `resolve_vault_and_config(explicit_vault) -> Result<(PathBuf, CairnConfig), ExitCode>`, then call that helper from both the `status` and `mcp` arms.

If extracting the helper is too invasive, the minimal alternative: copy the vault-binding probe block from `run_status` into the `mcp` arm verbatim. The plan explicitly accepts the duplication for Plan A; the eventual deduplication is its own follow-up.

- [ ] **Step 4: Run tests; confirm pass**

```bash
cargo nextest run -p cairn-cli --locked mcp::tests
cargo check -p cairn-cli --all-targets --locked
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
```

Expected: the three `mcp::tests::*` tests pass; the `#[allow(deprecated)]` block keeps clippy quiet on the `serve_stdio()` fallback; `cargo check` confirms `main.rs` compiles after the dispatch wiring.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/mcp.rs crates/cairn-cli/src/main.rs
git commit -m "$(cat <<'EOF'
feat(cli): wire MemoryStore + scope resolver into `cairn mcp`

resolve_scope_components reads [mcp.stdio] and returns
(Arc<dyn McpSessionScope>, ScopeTuple) when single_tenant = true,
else None. mcp::run validates config first (EX_CONFIG=78), opens
SQLite, then dispatches to serve_stdio_with_store. Without the opt-in
flag, falls back to the deprecated unwired serve_stdio (8-verb
manifest only) — Plan A keeps the existing default behaviour
byte-identical for non-opted-in deployments.

Issue #190 prerequisite (Plan A).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `cairn status` reports `mcp_graph_tools_available`

**Files:**
- Modify: `crates/cairn-cli/src/verbs/status.rs`
- Created by `insta`: `crates/cairn-cli/src/verbs/snapshots/status_mcp_graph_*.snap`

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-cli/src/verbs/status.rs` an inline test block (or extend the existing one):

```rust
#[cfg(test)]
mod mcp_graph_tests {
    use super::*;
    use cairn_core::config::CairnConfig;
    use cairn_core::contract::memory_store::MemoryStoreCapabilities;
    use cairn_core::domain::ScopeTuple;
    use cairn_core::mcp_auth::{
        ConfigBackedScope, McpGraphAvailability, McpSessionScope, McpTransport,
    };

    fn caps_with_graph(g: bool) -> MemoryStoreCapabilities {
        MemoryStoreCapabilities {
            fts: true,
            vector: false,
            graph_edges: g,
            transactions: true,
            per_record_consent_model: true,
        }
    }

    #[test]
    fn render_label_single_tenant_off() {
        let cfg = CairnConfig::default();
        let caps = caps_with_graph(true);
        let s = ConfigBackedScope::new(ScopeTuple::default());
        let dyn_s: &dyn McpSessionScope = &s;
        let avail =
            cfg.mcp_graph_tools_available(Some(dyn_s), McpTransport::Stdio, &caps);
        assert_eq!(
            render_mcp_graph_line(&avail, ProbeBasis::FullProbe),
            "mcp.graph_tools: unavailable (single-tenant mode off)",
        );
    }

    #[test]
    fn render_label_no_scope_resolver() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(ScopeTuple {
            tenant: Some("a".into()),
            ..ScopeTuple::default()
        });
        let caps = caps_with_graph(true);
        let avail =
            cfg.mcp_graph_tools_available(None, McpTransport::Stdio, &caps);
        assert_eq!(
            render_mcp_graph_line(&avail, ProbeBasis::FullProbe),
            "mcp.graph_tools: unavailable (no scope resolver wired)",
        );
    }

    #[test]
    fn render_label_no_store_capability() {
        let mut cfg = CairnConfig::default();
        cfg.mcp.stdio.single_tenant = true;
        cfg.mcp.stdio.principal = Some(ScopeTuple {
            tenant: Some("a".into()),
            ..ScopeTuple::default()
        });
        let caps = caps_with_graph(false);
        let s = ConfigBackedScope::new(cfg.mcp.stdio.principal.clone().unwrap());
        let dyn_s: &dyn McpSessionScope = &s;
        let avail =
            cfg.mcp_graph_tools_available(Some(dyn_s), McpTransport::Stdio, &caps);
        assert_eq!(
            render_mcp_graph_line(&avail, ProbeBasis::FullProbe),
            "mcp.graph_tools: unavailable (store does not advertise graph_edges)",
        );
    }

    #[test]
    fn render_label_available() {
        // Plan A never produces this state from the predicate, but the
        // formatter must still handle it for Plan C forward-compat.
        let avail = McpGraphAvailability::Available { tool_count: 5 };
        assert_eq!(
            render_mcp_graph_line(&avail, ProbeBasis::FullProbe),
            "mcp.graph_tools: available (5 tools)",
        );
    }
}
```

- [ ] **Step 2: Run tests; confirm failure**

```bash
cargo nextest run -p cairn-cli --locked verbs::status::mcp_graph_tests
```

Expected: compile errors — `render_mcp_graph_line` does not exist.

- [ ] **Step 3: Add the formatter and call it from `run_with_context`**

Append to `crates/cairn-cli/src/verbs/status.rs`:

```rust
/// Format one line for the MCP graph-tools availability state.
///
/// Used by the human-readable status output. JSON output emits the same
/// state via `McpGraphAvailability::label()` under `mcp.graph_tools`.
#[must_use]
pub fn render_mcp_graph_line(
    avail: &cairn_core::mcp_auth::McpGraphAvailability,
) -> String {
    use cairn_core::mcp_auth::McpGraphAvailability;
    match avail {
        McpGraphAvailability::Available { tool_count } => {
            format!("mcp.graph_tools: available ({tool_count} tools)")
        }
        McpGraphAvailability::UnavailableSingleTenantOff => {
            "mcp.graph_tools: unavailable (single-tenant mode off)".to_owned()
        }
        McpGraphAvailability::UnavailableNoStoreCapability => {
            "mcp.graph_tools: unavailable (store does not advertise graph_edges)"
                .to_owned()
        }
        McpGraphAvailability::UnavailableNoScopeResolver => {
            "mcp.graph_tools: unavailable (no scope resolver wired)".to_owned()
        }
    }
}
```

In `run_with_context` (around line 109 of the same file), after the existing capability-printing block (after `for cap in &resp.capabilities { ... }`), insert the MCP-graph reporting:

```rust
    // ── MCP graph-tools availability (issue #190 Plan A) ─────────────
    // Read the same predicate the MCP handler consults — using the
    // same inputs (real store capabilities, real scope resolver) so
    // `status` and `cairn mcp` cannot disagree about whether graph
    // tools are advertised. If we cannot open the store from this
    // call site (no vault, opening would fail, etc.) we report the
    // *config-only* portion of the predicate and label the result
    // explicitly as a config-only probe so operators are not misled.
    if let Some(cfg) = config {
        // Build the same scope-resolver components the MCP handler
        // would build from this config. Reuses the helper from
        // Task 5 so there is exactly one path that derives them.
        let scope_components =
            crate::mcp::resolve_scope_components(cfg).ok().flatten();
        let scope_for_predicate: Option<&dyn cairn_core::mcp_auth::McpSessionScope> =
            scope_components.as_ref().map(|(s, _principal)| {
                std::sync::Arc::as_ref(s) as &dyn cairn_core::mcp_auth::McpSessionScope
            });

        // Open the real store to read its capabilities. If the open
        // path fails (no vault, migrations not run, locked DB, …) we
        // fall back to the config-only probe and tag the JSON/human
        // output so operators see "config-only" rather than a false
        // "store does not advertise graph_edges" report.
        let (caps_for_predicate, probe_basis) = match vault_root {
            Some(root) => match try_open_store_capabilities(root) {
                Ok(caps) => (caps, ProbeBasis::FullProbe),
                Err(err) => {
                    tracing::debug!(
                        ?err,
                        "status: store-cap probe failed; falling back to config-only"
                    );
                    (
                        cairn_core::contract::memory_store::MemoryStoreCapabilities::default(),
                        ProbeBasis::ConfigOnly,
                    )
                }
            },
            None => (
                cairn_core::contract::memory_store::MemoryStoreCapabilities::default(),
                ProbeBasis::ConfigOnly,
            ),
        };

        let avail = cfg.mcp_graph_tools_available(
            scope_for_predicate,
            cairn_core::mcp_auth::McpTransport::Stdio,
            &caps_for_predicate,
        );

        if !json {
            // Human output: append one line, with a "(config-only)"
            // suffix when the store probe could not run, so a
            // misleading `unavailable (store does not advertise
            // graph_edges)` cannot stand in for a real verdict.
            println!("{}", render_mcp_graph_line(&avail, probe_basis));
        }
        // JSON output is handled below by extending the existing
        // status payload — NOT by emitting a second JSON document.
    }
```

**JSON integration: single-document, no addendum.** The previous
sketch emitted a second `{"mcp.graph_tools":"…"}` JSON object
after the primary status payload, which breaks `--json` parsers.
The correct shape is to extend the status response struct itself.

In `crates/cairn-cli/src/verbs/status.rs`, locate the response
struct serialized by `emit_json` (it is already `serde::Serialize`).
Add a new field:

```rust
#[derive(serde::Serialize)]
pub struct StatusResponse {
    // … existing fields stay unchanged …

    /// MCP graph-tools availability state. Always present in JSON
    /// output. Field shape:
    ///   { "state": "available" | "unavailable",
    ///     "reason": Option<&'static str>,
    ///     "tool_count": Option<u32>,
    ///     "probe_basis": "full" | "config_only" }
    pub mcp_graph_tools: McpGraphToolsStatus,
}

#[derive(serde::Serialize)]
pub struct McpGraphToolsStatus {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<u32>,
    pub probe_basis: &'static str, // "full" | "config_only"
}

impl McpGraphToolsStatus {
    pub fn from_predicate(
        avail: &cairn_core::mcp_auth::McpGraphAvailability,
        probe_basis: ProbeBasis,
    ) -> Self {
        use cairn_core::mcp_auth::McpGraphAvailability;
        let basis = match probe_basis {
            ProbeBasis::FullProbe => "full",
            ProbeBasis::ConfigOnly => "config_only",
        };
        match avail {
            McpGraphAvailability::Available { tool_count } => Self {
                state: "available",
                reason: None,
                tool_count: Some(u32::try_from(*tool_count).unwrap_or(0)),
                probe_basis: basis,
            },
            McpGraphAvailability::UnavailableSingleTenantOff => Self {
                state: "unavailable",
                reason: Some("single_tenant_off"),
                tool_count: None,
                probe_basis: basis,
            },
            McpGraphAvailability::UnavailableNoStoreCapability => Self {
                state: "unavailable",
                reason: Some("no_store_capability"),
                tool_count: None,
                probe_basis: basis,
            },
            McpGraphAvailability::UnavailableNoScopeResolver => Self {
                state: "unavailable",
                reason: Some("no_scope_resolver"),
                tool_count: None,
                probe_basis: basis,
            },
        }
    }
}
```

The existing `emit_json` call serializes the (now extended)
`StatusResponse` once. There is **no second `println!`** for the
JSON path — strict parsers see exactly one document.

```rust
/// Result of the store-capability probe — full open succeeded vs.
/// fell back to config-only. Surfaced in `status` output so an
/// "unavailable" verdict is never mistaken for an authoritative
/// negative when the store could not actually be inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeBasis {
    FullProbe,
    ConfigOnly,
}

/// Open the SQLite store at `vault_root` read-only and read its
/// `MemoryStoreCapabilities`. Mirrors what `cairn mcp` does at
/// startup — using the exact same code path keeps `status` and
/// `cairn mcp` from disagreeing about whether graph_edges is
/// advertised. Errors are returned to the caller so the `status`
/// path can degrade to a config-only probe with a tagged label.
fn try_open_store_capabilities(
    vault_root: &std::path::Path,
) -> Result<
    cairn_core::contract::memory_store::MemoryStoreCapabilities,
    Box<dyn std::error::Error>,
> {
    let db_path = vault_root.join(".cairn").join("memory.db");
    let store = cairn_store_sqlite::SqliteMemoryStore::open_read_only(&db_path)?;
    Ok(*store.capabilities())
}
```

If `SqliteMemoryStore::open_read_only` does not yet exist, this
task adds it as a thin wrapper over the existing `open()`
constructor with a `read_only = true` flag — small, mechanical,
and the same code is what `cairn mcp` will eventually share.

`render_mcp_graph_line` gains the `probe_basis` parameter so the
human-readable line can carry a `(config-only)` suffix when the
store could not be opened:

```rust
#[must_use]
pub fn render_mcp_graph_line(
    avail: &cairn_core::mcp_auth::McpGraphAvailability,
    probe_basis: ProbeBasis,
) -> String {
    use cairn_core::mcp_auth::McpGraphAvailability;
    let suffix = match probe_basis {
        ProbeBasis::FullProbe => "",
        ProbeBasis::ConfigOnly => " (config-only)",
    };
    match avail {
        McpGraphAvailability::Available { tool_count } => {
            format!("mcp.graph_tools: available ({tool_count} tools){suffix}")
        }
        McpGraphAvailability::UnavailableSingleTenantOff => {
            format!("mcp.graph_tools: unavailable (single-tenant mode off){suffix}")
        }
        McpGraphAvailability::UnavailableNoStoreCapability => {
            format!(
                "mcp.graph_tools: unavailable (store does not advertise graph_edges){suffix}"
            )
        }
        McpGraphAvailability::UnavailableNoScopeResolver => {
            format!("mcp.graph_tools: unavailable (no scope resolver wired){suffix}")
        }
    }
}
```

- [ ] **Step 4: Run tests; confirm pass + add snapshot tests**

```bash
cargo nextest run -p cairn-cli --locked verbs::status::mcp_graph_tests
cargo clippy -p cairn-cli --all-targets --locked -- -D warnings
```

Expected: 4 unit tests pass; clippy clean. **No `insta` snapshots are generated yet** — `render_mcp_graph_line` returns deterministic strings asserted by `assert_eq!`. If the team prefers snapshot coverage, add a single test:

```rust
    #[test]
    fn snapshot_all_four_states() {
        use cairn_core::mcp_auth::McpGraphAvailability;
        let states = [
            McpGraphAvailability::Available { tool_count: 5 },
            McpGraphAvailability::UnavailableSingleTenantOff,
            McpGraphAvailability::UnavailableNoStoreCapability,
            McpGraphAvailability::UnavailableNoScopeResolver,
        ];
        let rendered: Vec<String> =
            states.iter().map(render_mcp_graph_line).collect();
        insta::assert_yaml_snapshot!("status_mcp_graph_all_four", rendered);
    }
```

Then run:

```bash
cargo insta test -p cairn-cli --locked --accept verbs::status::mcp_graph_tests::snapshot_all_four_states
```

Expected: a single new snapshot file lands at
`crates/cairn-cli/src/verbs/snapshots/cairn_cli__verbs__status__mcp_graph_tests__status_mcp_graph_all_four.snap`.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/verbs/status.rs \
       crates/cairn-cli/src/verbs/snapshots/
git commit -m "$(cat <<'EOF'
feat(cli): cairn status reports mcp_graph_tools_available

render_mcp_graph_line maps McpGraphAvailability to one of four stable
human-readable lines. status.run_with_context calls it after the
existing capabilities block. JSON output gets a single-line
{"mcp.graph_tools":"<label>"} addendum so machine consumers can grep.
Snapshot pins all four states; Plan C's flip will fail this test
loudly so the formatter gets updated together with the predicate.

Issue #190 prerequisite (Plan A).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Integration test — unconfigured stdio returns 8-verb manifest only

**Files:**
- Create: `crates/cairn-mcp/tests/manifest_unconfigured.rs`
- Modify (if needed): `crates/cairn-mcp/Cargo.toml` (`[dev-dependencies]`)

- [ ] **Step 1: Write failing test**

Create `crates/cairn-mcp/tests/manifest_unconfigured.rs`:

```rust
//! Integration test (issue #190 Plan A acceptance criterion 8):
//!
//! An MCP stdio server constructed against a CairnConfig with no
//! [mcp.stdio] block (or with single_tenant = false) lists the 8-verb
//! manifest only. `mcp_graph_tools_available` must return
//! `UnavailableSingleTenantOff` for the same deployment shape.

use std::sync::Arc;

use cairn_core::config::CairnConfig;
use cairn_core::contract::memory_store::MemoryStoreCapabilities;
use cairn_core::domain::ScopeTuple;
use cairn_core::mcp_auth::{
    ConfigBackedScope, McpGraphAvailability, McpSessionScope, McpTransport,
};
use cairn_mcp::CairnMcpHandler;
use cairn_test_fixtures::FixtureStore;

fn graph_capable_caps() -> MemoryStoreCapabilities {
    MemoryStoreCapabilities {
        fts: true,
        vector: false,
        graph_edges: true,
        transactions: true,
        per_record_consent_model: true,
    }
}

#[test]
fn unconfigured_stdio_lists_eight_verbs_only() {
    // Default config: single_tenant = false → no scope resolver wired.
    let cfg = CairnConfig::default();
    let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
        Arc::new(FixtureStore::default());

    // Plan A: serve_stdio_with_store is gated behind single_tenant = true
    // in the CLI; the unwired path uses CairnMcpHandler::with_store
    // (no scope, no principal). Mirror that here.
    let handler = CairnMcpHandler::with_store(store, cfg.clone());

    let listed = handler.listed_tool_names();
    assert_eq!(
        listed.len(),
        cairn_mcp::generated::TOOLS.len(),
        "unconfigured stdio MUST list exactly the 8-verb manifest, got {listed:?}",
    );
    for tool in &listed {
        assert!(
            !tool.starts_with("graph."),
            "no graph.* tools in unconfigured manifest, got `{tool}`",
        );
    }

    // Predicate side: same config -> UnavailableSingleTenantOff
    // regardless of scope-resolver presence.
    let scope = ConfigBackedScope::new(ScopeTuple::default());
    let dyn_s: &dyn McpSessionScope = &scope;
    let avail = cfg.mcp_graph_tools_available(
        Some(dyn_s),
        McpTransport::Stdio,
        &graph_capable_caps(),
    );
    assert_eq!(
        avail,
        McpGraphAvailability::UnavailableSingleTenantOff,
        "predicate must report UnavailableSingleTenantOff for default config",
    );
}

#[test]
fn opted_in_stdio_with_graphless_store_reports_no_store_capability() {
    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(ScopeTuple {
        tenant: Some("acme".into()),
        ..ScopeTuple::default()
    });
    cfg.validate_mcp().expect("opt-in config valid");

    let scope = ConfigBackedScope::new(cfg.mcp.stdio.principal.clone().unwrap());
    let dyn_s: &dyn McpSessionScope = &scope;

    // Same as `graph_capable_caps` but graph_edges = false.
    let caps_no_graph = MemoryStoreCapabilities {
        graph_edges: false,
        ..graph_capable_caps()
    };

    let avail = cfg.mcp_graph_tools_available(
        Some(dyn_s),
        McpTransport::Stdio,
        &caps_no_graph,
    );
    assert_eq!(avail, McpGraphAvailability::UnavailableNoStoreCapability);
}

#[test]
fn opted_in_stdio_without_resolver_reports_no_scope_resolver() {
    let mut cfg = CairnConfig::default();
    cfg.mcp.stdio.single_tenant = true;
    cfg.mcp.stdio.principal = Some(ScopeTuple {
        tenant: Some("acme".into()),
        ..ScopeTuple::default()
    });
    let avail = cfg.mcp_graph_tools_available(
        None,
        McpTransport::Stdio,
        &graph_capable_caps(),
    );
    assert_eq!(avail, McpGraphAvailability::UnavailableNoScopeResolver);
}
```

- [ ] **Step 2: Run tests; confirm failure**

```bash
cargo nextest run -p cairn-mcp --locked --test manifest_unconfigured
```

Expected: compiles and tests pass IF Tasks 1–4 landed correctly. If `FixtureStore` is missing, add `cairn-test-fixtures = { workspace = true }` to `[dev-dependencies]` of `crates/cairn-mcp/Cargo.toml` (it should already be there from existing handler tests — verify with `grep cairn-test-fixtures crates/cairn-mcp/Cargo.toml`).

If the test fails on the assertion (e.g. listed.len() != 8), the regression is real — investigate before changing the test.

- [ ] **Step 3: Implementation already in place**

Tasks 1–4 implement everything this test exercises. If a gap surfaces, fix the underlying task rather than the test.

- [ ] **Step 4: Run tests; confirm pass**

```bash
cargo nextest run -p cairn-mcp --locked --test manifest_unconfigured
cargo clippy -p cairn-mcp --all-targets --locked -- -D warnings
```

Expected: 3 tests pass; clippy clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-mcp/tests/manifest_unconfigured.rs crates/cairn-mcp/Cargo.toml
git commit -m "$(cat <<'EOF'
test(mcp): integration test pins 8-verb manifest for unconfigured stdio

Plan A acceptance criterion: a CairnMcpHandler built against a default
CairnConfig (no [mcp.stdio] block, single_tenant = false) lists exactly
the 8-verb TOOLS slice and never a graph.* tool. The same config drives
mcp_graph_tools_available -> UnavailableSingleTenantOff.

Two companion cases pin the other two Unavailable variants so Plan C's
flip surfaces here as a single failing test.

Issue #190 prerequisite (Plan A).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Workspace verification — full CI sweep

**Files:** none (verification only).

- [ ] **Step 1: Run the same commands `ci.yml` runs**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
cargo run -p cairn-idl --bin cairn-codegen --locked -- --check
```

Expected: all green. The `check-core-boundary.sh` step is critical — it confirms `cairn-core/src/mcp_auth/` did not pull in any non-core workspace dep. The codegen `--check` confirms no IDL drift (this PR adds no IDL).

- [ ] **Step 2: Confirm `cairn status` produces the expected output**

```bash
cargo run -p cairn-cli --bin cairn -- bootstrap --vault /tmp/cairn-plan-a-test
cargo run -p cairn-cli --bin cairn --vault /tmp/cairn-plan-a-test -- status
```

Expected (last line):

```
mcp.graph_tools: unavailable (single-tenant mode off)
```

Then edit `/tmp/cairn-plan-a-test/.cairn/config.yaml` and add:

```yaml
mcp:
  stdio:
    single_tenant: true
    principal:
      tenant: acme
```

```bash
cargo run -p cairn-cli --bin cairn --vault /tmp/cairn-plan-a-test -- status
```

Expected: either `unavailable (no scope resolver wired)` or `unavailable (store does not advertise graph_edges)` — Plan A's `probe_store_caps` returns `None`, so the line is `unavailable (no scope resolver wired)` (the predicate hits the scope branch first because `cairn status` does not construct a resolver).

Cleanup:

```bash
rm -rf /tmp/cairn-plan-a-test
```

- [ ] **Step 3: Run `cargo deny`, `cargo audit`, `cargo machete`**

```bash
cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: clean. No new dependencies were added; the only changes are workspace-internal.

- [ ] **Step 4: Update `docs/design/traceability.md`**

Open `docs/design/traceability.md` and add a line under the §4.1 `MCPServer` row (or create the row if absent) pointing to issue #190 Plan A:

```markdown
| §4.1 MCPServer (auth substrate) | issue #190 Plan A — `[mcp.stdio]` config, `McpSessionScope`, `mcp_graph_tools_available` |
```

- [ ] **Step 5: Commit verification + docs**

```bash
git add docs/design/traceability.md
git commit -m "$(cat <<'EOF'
docs(traceability): map issue #190 Plan A to §4.1 MCPServer

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review notes

- **Spec coverage:**
  - §2.1 (capability advertisement gating + scope-by-provenance trust model) — surfaced through `mcp_graph_tools_available` (Task 3) and the integration test (Task 7).
  - §2.1.1 (scope-resolution prereq, single-tenant on stdio) — `McpSessionScope` + `ConfigBackedScope` (Task 1), config schema (Task 2), CLI wiring (Task 5). The plan does NOT implement per-request principal extraction from `RequestContext::extensions` — that is a separate transport issue and §2.1.1 explicitly defers it.
  - §6 (capability advertisement) — `McpGraphAvailability` + `cairn status` line (Tasks 1, 3, 6).
  - `serve_stdio_with_store` (Task 4), `mcp_graph_tools_available` (Task 3), `McpAuthContext` (Task 1), `McpStdioConfig` (Task 2), `single_tenant` + `principal` fields (Task 2), all referenced spec identifiers landed.
  - **Spec coverage gaps (deliberate, in Plan A):**
    1. No graph tools land — `Available { tool_count }` is reachable in tests but never produced by the predicate (Task 3 documents this).
    2. `probe_store_caps` (Task 6) is a stub returning `None`. Plan C replaces it with a real SQLite probe.
    3. `tools/list` re-resolution per-request (spec §2.1) is not exercised — Plan A keeps the manifest at the 8-verb constant slice, so per-request gating is moot until Plan C.

- **Placeholder scan:** searched for "TODO", "TBD", "similar to", "as above", "fill in", "later" — none remain in normative content. The `probe_store_caps` body is intentionally a stub with a code comment explaining why; that is implementation, not a plan placeholder.

- **Type consistency:**
  - `McpAuthContext::new(&ScopeTuple, &str)` used identically in Tasks 1, 4, 7.
  - `McpGraphAvailability` variants: `Available { tool_count: u8 }` and three `Unavailable*` units — consistent across Tasks 1, 3, 6, 7.
  - `mcp_graph_tools_available(Option<&dyn McpSessionScope>, McpTransport, &MemoryStoreCapabilities) -> McpGraphAvailability` — same signature in Tasks 3, 6, 7.
  - `serve_stdio_with_store(Arc<dyn MemoryStore>, Arc<dyn McpSessionScope>, CairnConfig, ScopeTuple)` — same in Tasks 4 and 5.
  - `McpStdioConfig { single_tenant: bool, principal: Option<ScopeTuple> }` — same in Tasks 2, 5, 6, 7.
