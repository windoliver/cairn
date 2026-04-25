# Plugin Host + `cairn plugins list/verify` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the remaining acceptance criteria of issue #143 by wiring a live plugin host into `cairn-cli`, adding `cairn plugins list` + `cairn plugins verify`, shipping a two-tier conformance suite in `cairn-core`, and integrating verify into CI.

**Architecture:** Bundled adapter crates (`cairn-store-sqlite`, `cairn-mcp`, `cairn-sensors-local`, `cairn-workflows`) each ship a `plugin.toml` + `register()` function emitted by the `register_plugin!` macro (extended to a manifest-aware four-arg form). `cairn-cli` calls each `register()` in alphabetical order from `plugins::host::register_all()`, populates a `PluginRegistry`, and serves `plugins list`/`plugins verify` against it. Tier-1 cases (manifest match, register round-trip, capability self-consistency) live in `cairn-core::contract::conformance` as pure code; tier-2 stubs return `Pending` until per-impl PRs land.

**Tech Stack:** Rust 2024 edition, `clap` 4.5 (derive), `serde`/`serde_json` (JSON output), `toml` (manifest parsing — already a workspace dep), `insta` (snapshot tests), `cargo nextest` (test runner).

**Spec:** `docs/superpowers/specs/2026-04-25-plugin-host-list-verify-design.md`

---

## File Structure

**Created:**
- `crates/cairn-core/src/contract/conformance/mod.rs`
- `crates/cairn-core/src/contract/conformance/memory_store.rs`
- `crates/cairn-core/src/contract/conformance/mcp_server.rs`
- `crates/cairn-core/src/contract/conformance/sensor_ingress.rs`
- `crates/cairn-core/src/contract/conformance/workflow_orchestrator.rs`
- `crates/cairn-cli/src/plugins/mod.rs`
- `crates/cairn-cli/src/plugins/host.rs`
- `crates/cairn-cli/src/plugins/list.rs`
- `crates/cairn-cli/src/plugins/verify.rs`
- `crates/cairn-cli/tests/plugins_list_snapshot.rs`
- `crates/cairn-cli/tests/plugins_verify_snapshot.rs`
- `crates/cairn-cli/tests/plugins_verify.rs`
- `crates/cairn-store-sqlite/plugin.toml`
- `crates/cairn-store-sqlite/tests/manifest_validates.rs`
- `crates/cairn-mcp/plugin.toml`
- `crates/cairn-mcp/tests/manifest_validates.rs`
- `crates/cairn-sensors-local/plugin.toml`
- `crates/cairn-sensors-local/tests/manifest_validates.rs`
- `crates/cairn-workflows/plugin.toml`
- `crates/cairn-workflows/tests/manifest_validates.rs`

**Modified:**
- `Cargo.toml` (workspace) — add `clap`, `insta` workspace deps
- `crates/cairn-core/src/contract/registry.rs` — add `register_*_with_manifest` siblings + global manifest map + `parsed_manifest` accessor
- `crates/cairn-core/src/contract/macros.rs` — extend each arm with 4-arg manifest-aware variant
- `crates/cairn-core/src/contract/mod.rs` — re-export `conformance`
- `crates/cairn-cli/Cargo.toml` — add `clap`, `serde`, `serde_json` deps; drop machete ignores for now-used crates
- `crates/cairn-cli/src/main.rs` — replace argv matcher with clap dispatch
- `crates/cairn-store-sqlite/Cargo.toml` — drop `cairn-core` from machete ignored
- `crates/cairn-store-sqlite/src/lib.rs` — stub `MemoryStore` impl + `register()`
- `crates/cairn-mcp/Cargo.toml` — add `cairn-core` dep + drop machete ignored once used
- `crates/cairn-mcp/src/lib.rs` — stub `MCPServer` impl + `register()`
- `crates/cairn-sensors-local/Cargo.toml` — add `cairn-core` dep
- `crates/cairn-sensors-local/src/lib.rs` — stub `SensorIngress` impl + `register()`
- `crates/cairn-workflows/Cargo.toml` — add `cairn-core` dep
- `crates/cairn-workflows/src/lib.rs` — stub `WorkflowOrchestrator` impl + `register()`
- `.github/workflows/ci.yml` — add `cairn plugins verify` step + JSON artifact upload

---

## Task 1: Registry — global manifest map + `_with_manifest` siblings

**Files:**
- Modify: `crates/cairn-core/src/contract/registry.rs`
- Test: `crates/cairn-core/src/contract/registry.rs` (existing `mod tests`)

- [ ] **Step 1: Write failing test** — `crates/cairn-core/src/contract/registry.rs`

Append to the `#[cfg(test)] mod tests` block (after the existing `lookup_returns_none_for_unknown_name` test):

```rust
    use crate::contract::manifest::{ContractKind, PluginManifest};

    fn store_manifest_text() -> &'static str {
        r#"
name = "cairn-store-sqlite"
contract = "MemoryStore"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0
"#
    }

    #[test]
    fn register_with_manifest_inserts_into_both_maps() {
        let mut reg = PluginRegistry::new();
        let name = PluginName::new("cairn-store-sqlite").expect("valid");
        let manifest =
            PluginManifest::parse_toml(store_manifest_text()).expect("manifest parses");
        reg.register_memory_store_with_manifest(
            name.clone(),
            manifest,
            Arc::new(StubStore {
                name: "cairn-store-sqlite",
                range: compatible(),
            }),
        )
        .expect("manifest-aware registration succeeds");

        assert!(reg.memory_store(&name).is_some(), "store registered");
        assert!(
            reg.parsed_manifest(&name).is_some(),
            "manifest registered"
        );
        assert_eq!(
            reg.parsed_manifest(&name).unwrap().contract(),
            ContractKind::MemoryStore
        );
    }

    #[test]
    fn register_with_manifest_rejects_kind_mismatch() {
        // Manifest declares MemoryStore; we try to register through the
        // LLMProvider sibling — verify_compatible_with must trip.
        let mut reg = PluginRegistry::new();
        let name = PluginName::new("cairn-store-sqlite").expect("valid");
        let manifest =
            PluginManifest::parse_toml(store_manifest_text()).expect("manifest parses");

        // We stub an LLMProvider impl in registry tests by reusing StubStore
        // is impossible (different trait). Use an existing stub from the
        // contract registry — fall back to building a custom stub for this
        // test directly below.
        struct StubLlm {
            name: &'static str,
            range: VersionRange,
        }
        #[async_trait::async_trait]
        impl crate::contract::llm_provider::LLMProvider for StubLlm {
            fn name(&self) -> &str {
                self.name
            }
            fn capabilities(&self) -> &crate::contract::llm_provider::LLMProviderCapabilities {
                static CAPS: crate::contract::llm_provider::LLMProviderCapabilities =
                    crate::contract::llm_provider::LLMProviderCapabilities {
                        streaming: false,
                        tool_use: false,
                        vision: false,
                    };
                &CAPS
            }
            fn supported_contract_versions(&self) -> VersionRange {
                self.range
            }
        }
        let err = reg
            .register_llm_provider_with_manifest(
                name,
                manifest,
                Arc::new(StubLlm {
                    name: "cairn-store-sqlite",
                    range: compatible(),
                }),
            )
            .expect_err("kind mismatch must fail closed");
        assert!(matches!(err, PluginError::ContractMismatch { .. }));
    }

    #[test]
    fn register_with_manifest_rejects_global_duplicate_name() {
        let mut reg = PluginRegistry::new();
        let name = PluginName::new("cairn-store-sqlite").expect("valid");
        let manifest =
            PluginManifest::parse_toml(store_manifest_text()).expect("manifest parses");
        reg.register_memory_store_with_manifest(
            name.clone(),
            manifest.clone(),
            Arc::new(StubStore {
                name: "cairn-store-sqlite",
                range: compatible(),
            }),
        )
        .expect("first registration succeeds");

        let err = reg
            .register_memory_store_with_manifest(
                name,
                manifest,
                Arc::new(StubStore {
                    name: "cairn-store-sqlite",
                    range: compatible(),
                }),
            )
            .expect_err("global duplicate must fail");
        assert!(matches!(err, PluginError::DuplicateName { .. }));
    }
```

Note: tests use the existing `StubStore` defined earlier in the same `mod tests` block (lines ~390–413).

- [ ] **Step 2: Run test, expect failure**

Run: `cargo test -p cairn-core --lib contract::registry::tests -- --nocapture`
Expected: compile error (`register_memory_store_with_manifest` does not exist; `parsed_manifest` does not exist).

- [ ] **Step 3: Add the global manifest map**

In `crates/cairn-core/src/contract/registry.rs`, modify the `PluginRegistry` struct (around line 156):

```rust
#[derive(Default)]
pub struct PluginRegistry {
    memory_stores: HashMap<PluginName, Arc<dyn MemoryStore>>,
    llm_providers: HashMap<PluginName, Arc<dyn LLMProvider>>,
    workflow_orchestrators: HashMap<PluginName, Arc<dyn WorkflowOrchestrator>>,
    sensor_ingress: HashMap<PluginName, Arc<dyn SensorIngress>>,
    mcp_servers: HashMap<PluginName, Arc<dyn MCPServer>>,
    frontend_adapters: HashMap<PluginName, Arc<dyn FrontendAdapter>>,
    agent_providers: HashMap<PluginName, Arc<dyn AgentProvider>>,
    /// Per-name manifest, populated by `register_*_with_manifest`. Global
    /// across contracts: a single `PluginName` cannot have two manifests
    /// even if the impl side allows reusing the name across contracts.
    manifests: HashMap<PluginName, crate::contract::manifest::PluginManifest>,
}
```

Add `manifests` to the manual `Debug` impl too (line ~166):

```rust
            .field("manifests", &self.manifests.keys().collect::<Vec<_>>())
```

- [ ] **Step 4: Add a `register_method_with_manifest!` helper macro**

Below the existing `register_method!` macro (around line 200), add:

```rust
// Manifest-aware sibling. Verifies the manifest matches the contract kind
// and host version, enforces global manifest-name uniqueness, then delegates
// to the bare `register_<contract>` method.
macro_rules! register_method_with_manifest {
    ($method:ident, $bare:ident, $trait_:path, $contract:literal, $kind:expr, $host_version:expr) => {
        /// Manifest-aware registration.
        ///
        /// # Errors
        /// - [`PluginError::ContractMismatch`] / [`PluginError::ManifestNameMismatch`] /
        ///   [`PluginError::UnsupportedContractVersion`] from
        ///   [`crate::contract::manifest::PluginManifest::verify_compatible_with`].
        /// - [`PluginError::DuplicateName`] when another plugin already
        ///   holds this name globally (across all contracts).
        /// - Plus any error returned by the bare `register_<contract>`
        ///   method (identity / per-contract duplicate / version).
        pub fn $method(
            &mut self,
            name: PluginName,
            manifest: crate::contract::manifest::PluginManifest,
            plugin: Arc<dyn $trait_>,
        ) -> Result<(), PluginError> {
            manifest.verify_compatible_with(&name, $kind, $host_version)?;
            if self.manifests.contains_key(&name) {
                return Err(PluginError::DuplicateName {
                    contract: $contract,
                    name,
                });
            }
            self.$bare(name.clone(), plugin)?;
            self.manifests.insert(name, manifest);
            Ok(())
        }
    };
}
```

- [ ] **Step 5: Wire each `_with_manifest` method**

Inside the existing `impl PluginRegistry` block (after the seven `register_method!` invocations near line 247), add:

```rust
    register_method_with_manifest!(
        register_memory_store_with_manifest,
        register_memory_store,
        crate::contract::memory_store::MemoryStore,
        "MemoryStore",
        crate::contract::manifest::ContractKind::MemoryStore,
        crate::contract::memory_store::CONTRACT_VERSION
    );
    register_method_with_manifest!(
        register_llm_provider_with_manifest,
        register_llm_provider,
        crate::contract::llm_provider::LLMProvider,
        "LLMProvider",
        crate::contract::manifest::ContractKind::LLMProvider,
        crate::contract::llm_provider::CONTRACT_VERSION
    );
    register_method_with_manifest!(
        register_workflow_orchestrator_with_manifest,
        register_workflow_orchestrator,
        crate::contract::workflow_orchestrator::WorkflowOrchestrator,
        "WorkflowOrchestrator",
        crate::contract::manifest::ContractKind::WorkflowOrchestrator,
        crate::contract::workflow_orchestrator::CONTRACT_VERSION
    );
    register_method_with_manifest!(
        register_sensor_ingress_with_manifest,
        register_sensor_ingress,
        crate::contract::sensor_ingress::SensorIngress,
        "SensorIngress",
        crate::contract::manifest::ContractKind::SensorIngress,
        crate::contract::sensor_ingress::CONTRACT_VERSION
    );
    register_method_with_manifest!(
        register_mcp_server_with_manifest,
        register_mcp_server,
        crate::contract::mcp_server::MCPServer,
        "MCPServer",
        crate::contract::manifest::ContractKind::MCPServer,
        crate::contract::mcp_server::CONTRACT_VERSION
    );
    register_method_with_manifest!(
        register_frontend_adapter_with_manifest,
        register_frontend_adapter,
        crate::contract::frontend_adapter::FrontendAdapter,
        "FrontendAdapter",
        crate::contract::manifest::ContractKind::FrontendAdapter,
        crate::contract::frontend_adapter::CONTRACT_VERSION
    );
    register_method_with_manifest!(
        register_agent_provider_with_manifest,
        register_agent_provider,
        crate::contract::agent_provider::AgentProvider,
        "AgentProvider",
        crate::contract::manifest::ContractKind::AgentProvider,
        crate::contract::agent_provider::CONTRACT_VERSION
    );

    /// Look up the parsed manifest for a registered plugin by name.
    #[must_use]
    pub fn parsed_manifest(
        &self,
        name: &PluginName,
    ) -> Option<&crate::contract::manifest::PluginManifest> {
        self.manifests.get(name)
    }

    /// Iterate every parsed manifest in alphabetical order by plugin name.
    /// Used by `cairn plugins list`/`verify` for stable output.
    pub fn parsed_manifests_sorted(
        &self,
    ) -> Vec<(&PluginName, &crate::contract::manifest::PluginManifest)> {
        let mut v: Vec<_> = self.manifests.iter().collect();
        v.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        v
    }
```

- [ ] **Step 6: Run test, expect pass**

Run: `cargo test -p cairn-core --lib contract::registry::tests`
Expected: PASS, including the three new tests + all existing tests.

- [ ] **Step 7: Lint clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-core/src/contract/registry.rs
git commit -m "feat(core): add register_*_with_manifest siblings + parsed_manifest accessor (#143)

Brief §4.1: hosts gate plugin activation on a parsed PluginManifest.
The new sibling methods verify the manifest's name+kind+version against
the host before delegating to the bare register_<contract> method, and
populate a global manifests map so 'cairn plugins list'/'verify' can
read the manifest for each registered plugin.

A global manifest map enforces name-uniqueness across contracts, which
is brief-aligned (one PluginName per crate)."
```

---

## Task 2: Macro extension — 4-arg manifest-aware variant

**Files:**
- Modify: `crates/cairn-core/src/contract/macros.rs`
- Test: `crates/cairn-core/tests/contract_registry.rs` (new test cases)

- [ ] **Step 1: Write failing test** — add to `crates/cairn-core/tests/contract_registry.rs`

Append to the bottom of the file:

```rust
mod manifest_aware_plugin {
    use super::*;
    use cairn_core::contract::manifest::PluginManifest;

    pub const MANIFEST_TOML: &str = r#"
name = "fake-with-manifest"
contract = "MemoryStore"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0
"#;

    #[derive(Default)]
    pub struct FakeStore;

    #[async_trait::async_trait]
    impl MemoryStore for FakeStore {
        fn name(&self) -> &'static str {
            "fake-with-manifest"
        }
        fn capabilities(&self) -> &MemoryStoreCapabilities {
            static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
                fts: false,
                vector: false,
                graph_edges: false,
                transactions: false,
            };
            &CAPS
        }
        fn supported_contract_versions(&self) -> VersionRange {
            VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
        }
    }

    register_plugin!(MemoryStore, FakeStore, "fake-with-manifest", MANIFEST_TOML);

    // Also build the parsed manifest at module scope so tests can compare.
    pub fn parsed_manifest() -> PluginManifest {
        PluginManifest::parse_toml(MANIFEST_TOML).expect("manifest parses")
    }
}

#[test]
fn manifest_aware_macro_registers_with_manifest() {
    let mut reg = PluginRegistry::new();
    manifest_aware_plugin::register(&mut reg).expect("manifest-aware register succeeds");

    let name = PluginName::new("fake-with-manifest").expect("valid");
    assert!(reg.memory_store(&name).is_some(), "trait registered");
    assert!(reg.parsed_manifest(&name).is_some(), "manifest registered");
    assert_eq!(
        reg.parsed_manifest(&name).unwrap().contract(),
        cairn_core::contract::manifest::ContractKind::MemoryStore
    );
}
```

- [ ] **Step 2: Run test, expect failure**

Run: `cargo test -p cairn-core --test contract_registry`
Expected: compile error — `register_plugin!` does not accept four arguments.

- [ ] **Step 3: Extend the macro**

In `crates/cairn-core/src/contract/macros.rs`, replace each existing arm with a *pair* of arms (3-arg + 4-arg) and add the manifest-aware helper.

Replace the entire `register_plugin!` macro (lines 70–93) with:

```rust
#[macro_export]
macro_rules! register_plugin {
    // 3-arg form: legacy / unit-test path with no manifest.
    (MemoryStore, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_memory_store, $impl, $name);
    };
    (LLMProvider, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_llm_provider, $impl, $name);
    };
    (WorkflowOrchestrator, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_workflow_orchestrator, $impl, $name);
    };
    (SensorIngress, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_sensor_ingress, $impl, $name);
    };
    (MCPServer, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_mcp_server, $impl, $name);
    };
    (FrontendAdapter, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_frontend_adapter, $impl, $name);
    };
    (AgentProvider, $impl:ty, $name:literal) => {
        $crate::__register_plugin_helper!(register_agent_provider, $impl, $name);
    };

    // 4-arg form: manifest-aware. `$manifest` is an expression producing a
    // `&'static str` (typically a `pub const MANIFEST_TOML: &str = include_str!(...)`).
    (MemoryStore, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_memory_store_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (LLMProvider, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_llm_provider_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (WorkflowOrchestrator, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_workflow_orchestrator_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (SensorIngress, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_sensor_ingress_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (MCPServer, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_mcp_server_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (FrontendAdapter, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_frontend_adapter_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
    (AgentProvider, $impl:ty, $name:literal, $manifest:expr) => {
        $crate::__register_plugin_with_manifest_helper!(
            register_agent_provider_with_manifest,
            $impl,
            $name,
            $manifest
        );
    };
}
```

Then add the manifest-aware helper macro at the bottom of the file:

```rust
#[doc(hidden)]
#[macro_export]
macro_rules! __register_plugin_with_manifest_helper {
    ($method:ident, $impl:ty, $name:literal, $manifest:expr) => {
        /// Plugin entry point with manifest-aware registration.
        ///
        /// # Errors
        /// Returns [`cairn_core::contract::registry::PluginError`] when the
        /// name is invalid, the manifest fails to parse, the manifest
        /// disagrees with the registered name / contract / host version,
        /// or another plugin already holds this name.
        pub fn register(
            reg: &mut $crate::contract::registry::PluginRegistry,
        ) -> ::core::result::Result<(), $crate::contract::registry::PluginError> {
            let name = $crate::contract::registry::PluginName::new($name)?;
            let manifest =
                $crate::contract::manifest::PluginManifest::parse_toml($manifest)?;
            reg.$method(
                name,
                manifest,
                ::std::sync::Arc::new(<$impl as ::core::default::Default>::default()),
            )
        }
    };
}
```

- [ ] **Step 4: Update macro doc-comment**

In the doc-comment block at the top of `register_plugin!` (lines 11–69), add an additional `# Examples` section showing the 4-arg form:

```rust
///
/// # Manifest-aware form (preferred for bundled plugins)
///
/// ```
/// # use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities};
/// # use cairn_core::contract::version::{ContractVersion, VersionRange};
/// use cairn_core::register_plugin;
///
/// const MANIFEST_TOML: &str = r#"
/// name = "acme-store"
/// contract = "MemoryStore"
///
/// [contract_version_range.min]
/// major = 0
/// minor = 1
/// patch = 0
///
/// [contract_version_range.max_exclusive]
/// major = 0
/// minor = 2
/// patch = 0
/// "#;
///
/// #[derive(Default)]
/// struct MyStore;
///
/// #[async_trait::async_trait]
/// impl MemoryStore for MyStore {
///     fn name(&self) -> &str { "acme-store" }
///     fn capabilities(&self) -> &MemoryStoreCapabilities {
///         static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
///             fts: false,
///             vector: false,
///             graph_edges: false,
///             transactions: false,
///         };
///         &CAPS
///     }
///     fn supported_contract_versions(&self) -> VersionRange {
///         VersionRange::new(
///             ContractVersion::new(0, 1, 0),
///             ContractVersion::new(0, 2, 0),
///         )
///     }
/// }
///
/// register_plugin!(MemoryStore, MyStore, "acme-store", MANIFEST_TOML);
///
/// let mut reg = cairn_core::contract::registry::PluginRegistry::new();
/// register(&mut reg).expect("compatible plugin registers");
/// assert!(reg.parsed_manifest(
///     &cairn_core::contract::registry::PluginName::new("acme-store").unwrap()
/// ).is_some());
/// ```
```

- [ ] **Step 5: Convert `PluginManifest::parse_toml` errors to `PluginError`**

Open `crates/cairn-core/src/contract/manifest.rs` and confirm `parse_toml` returns `Result<Self, PluginError>` (it already does — see line 143 of the existing code). The `?` in the macro body relies on this; no change needed.

- [ ] **Step 6: Run test, expect pass**

Run: `cargo test -p cairn-core --test contract_registry` and `cargo test -p cairn-core --doc`
Expected: PASS for all tests in both targets.

- [ ] **Step 7: Lint clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-core/src/contract/macros.rs crates/cairn-core/tests/contract_registry.rs
git commit -m "feat(core): register_plugin! 4-arg manifest-aware form (#143)

The new fourth argument is the manifest TOML &'static str (typically
include_str!(\"../plugin.toml\")). The expansion parses the manifest,
forwards the parsed PluginManifest into register_<contract>_with_manifest,
and fails closed on parse / kind / version mismatch before construction.

Existing 3-arg call sites are kept for unit tests that don't need a
manifest; bundled plugins use the 4-arg form."
```

---

## Task 3: Conformance — types module

**Files:**
- Create: `crates/cairn-core/src/contract/conformance/mod.rs`
- Modify: `crates/cairn-core/src/contract/mod.rs`

- [ ] **Step 1: Write failing test** — temporarily add to `crates/cairn-core/src/contract/conformance/mod.rs` once it exists. For now, write the test stub in `crates/cairn-core/tests/conformance_types.rs` (new file):

```rust
//! Smoke test for conformance types — they exist and are constructible.

use cairn_core::contract::conformance::{CaseOutcome, CaseStatus, Tier};

#[test]
fn case_outcome_constructs_pending() {
    let outcome = CaseOutcome {
        id: "put_get_roundtrip",
        tier: Tier::Two,
        status: CaseStatus::Pending {
            reason: "real impl pending",
        },
    };
    assert_eq!(outcome.id, "put_get_roundtrip");
    assert_eq!(outcome.tier, Tier::Two);
    assert!(matches!(
        outcome.status,
        CaseStatus::Pending {
            reason: "real impl pending"
        }
    ));
}

#[test]
fn case_outcome_constructs_ok() {
    let outcome = CaseOutcome {
        id: "arc_pointer_stable",
        tier: Tier::One,
        status: CaseStatus::Ok,
    };
    assert_eq!(outcome.id, "arc_pointer_stable");
    assert_eq!(outcome.tier, Tier::One);
    assert!(matches!(outcome.status, CaseStatus::Ok));
}

#[test]
fn case_outcome_constructs_failed() {
    let outcome = CaseOutcome {
        id: "manifest_matches_host",
        tier: Tier::One,
        status: CaseStatus::Failed {
            message: "version mismatch".to_string(),
        },
    };
    assert_eq!(outcome.id, "manifest_matches_host");
    assert_eq!(outcome.tier, Tier::One);
    assert!(
        matches!(&outcome.status, CaseStatus::Failed { message } if message == "version mismatch")
    );
}
```

- [ ] **Step 2: Run test, expect failure**

Run: `cargo test -p cairn-core --test conformance_types`
Expected: compile error — module `conformance` does not exist.

- [ ] **Step 3: Create the conformance module**

Create `crates/cairn-core/src/contract/conformance/mod.rs`:

```rust
//! Two-tier conformance suite for plugins (brief §4.1).
//!
//! Tier-1 cases are pure trait-surface checks: they instantiate the plugin
//! against a fresh registry, verify name / version / manifest agreement,
//! and assert capability self-consistency. Every plugin must pass tier-1
//! to be considered conformant.
//!
//! Tier-2 cases exercise verb behaviour. They are stubbed `Pending` until
//! per-impl PRs land — at which point each contract module's tier-2 case
//! body is replaced with a real check.
//!
//! The suite lives in `cairn-core` (rather than `cairn-test-fixtures`)
//! because `cairn plugins verify` is a production code path. All cases
//! are pure functions: zero I/O, no adapter deps.
//!
//! Entry point: [`run_conformance_for_plugin`].
//!
//! See `docs/superpowers/specs/2026-04-25-plugin-host-list-verify-design.md`
//! §4 for the full design.

pub mod memory_store;
pub mod mcp_server;
pub mod sensor_ingress;
pub mod workflow_orchestrator;

use crate::contract::manifest::ContractKind;
use crate::contract::registry::{PluginError, PluginName, PluginRegistry};

/// Outcome of running a single conformance case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseOutcome {
    /// Stable case identifier (snake_case, brief-aligned).
    pub id: &'static str,
    /// Tier this case belongs to.
    pub tier: Tier,
    /// Case result.
    pub status: CaseStatus,
}

/// Conformance tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Always run; failure makes the plugin non-conformant.
    One,
    /// Verb-behaviour cases; stubbed `Pending` until per-impl PRs land.
    Two,
}

impl Tier {
    /// Numeric encoding used in JSON output (`1` or `2`).
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            Tier::One => 1,
            Tier::Two => 2,
        }
    }
}

/// Result of a single conformance case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaseStatus {
    /// Case ran to completion and the invariant held.
    Ok,
    /// Case body is a stub awaiting a real implementation.
    Pending {
        /// Static reason string explaining what's pending.
        reason: &'static str,
    },
    /// Case ran and the invariant did not hold.
    Failed {
        /// Human-readable failure message.
        message: String,
    },
}

impl CaseStatus {
    /// `true` iff this status counts as a tier-1 failure when running with
    /// `--strict` interpreting `Pending` as failure.
    #[must_use]
    pub fn is_failure(&self) -> bool {
        matches!(self, CaseStatus::Failed { .. })
    }

    /// Stable string discriminator used in JSON output.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            CaseStatus::Ok => "ok",
            CaseStatus::Pending { .. } => "pending",
            CaseStatus::Failed { .. } => "failed",
        }
    }
}

/// Run the full conformance suite (tier-1 + tier-2) against the plugin
/// identified by `name` in `registry`.
///
/// Returns an empty vec if the plugin is not registered. Otherwise the
/// returned vec contains tier-1 cases first, then tier-2 cases, in
/// declaration order.
///
/// # Errors
/// This function does not return `Result`; case-level errors are encoded
/// in `CaseStatus::Failed`. The only way this function "fails" is by
/// returning an empty vec when `name` is not in the registry.
#[must_use]
pub fn run_conformance_for_plugin(
    registry: &PluginRegistry,
    name: &PluginName,
) -> Vec<CaseOutcome> {
    let Some(manifest) = registry.parsed_manifest(name) else {
        return Vec::new();
    };
    match manifest.contract() {
        ContractKind::MemoryStore => memory_store::run(registry, name),
        ContractKind::WorkflowOrchestrator => workflow_orchestrator::run(registry, name),
        ContractKind::SensorIngress => sensor_ingress::run(registry, name),
        ContractKind::MCPServer => mcp_server::run(registry, name),
        // P0 ships no bundled plugins for these — verify returns empty.
        ContractKind::LLMProvider
        | ContractKind::FrontendAdapter
        | ContractKind::AgentProvider => Vec::new(),
    }
}

/// Internal helper: tier-1 `manifest_matches_host` case.
///
/// Calls [`crate::contract::manifest::PluginManifest::verify_compatible_with`]
/// with the plugin's runtime name, the manifest's declared contract kind,
/// and the host's `CONTRACT_VERSION` for that contract. The host version
/// is supplied by callers (per-contract `run` functions) so this helper
/// stays generic over contract kind.
pub(super) fn tier1_manifest_matches_host(
    registry: &PluginRegistry,
    name: &PluginName,
    host_version: crate::contract::version::ContractVersion,
) -> CaseOutcome {
    let Some(manifest) = registry.parsed_manifest(name) else {
        return CaseOutcome {
            id: "manifest_matches_host",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!("no manifest registered for plugin {name}"),
            },
        };
    };
    let result = manifest.verify_compatible_with(name, manifest.contract(), host_version);
    let status = match result {
        Ok(()) => CaseStatus::Ok,
        Err(PluginError::ContractMismatch { expected, actual }) => CaseStatus::Failed {
            message: format!("manifest contract {actual:?} does not match host {expected:?}"),
        },
        Err(PluginError::ManifestNameMismatch { expected, manifest }) => CaseStatus::Failed {
            message: format!(
                "manifest name {manifest} does not match registered name {expected}"
            ),
        },
        Err(PluginError::UnsupportedContractVersion {
            host, plugin_range, ..
        }) => CaseStatus::Failed {
            message: format!(
                "host CONTRACT_VERSION {host} is outside manifest range {plugin_range:?}"
            ),
        },
        Err(other) => CaseStatus::Failed {
            message: format!("verify_compatible_with failed: {other}"),
        },
    };
    CaseOutcome {
        id: "manifest_matches_host",
        tier: Tier::One,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_as_u8() {
        assert_eq!(Tier::One.as_u8(), 1);
        assert_eq!(Tier::Two.as_u8(), 2);
    }

    #[test]
    fn case_status_str() {
        assert_eq!(CaseStatus::Ok.as_str(), "ok");
        assert_eq!(
            CaseStatus::Pending {
                reason: "x"
            }
            .as_str(),
            "pending"
        );
        assert_eq!(
            CaseStatus::Failed {
                message: "x".to_string()
            }
            .as_str(),
            "failed"
        );
    }

    #[test]
    fn run_for_unregistered_plugin_is_empty() {
        let reg = PluginRegistry::new();
        let name = PluginName::new("does-not-exist").expect("valid");
        assert!(run_conformance_for_plugin(&reg, &name).is_empty());
    }
}
```

- [ ] **Step 4: Re-export from `contract/mod.rs`**

Edit `crates/cairn-core/src/contract/mod.rs`:

Add to the module list (after `pub mod agent_provider;` block around line 19):

```rust
pub mod conformance;
```

And to the flat re-exports section (around line 35, after `pub use manifest::...`):

```rust
pub use conformance::{CaseOutcome, CaseStatus, Tier, run_conformance_for_plugin};
```

- [ ] **Step 5: Create empty per-contract module files**

Create four empty files so the `pub mod` lines compile:

`crates/cairn-core/src/contract/conformance/memory_store.rs`:
```rust
//! Conformance cases for `MemoryStore` plugins (filled in Task 4).
```

`crates/cairn-core/src/contract/conformance/mcp_server.rs`:
```rust
//! Conformance cases for `MCPServer` plugins (filled in Task 4).
```

`crates/cairn-core/src/contract/conformance/sensor_ingress.rs`:
```rust
//! Conformance cases for `SensorIngress` plugins (filled in Task 4).
```

`crates/cairn-core/src/contract/conformance/workflow_orchestrator.rs`:
```rust
//! Conformance cases for `WorkflowOrchestrator` plugins (filled in Task 4).
```

- [ ] **Step 6: Run test, expect compile failure on per-contract `run` functions**

Run: `cargo test -p cairn-core --lib`
Expected: compile error — `memory_store::run`, `mcp_server::run`, `sensor_ingress::run`, `workflow_orchestrator::run` not found.

- [ ] **Step 7: Stub each per-contract `run` to satisfy the dispatch**

In each of the four files, append:

```rust
use crate::contract::conformance::CaseOutcome;
use crate::contract::registry::{PluginName, PluginRegistry};

/// Run the conformance suite for this contract. Filled in Task 4.
#[must_use]
pub fn run(_registry: &PluginRegistry, _name: &PluginName) -> Vec<CaseOutcome> {
    Vec::new()
}
```

- [ ] **Step 8: Run test, expect pass**

Run: `cargo test -p cairn-core` (lib + integration + doc)
Expected: all tests pass, including `conformance_types` integration test.

- [ ] **Step 9: Lint clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 10: Commit**

```bash
git add crates/cairn-core/src/contract/conformance \
        crates/cairn-core/src/contract/mod.rs \
        crates/cairn-core/tests/conformance_types.rs
git commit -m "feat(core): conformance type module skeleton (#143)

Adds CaseOutcome, CaseStatus, Tier and the run_conformance_for_plugin
dispatcher. Per-contract case modules ship empty 'run()' stubs returning
Vec::new() — bodies fill in the next task."
```

---

## Task 4: Conformance — tier-1 cases per contract

**Files:**
- Modify: `crates/cairn-core/src/contract/conformance/memory_store.rs`
- Modify: `crates/cairn-core/src/contract/conformance/mcp_server.rs`
- Modify: `crates/cairn-core/src/contract/conformance/sensor_ingress.rs`
- Modify: `crates/cairn-core/src/contract/conformance/workflow_orchestrator.rs`
- Test: `crates/cairn-core/tests/conformance_tier1.rs` (new)

- [ ] **Step 1: Write failing test** — create `crates/cairn-core/tests/conformance_tier1.rs`:

```rust
//! Tier-1 conformance: ensure the three core cases pass against a
//! well-formed stub plugin registered with a matching manifest.

use std::sync::Arc;

use cairn_core::contract::conformance::{
    CaseStatus, Tier, run_conformance_for_plugin,
};
use cairn_core::contract::manifest::PluginManifest;
use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities};
use cairn_core::contract::registry::{PluginName, PluginRegistry};
use cairn_core::contract::version::{ContractVersion, VersionRange};

const STORE_MANIFEST: &str = r#"
name = "stub-store"
contract = "MemoryStore"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0
"#;

#[derive(Default)]
struct StubStore;

#[async_trait::async_trait]
impl MemoryStore for StubStore {
    fn name(&self) -> &'static str {
        "stub-store"
    }
    fn capabilities(&self) -> &MemoryStoreCapabilities {
        // `Default::default()` is not const, so use an explicit literal.
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: false,
            vector: false,
            graph_edges: false,
            transactions: false,
        };
        &CAPS
    }
    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(ContractVersion::new(0, 1, 0), ContractVersion::new(0, 2, 0))
    }
}

#[test]
fn tier1_cases_pass_for_well_formed_memory_store() {
    let mut reg = PluginRegistry::new();
    let name = PluginName::new("stub-store").expect("valid");
    let manifest = PluginManifest::parse_toml(STORE_MANIFEST).expect("manifest parses");
    reg.register_memory_store_with_manifest(name.clone(), manifest, Arc::new(StubStore))
        .expect("registers");

    let outcomes = run_conformance_for_plugin(&reg, &name);

    let tier1: Vec<_> = outcomes.iter().filter(|o| o.tier == Tier::One).collect();
    assert_eq!(tier1.len(), 3, "expect 3 tier-1 cases");
    for outcome in &tier1 {
        assert!(
            matches!(outcome.status, CaseStatus::Ok),
            "tier-1 case {} must pass, got {:?}",
            outcome.id,
            outcome.status
        );
    }

    let ids: Vec<_> = tier1.iter().map(|o| o.id).collect();
    assert!(ids.contains(&"manifest_matches_host"));
    assert!(ids.contains(&"arc_pointer_stable"));
    assert!(ids.contains(&"capability_self_consistency_floor"));
}
```

Note: this test uses `MemoryStoreCapabilities::default()`. The struct already derives `Default` (verified at line 18 of `memory_store.rs`).

- [ ] **Step 2: Run test, expect failure**

Run: `cargo test -p cairn-core --test conformance_tier1`
Expected: compile passes (modules exist) but assertion fails — `tier1.len() == 0` because per-contract `run()` stubs return empty vecs.

- [ ] **Step 3: Implement `MemoryStore` tier-1 cases**

Replace `crates/cairn-core/src/contract/conformance/memory_store.rs` with:

```rust
//! Conformance cases for `MemoryStore` plugins.
//!
//! Tier-1 cases run against any registered `MemoryStore` plugin and assert
//! manifest/identity/version invariants. Tier-2 cases (verb behaviour)
//! return `Pending` until per-impl PRs replace the bodies.

use crate::contract::conformance::{
    CaseOutcome, CaseStatus, Tier, tier1_manifest_matches_host,
};
use crate::contract::memory_store::CONTRACT_VERSION;
use crate::contract::registry::{PluginName, PluginRegistry};

/// Run tier-1 + tier-2 cases for a `MemoryStore` plugin.
///
/// Returns an empty vec if no `MemoryStore` is registered under `name`.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.memory_store(name) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(6);

    // Tier 1
    out.push(tier1_manifest_matches_host(registry, name, CONTRACT_VERSION));
    out.push(tier1_arc_pointer_stable(registry, name, &plugin));
    out.push(tier1_capability_self_consistency_floor(&*plugin));

    // Tier 2 (stubs)
    out.push(CaseOutcome {
        id: "put_get_roundtrip",
        tier: Tier::Two,
        status: CaseStatus::Pending {
            reason: "real impl pending",
        },
    });
    out.push(CaseOutcome {
        id: "fts_query_returns_doc",
        tier: Tier::Two,
        status: CaseStatus::Pending {
            reason: "real impl pending",
        },
    });
    out.push(CaseOutcome {
        id: "vector_search_when_advertised",
        tier: Tier::Two,
        status: CaseStatus::Pending {
            reason: "real impl pending",
        },
    });

    out
}

fn tier1_arc_pointer_stable(
    registry: &PluginRegistry,
    name: &PluginName,
    plugin: &std::sync::Arc<dyn crate::contract::memory_store::MemoryStore>,
) -> CaseOutcome {
    let resolved = match registry.memory_store(name) {
        Some(p) => p,
        None => {
            return CaseOutcome {
                id: "arc_pointer_stable",
                tier: Tier::One,
                status: CaseStatus::Failed {
                    message: "lookup returned None for registered plugin".to_string(),
                },
            };
        }
    };
    let status = if std::sync::Arc::ptr_eq(plugin, &resolved) {
        CaseStatus::Ok
    } else {
        CaseStatus::Failed {
            message: "two lookups returned different Arcs".to_string(),
        }
    };
    CaseOutcome {
        id: "arc_pointer_stable",
        tier: Tier::One,
        status,
    }
}

fn tier1_capability_self_consistency_floor(
    plugin: &dyn crate::contract::memory_store::MemoryStore,
) -> CaseOutcome {
    // Floor: capabilities() must return without panic, name() non-empty,
    // supported_contract_versions() must accept the host CONTRACT_VERSION.
    let caps = plugin.capabilities();
    if plugin.name().is_empty() {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "plugin.name() returned empty string".to_string(),
            },
        };
    }
    if !plugin.supported_contract_versions().accepts(CONTRACT_VERSION) {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!(
                    "plugin does not accept host CONTRACT_VERSION {CONTRACT_VERSION}"
                ),
            },
        };
    }
    // Touch all four bool fields so a future panicking-getter regression
    // would surface here.
    let _ = (caps.fts, caps.vector, caps.graph_edges, caps.transactions);
    CaseOutcome {
        id: "capability_self_consistency_floor",
        tier: Tier::One,
        status: CaseStatus::Ok,
    }
}
```

- [ ] **Step 4: Implement `MCPServer` tier-1 cases**

Replace `crates/cairn-core/src/contract/conformance/mcp_server.rs` with:

```rust
//! Conformance cases for `MCPServer` plugins.

use crate::contract::conformance::{
    CaseOutcome, CaseStatus, Tier, tier1_manifest_matches_host,
};
use crate::contract::mcp_server::CONTRACT_VERSION;
use crate::contract::registry::{PluginName, PluginRegistry};

/// Run tier-1 + tier-2 cases for an `MCPServer` plugin.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.mcp_server(name) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(4);
    out.push(tier1_manifest_matches_host(registry, name, CONTRACT_VERSION));
    out.push(tier1_arc_pointer_stable(registry, name, &plugin));
    out.push(tier1_capability_self_consistency_floor(&*plugin));
    out.push(CaseOutcome {
        id: "initialize_and_list_tools",
        tier: Tier::Two,
        status: CaseStatus::Pending {
            reason: "real impl pending",
        },
    });
    out
}

fn tier1_arc_pointer_stable(
    registry: &PluginRegistry,
    name: &PluginName,
    plugin: &std::sync::Arc<dyn crate::contract::mcp_server::MCPServer>,
) -> CaseOutcome {
    let resolved = match registry.mcp_server(name) {
        Some(p) => p,
        None => {
            return CaseOutcome {
                id: "arc_pointer_stable",
                tier: Tier::One,
                status: CaseStatus::Failed {
                    message: "lookup returned None for registered plugin".to_string(),
                },
            };
        }
    };
    let status = if std::sync::Arc::ptr_eq(plugin, &resolved) {
        CaseStatus::Ok
    } else {
        CaseStatus::Failed {
            message: "two lookups returned different Arcs".to_string(),
        }
    };
    CaseOutcome {
        id: "arc_pointer_stable",
        tier: Tier::One,
        status,
    }
}

fn tier1_capability_self_consistency_floor(
    plugin: &dyn crate::contract::mcp_server::MCPServer,
) -> CaseOutcome {
    let caps = plugin.capabilities();
    if plugin.name().is_empty() {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "plugin.name() returned empty string".to_string(),
            },
        };
    }
    if !plugin.supported_contract_versions().accepts(CONTRACT_VERSION) {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!(
                    "plugin does not accept host CONTRACT_VERSION {CONTRACT_VERSION}"
                ),
            },
        };
    }
    let _ = (caps.stdio, caps.sse, caps.http_streamable, caps.extensions);
    CaseOutcome {
        id: "capability_self_consistency_floor",
        tier: Tier::One,
        status: CaseStatus::Ok,
    }
}
```

- [ ] **Step 5: Implement `SensorIngress` tier-1 cases**

Replace `crates/cairn-core/src/contract/conformance/sensor_ingress.rs` with:

```rust
//! Conformance cases for `SensorIngress` plugins.

use crate::contract::conformance::{
    CaseOutcome, CaseStatus, Tier, tier1_manifest_matches_host,
};
use crate::contract::registry::{PluginName, PluginRegistry};
use crate::contract::sensor_ingress::CONTRACT_VERSION;

/// Run tier-1 + tier-2 cases for a `SensorIngress` plugin.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.sensor_ingress_plugin(name) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(4);
    out.push(tier1_manifest_matches_host(registry, name, CONTRACT_VERSION));
    out.push(tier1_arc_pointer_stable(registry, name, &plugin));
    out.push(tier1_capability_self_consistency_floor(&*plugin));
    out.push(CaseOutcome {
        id: "emits_envelope_when_poked",
        tier: Tier::Two,
        status: CaseStatus::Pending {
            reason: "real impl pending",
        },
    });
    out
}

fn tier1_arc_pointer_stable(
    registry: &PluginRegistry,
    name: &PluginName,
    plugin: &std::sync::Arc<dyn crate::contract::sensor_ingress::SensorIngress>,
) -> CaseOutcome {
    let resolved = match registry.sensor_ingress_plugin(name) {
        Some(p) => p,
        None => {
            return CaseOutcome {
                id: "arc_pointer_stable",
                tier: Tier::One,
                status: CaseStatus::Failed {
                    message: "lookup returned None for registered plugin".to_string(),
                },
            };
        }
    };
    let status = if std::sync::Arc::ptr_eq(plugin, &resolved) {
        CaseStatus::Ok
    } else {
        CaseStatus::Failed {
            message: "two lookups returned different Arcs".to_string(),
        }
    };
    CaseOutcome {
        id: "arc_pointer_stable",
        tier: Tier::One,
        status,
    }
}

fn tier1_capability_self_consistency_floor(
    plugin: &dyn crate::contract::sensor_ingress::SensorIngress,
) -> CaseOutcome {
    let caps = plugin.capabilities();
    if plugin.name().is_empty() {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "plugin.name() returned empty string".to_string(),
            },
        };
    }
    if !plugin.supported_contract_versions().accepts(CONTRACT_VERSION) {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!(
                    "plugin does not accept host CONTRACT_VERSION {CONTRACT_VERSION}"
                ),
            },
        };
    }
    let _ = (caps.batches, caps.streaming, caps.consent_aware);
    CaseOutcome {
        id: "capability_self_consistency_floor",
        tier: Tier::One,
        status: CaseStatus::Ok,
    }
}
```

- [ ] **Step 6: Implement `WorkflowOrchestrator` tier-1 cases**

Replace `crates/cairn-core/src/contract/conformance/workflow_orchestrator.rs` with:

```rust
//! Conformance cases for `WorkflowOrchestrator` plugins.

use crate::contract::conformance::{
    CaseOutcome, CaseStatus, Tier, tier1_manifest_matches_host,
};
use crate::contract::registry::{PluginName, PluginRegistry};
use crate::contract::workflow_orchestrator::CONTRACT_VERSION;

/// Run tier-1 + tier-2 cases for a `WorkflowOrchestrator` plugin.
#[must_use]
pub fn run(registry: &PluginRegistry, name: &PluginName) -> Vec<CaseOutcome> {
    let Some(plugin) = registry.workflow_orchestrator(name) else {
        return Vec::new();
    };

    let mut out = Vec::with_capacity(4);
    out.push(tier1_manifest_matches_host(registry, name, CONTRACT_VERSION));
    out.push(tier1_arc_pointer_stable(registry, name, &plugin));
    out.push(tier1_capability_self_consistency_floor(&*plugin));
    out.push(CaseOutcome {
        id: "enqueue_then_complete",
        tier: Tier::Two,
        status: CaseStatus::Pending {
            reason: "real impl pending",
        },
    });
    out
}

fn tier1_arc_pointer_stable(
    registry: &PluginRegistry,
    name: &PluginName,
    plugin: &std::sync::Arc<dyn crate::contract::workflow_orchestrator::WorkflowOrchestrator>,
) -> CaseOutcome {
    let resolved = match registry.workflow_orchestrator(name) {
        Some(p) => p,
        None => {
            return CaseOutcome {
                id: "arc_pointer_stable",
                tier: Tier::One,
                status: CaseStatus::Failed {
                    message: "lookup returned None for registered plugin".to_string(),
                },
            };
        }
    };
    let status = if std::sync::Arc::ptr_eq(plugin, &resolved) {
        CaseStatus::Ok
    } else {
        CaseStatus::Failed {
            message: "two lookups returned different Arcs".to_string(),
        }
    };
    CaseOutcome {
        id: "arc_pointer_stable",
        tier: Tier::One,
        status,
    }
}

fn tier1_capability_self_consistency_floor(
    plugin: &dyn crate::contract::workflow_orchestrator::WorkflowOrchestrator,
) -> CaseOutcome {
    let caps = plugin.capabilities();
    if plugin.name().is_empty() {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: "plugin.name() returned empty string".to_string(),
            },
        };
    }
    if !plugin.supported_contract_versions().accepts(CONTRACT_VERSION) {
        return CaseOutcome {
            id: "capability_self_consistency_floor",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!(
                    "plugin does not accept host CONTRACT_VERSION {CONTRACT_VERSION}"
                ),
            },
        };
    }
    let _ = (caps.durable, caps.crash_safe, caps.cron_schedules);
    CaseOutcome {
        id: "capability_self_consistency_floor",
        tier: Tier::One,
        status: CaseStatus::Ok,
    }
}
```

- [ ] **Step 7: Run test, expect pass**

Run: `cargo test -p cairn-core` (lib + integration tests + doctests).
Expected: PASS, including `conformance_tier1` showing all three tier-1 cases as `Ok`.

- [ ] **Step 8: Lint clean**

Run: `cargo clippy -p cairn-core --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-core/src/contract/conformance \
        crates/cairn-core/tests/conformance_tier1.rs
git commit -m "feat(core): tier-1 conformance cases per contract (#143)

Each of the four bundled-plugin contracts (MemoryStore, MCPServer,
SensorIngress, WorkflowOrchestrator) gets:

  - tier-1 manifest_matches_host    (delegates to verify_compatible_with)
  - tier-1 arc_pointer_stable      (Arc::ptr_eq across two lookups)
  - tier-1 capability_self_consistency_floor (name non-empty, version
                                              accepts host, all cap
                                              fields readable)
  - tier-2 verb-behaviour stubs returning Pending('real impl pending')

LLMProvider/FrontendAdapter/AgentProvider have no bundled plugins in
P0 — verify returns an empty vec for them."
```

---

## Task 5: Bundled plugin — `cairn-store-sqlite`

**Files:**
- Create: `crates/cairn-store-sqlite/plugin.toml`
- Modify: `crates/cairn-store-sqlite/src/lib.rs`
- Modify: `crates/cairn-store-sqlite/Cargo.toml`
- Create: `crates/cairn-store-sqlite/tests/manifest_validates.rs`

- [ ] **Step 1: Write failing test** — create `crates/cairn-store-sqlite/tests/manifest_validates.rs`:

```rust
//! Integration: the bundled plugin.toml parses, validates against the
//! IDL JSON schema, and matches the host contract version + name.

use cairn_core::contract::manifest::{ContractKind, PluginManifest};
use cairn_core::contract::memory_store::CONTRACT_VERSION;
use cairn_core::contract::registry::PluginName;

#[test]
fn manifest_parses_and_matches_host() {
    let manifest =
        PluginManifest::parse_toml(cairn_store_sqlite::MANIFEST_TOML).expect("manifest parses");
    assert_eq!(manifest.name().as_str(), "cairn-store-sqlite");
    assert_eq!(manifest.contract(), ContractKind::MemoryStore);
    let expected = PluginName::new("cairn-store-sqlite").expect("valid");
    manifest
        .verify_compatible_with(&expected, ContractKind::MemoryStore, CONTRACT_VERSION)
        .expect("manifest matches host");
}

#[test]
fn register_populates_registry() {
    let mut reg = cairn_core::contract::registry::PluginRegistry::new();
    cairn_store_sqlite::register(&mut reg).expect("registers");
    let name = PluginName::new("cairn-store-sqlite").expect("valid");
    assert!(reg.memory_store(&name).is_some());
    assert!(reg.parsed_manifest(&name).is_some());
}
```

- [ ] **Step 2: Run test, expect failure**

Run: `cargo test -p cairn-store-sqlite --test manifest_validates`
Expected: compile error — `MANIFEST_TOML` and `register` not found in crate root.

- [ ] **Step 3: Create `plugin.toml`** — `crates/cairn-store-sqlite/plugin.toml`:

```toml
name = "cairn-store-sqlite"
contract = "MemoryStore"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
fts = false
vector = false
graph_edges = false
transactions = false
```

- [ ] **Step 4: Add a stub `MemoryStore` impl + `register()`**

Replace `crates/cairn-store-sqlite/src/lib.rs` with:

```rust
//! `SQLite` record store for Cairn (P0 scaffold).
//!
//! Schema, migrations, FTS5 and sqlite-vec integration arrive in
//! follow-up issues (#46 and later). For now this crate ships only the
//! plugin manifest, a stub `MemoryStore` impl with all capability flags
//! `false`, and a `register()` entry point so the host can include it
//! in `cairn plugins list/verify`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use cairn_core::contract::memory_store::{
    CONTRACT_VERSION, MemoryStore, MemoryStoreCapabilities,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-store-sqlite";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// P0 stub `MemoryStore`. All capability flags are `false`; verb methods
/// land with the storage implementation in #46.
#[derive(Default)]
pub struct SqliteMemoryStore;

#[async_trait::async_trait]
impl MemoryStore for SqliteMemoryStore {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MemoryStoreCapabilities {
        static CAPS: MemoryStoreCapabilities = MemoryStoreCapabilities {
            fts: false,
            vector: false,
            graph_edges: false,
            transactions: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(
            ContractVersion::new(0, 1, 0),
            ContractVersion::new(0, 2, 0),
        )
    }
}

// Compile-time guard: this crate's accepted range must include the host
// CONTRACT_VERSION. If we ever bump CONTRACT_VERSION without bumping the
// range, the const evaluation here panics at build.
const _: () = {
    let lo = (0u16, 1u16, 0u16);
    let hi = (0u16, 2u16, 0u16);
    let v = (
        CONTRACT_VERSION.major,
        CONTRACT_VERSION.minor,
        CONTRACT_VERSION.patch,
    );
    assert!(
        (v.0 > lo.0 || (v.0 == lo.0 && (v.1 > lo.1 || (v.1 == lo.1 && v.2 >= lo.2))))
            && (v.0 < hi.0 || (v.0 == hi.0 && (v.1 < hi.1 || (v.1 == hi.1 && v.2 < hi.2)))),
        "host CONTRACT_VERSION outside this crate's declared range"
    );
};

register_plugin!(MemoryStore, SqliteMemoryStore, "cairn-store-sqlite", MANIFEST_TOML);
```

- [ ] **Step 5: Update `Cargo.toml`** — `crates/cairn-store-sqlite/Cargo.toml`:

Two minimal edits to the existing file:

(a) Add `async-trait` to `[dependencies]` (the trait impl uses it). The block becomes:

```toml
[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
thiserror = { workspace = true }
# NOTE: rusqlite (with `bundled`) lands with the storage implementation in #46.
```

(b) Update `[package.metadata.cargo-machete]` to drop `cairn-core` (now used) and keep `thiserror`:

```toml
[package.metadata.cargo-machete]
ignored = ["thiserror"]
```

- [ ] **Step 6: Run test, expect pass**

Run: `cargo test -p cairn-store-sqlite`
Expected: PASS for both new tests.

- [ ] **Step 7: Lint clean**

Run: `cargo clippy -p cairn-store-sqlite --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Manifest schema validation**

Run: `cargo test -p cairn-idl --test plugin_manifest_validation` (existing test that validates every TOML in `crates/cairn-idl/schema/plugin/`).

If this test does not pick up the new `crates/cairn-store-sqlite/plugin.toml`, *and* if the existing test only globs the `cairn-idl` schema dir, defer real CI-level schema validation to the manifest-aware Rust parser (which is identical content). No additional action needed unless the existing test framework is a globbing one — confirm by reading `crates/cairn-idl/tests/plugin_manifest_validation.rs` first. If a glob exists, extend it to include `crates/*/plugin.toml`.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-store-sqlite
git commit -m "feat(store-sqlite): plugin.toml + stub register() (#143)

P0 scaffold for the bundled SQLite MemoryStore plugin. Registers a
stub SqliteMemoryStore with all capability flags false so 'cairn
plugins list' / 'verify' can include it. Real schema, FTS5, and
sqlite-vec integration land in #46.

A const-eval guard catches drift between the host CONTRACT_VERSION
and the manifest's declared accepted range at build time."
```

---

## Task 6: Bundled plugin — `cairn-mcp`

**Files:**
- Create: `crates/cairn-mcp/plugin.toml`
- Modify: `crates/cairn-mcp/src/lib.rs`
- Modify: `crates/cairn-mcp/Cargo.toml`
- Create: `crates/cairn-mcp/tests/manifest_validates.rs`

- [ ] **Step 1: Write failing test** — create `crates/cairn-mcp/tests/manifest_validates.rs`:

```rust
//! Integration: the bundled plugin.toml parses and matches host contract.

use cairn_core::contract::manifest::{ContractKind, PluginManifest};
use cairn_core::contract::mcp_server::CONTRACT_VERSION;
use cairn_core::contract::registry::PluginName;

#[test]
fn manifest_parses_and_matches_host() {
    let manifest =
        PluginManifest::parse_toml(cairn_mcp::MANIFEST_TOML).expect("manifest parses");
    assert_eq!(manifest.name().as_str(), "cairn-mcp");
    assert_eq!(manifest.contract(), ContractKind::MCPServer);
    let expected = PluginName::new("cairn-mcp").expect("valid");
    manifest
        .verify_compatible_with(&expected, ContractKind::MCPServer, CONTRACT_VERSION)
        .expect("manifest matches host");
}

#[test]
fn register_populates_registry() {
    let mut reg = cairn_core::contract::registry::PluginRegistry::new();
    cairn_mcp::register(&mut reg).expect("registers");
    let name = PluginName::new("cairn-mcp").expect("valid");
    assert!(reg.mcp_server(&name).is_some());
    assert!(reg.parsed_manifest(&name).is_some());
}
```

- [ ] **Step 2: Run test, expect failure**

Run: `cargo test -p cairn-mcp --test manifest_validates`
Expected: compile error.

- [ ] **Step 3: Create `plugin.toml`** — `crates/cairn-mcp/plugin.toml`:

```toml
name = "cairn-mcp"
contract = "MCPServer"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
stdio = false
sse = false
http_streamable = false
extensions = false
```

- [ ] **Step 4: Add stub impl + `register()`** — replace `crates/cairn-mcp/src/lib.rs`:

```rust
//! Cairn MCP adapter (P0 scaffold).
//!
//! P0: no transports yet — this crate ships only the plugin manifest,
//! a stub `MCPServer` impl with all capability flags `false`, and a
//! `register()` entry point. Real stdio + SSE wiring lands in #64.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use cairn_core::contract::mcp_server::{CONTRACT_VERSION, MCPServer, MCPServerCapabilities};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

/// Stable plugin name. Matches `name = ...` in `plugin.toml`.
pub const PLUGIN_NAME: &str = "cairn-mcp";

/// Plugin capability manifest TOML (parsed at registration time).
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// P0 stub `MCPServer`. All capability flags are `false`; transport
/// wiring lands in #64.
#[derive(Default)]
pub struct CairnMcpServer;

#[async_trait::async_trait]
impl MCPServer for CairnMcpServer {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &MCPServerCapabilities {
        static CAPS: MCPServerCapabilities = MCPServerCapabilities {
            stdio: false,
            sse: false,
            http_streamable: false,
            extensions: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(
            ContractVersion::new(0, 1, 0),
            ContractVersion::new(0, 2, 0),
        )
    }
}

const _: () = {
    let lo = (0u16, 1u16, 0u16);
    let hi = (0u16, 2u16, 0u16);
    let v = (
        CONTRACT_VERSION.major,
        CONTRACT_VERSION.minor,
        CONTRACT_VERSION.patch,
    );
    assert!(
        (v.0 > lo.0 || (v.0 == lo.0 && (v.1 > lo.1 || (v.1 == lo.1 && v.2 >= lo.2))))
            && (v.0 < hi.0 || (v.0 == hi.0 && (v.1 < hi.1 || (v.1 == hi.1 && v.2 < hi.2)))),
        "host CONTRACT_VERSION outside this crate's declared range"
    );
};

register_plugin!(MCPServer, CairnMcpServer, "cairn-mcp", MANIFEST_TOML);
```

- [ ] **Step 5: Update `Cargo.toml`** — `crates/cairn-mcp/Cargo.toml`:

Two minimal edits:

(a) Add `async-trait` to `[dependencies]`:

```toml
[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
```

(b) Leave `[package.metadata.cargo-machete] ignored = ["serde", "serde_json"]`
unchanged — those deps remain forward-declared until the MCP transport
work in #64 lands.

- [ ] **Step 6: Run test, expect pass**

Run: `cargo test -p cairn-mcp`
Expected: PASS.

- [ ] **Step 7: Lint clean**

Run: `cargo clippy -p cairn-mcp --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-mcp
git commit -m "feat(mcp): plugin.toml + stub register() (#143)

P0 scaffold for the bundled MCP plugin. Stub CairnMcpServer with all
transport flags false; real stdio + SSE wiring lands in #64."
```

---

## Task 7: Bundled plugin — `cairn-sensors-local`

**Files:**
- Create: `crates/cairn-sensors-local/plugin.toml`
- Modify: `crates/cairn-sensors-local/src/lib.rs`
- Modify: `crates/cairn-sensors-local/Cargo.toml`
- Create: `crates/cairn-sensors-local/tests/manifest_validates.rs`

- [ ] **Step 1: Write failing test** — create `crates/cairn-sensors-local/tests/manifest_validates.rs`:

```rust
//! Integration: the bundled plugin.toml parses and matches host contract.

use cairn_core::contract::manifest::{ContractKind, PluginManifest};
use cairn_core::contract::registry::PluginName;
use cairn_core::contract::sensor_ingress::CONTRACT_VERSION;

#[test]
fn manifest_parses_and_matches_host() {
    let manifest =
        PluginManifest::parse_toml(cairn_sensors_local::MANIFEST_TOML).expect("manifest parses");
    assert_eq!(manifest.name().as_str(), "cairn-sensors-local");
    assert_eq!(manifest.contract(), ContractKind::SensorIngress);
    let expected = PluginName::new("cairn-sensors-local").expect("valid");
    manifest
        .verify_compatible_with(&expected, ContractKind::SensorIngress, CONTRACT_VERSION)
        .expect("manifest matches host");
}

#[test]
fn register_populates_registry() {
    let mut reg = cairn_core::contract::registry::PluginRegistry::new();
    cairn_sensors_local::register(&mut reg).expect("registers");
    let name = PluginName::new("cairn-sensors-local").expect("valid");
    assert!(reg.sensor_ingress_plugin(&name).is_some());
    assert!(reg.parsed_manifest(&name).is_some());
}
```

- [ ] **Step 2: Run test, expect failure**

Run: `cargo test -p cairn-sensors-local --test manifest_validates`
Expected: compile error.

- [ ] **Step 3: Create `plugin.toml`** — `crates/cairn-sensors-local/plugin.toml`:

```toml
name = "cairn-sensors-local"
contract = "SensorIngress"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
batches = false
streaming = false
consent_aware = false
```

- [ ] **Step 4: Add stub impl + `register()`** — replace `crates/cairn-sensors-local/src/lib.rs`:

```rust
//! Local sensors for Cairn — IDE hook, terminal, clipboard, voice, screen.
//!
//! P0 scaffold: stub `SensorIngress` impl with all capability flags
//! `false`. Real capture lands per-sensor in #84 and follow-ups.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use cairn_core::contract::sensor_ingress::{
    CONTRACT_VERSION, SensorIngress, SensorIngressCapabilities,
};
use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::register_plugin;

/// Stable plugin name.
pub const PLUGIN_NAME: &str = "cairn-sensors-local";

/// Plugin capability manifest TOML.
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// P0 stub `SensorIngress`. All capability flags are `false`.
#[derive(Default)]
pub struct LocalSensorIngress;

#[async_trait::async_trait]
impl SensorIngress for LocalSensorIngress {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &SensorIngressCapabilities {
        static CAPS: SensorIngressCapabilities = SensorIngressCapabilities {
            batches: false,
            streaming: false,
            consent_aware: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(
            ContractVersion::new(0, 1, 0),
            ContractVersion::new(0, 2, 0),
        )
    }
}

const _: () = {
    let lo = (0u16, 1u16, 0u16);
    let hi = (0u16, 2u16, 0u16);
    let v = (
        CONTRACT_VERSION.major,
        CONTRACT_VERSION.minor,
        CONTRACT_VERSION.patch,
    );
    assert!(
        (v.0 > lo.0 || (v.0 == lo.0 && (v.1 > lo.1 || (v.1 == lo.1 && v.2 >= lo.2))))
            && (v.0 < hi.0 || (v.0 == hi.0 && (v.1 < hi.1 || (v.1 == hi.1 && v.2 < hi.2)))),
        "host CONTRACT_VERSION outside this crate's declared range"
    );
};

register_plugin!(SensorIngress, LocalSensorIngress, "cairn-sensors-local", MANIFEST_TOML);
```

- [ ] **Step 5: Update `Cargo.toml`** — `crates/cairn-sensors-local/Cargo.toml`:

Two minimal edits:

(a) Add `async-trait` to `[dependencies]`:

```toml
[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
tracing = { workspace = true }
```

(b) Update `[package.metadata.cargo-machete]` to drop `cairn-core` (now used):

```toml
[package.metadata.cargo-machete]
ignored = ["tracing"]
```

- [ ] **Step 6: Run test, expect pass**

Run: `cargo test -p cairn-sensors-local`
Expected: PASS.

- [ ] **Step 7: Lint clean**

Run: `cargo clippy -p cairn-sensors-local --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-sensors-local
git commit -m "feat(sensors-local): plugin.toml + stub register() (#143)

P0 scaffold for the bundled sensor plugin. Real per-sensor capture
lands in #84 and follow-ups."
```

---

## Task 8: Bundled plugin — `cairn-workflows`

**Files:**
- Create: `crates/cairn-workflows/plugin.toml`
- Modify: `crates/cairn-workflows/src/lib.rs`
- Modify: `crates/cairn-workflows/Cargo.toml`
- Create: `crates/cairn-workflows/tests/manifest_validates.rs`

- [ ] **Step 1: Write failing test** — create `crates/cairn-workflows/tests/manifest_validates.rs`:

```rust
//! Integration: the bundled plugin.toml parses and matches host contract.

use cairn_core::contract::manifest::{ContractKind, PluginManifest};
use cairn_core::contract::registry::PluginName;
use cairn_core::contract::workflow_orchestrator::CONTRACT_VERSION;

#[test]
fn manifest_parses_and_matches_host() {
    let manifest =
        PluginManifest::parse_toml(cairn_workflows::MANIFEST_TOML).expect("manifest parses");
    assert_eq!(manifest.name().as_str(), "cairn-workflows");
    assert_eq!(manifest.contract(), ContractKind::WorkflowOrchestrator);
    let expected = PluginName::new("cairn-workflows").expect("valid");
    manifest
        .verify_compatible_with(
            &expected,
            ContractKind::WorkflowOrchestrator,
            CONTRACT_VERSION,
        )
        .expect("manifest matches host");
}

#[test]
fn register_populates_registry() {
    let mut reg = cairn_core::contract::registry::PluginRegistry::new();
    cairn_workflows::register(&mut reg).expect("registers");
    let name = PluginName::new("cairn-workflows").expect("valid");
    assert!(reg.workflow_orchestrator(&name).is_some());
    assert!(reg.parsed_manifest(&name).is_some());
}
```

- [ ] **Step 2: Run test, expect failure**

Run: `cargo test -p cairn-workflows --test manifest_validates`
Expected: compile error.

- [ ] **Step 3: Create `plugin.toml`** — `crates/cairn-workflows/plugin.toml`:

```toml
name = "cairn-workflows"
contract = "WorkflowOrchestrator"

[contract_version_range.min]
major = 0
minor = 1
patch = 0

[contract_version_range.max_exclusive]
major = 0
minor = 2
patch = 0

[features]
durable = false
crash_safe = false
cron_schedules = false
```

- [ ] **Step 4: Add stub impl + `register()`** — replace `crates/cairn-workflows/src/lib.rs`:

```rust
//! Cairn background workflows host (P0 scaffold).
//!
//! P0: no runner yet — stub `WorkflowOrchestrator` with all capability
//! flags `false`. Tokio + SQLite-backed job table lands in #89.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use cairn_core::contract::version::{ContractVersion, VersionRange};
use cairn_core::contract::workflow_orchestrator::{
    CONTRACT_VERSION, WorkflowOrchestrator, WorkflowOrchestratorCapabilities,
};
use cairn_core::register_plugin;

/// Stable plugin name.
pub const PLUGIN_NAME: &str = "cairn-workflows";

/// Plugin capability manifest TOML.
pub const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// P0 stub `WorkflowOrchestrator`. All capability flags are `false`.
#[derive(Default)]
pub struct InProcessOrchestrator;

#[async_trait::async_trait]
impl WorkflowOrchestrator for InProcessOrchestrator {
    fn name(&self) -> &str {
        PLUGIN_NAME
    }

    fn capabilities(&self) -> &WorkflowOrchestratorCapabilities {
        static CAPS: WorkflowOrchestratorCapabilities = WorkflowOrchestratorCapabilities {
            durable: false,
            crash_safe: false,
            cron_schedules: false,
        };
        &CAPS
    }

    fn supported_contract_versions(&self) -> VersionRange {
        VersionRange::new(
            ContractVersion::new(0, 1, 0),
            ContractVersion::new(0, 2, 0),
        )
    }
}

const _: () = {
    let lo = (0u16, 1u16, 0u16);
    let hi = (0u16, 2u16, 0u16);
    let v = (
        CONTRACT_VERSION.major,
        CONTRACT_VERSION.minor,
        CONTRACT_VERSION.patch,
    );
    assert!(
        (v.0 > lo.0 || (v.0 == lo.0 && (v.1 > lo.1 || (v.1 == lo.1 && v.2 >= lo.2))))
            && (v.0 < hi.0 || (v.0 == hi.0 && (v.1 < hi.1 || (v.1 == hi.1 && v.2 < hi.2)))),
        "host CONTRACT_VERSION outside this crate's declared range"
    );
};

register_plugin!(
    WorkflowOrchestrator,
    InProcessOrchestrator,
    "cairn-workflows",
    MANIFEST_TOML
);
```

- [ ] **Step 5: Update `Cargo.toml`** — `crates/cairn-workflows/Cargo.toml`:

Two minimal edits:

(a) Add `async-trait` to `[dependencies]`:

```toml
[dependencies]
cairn-core = { workspace = true }
async-trait = { workspace = true }
tokio = { workspace = true, features = ["rt", "macros"] }
tracing = { workspace = true }
```

(b) Update `[package.metadata.cargo-machete]` to drop `cairn-core` (now used):

```toml
[package.metadata.cargo-machete]
ignored = ["tokio", "tracing"]
```

- [ ] **Step 6: Run test, expect pass**

Run: `cargo test -p cairn-workflows`
Expected: PASS.

- [ ] **Step 7: Lint clean**

Run: `cargo clippy -p cairn-workflows --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-workflows
git commit -m "feat(workflows): plugin.toml + stub register() (#143)

P0 scaffold for the bundled workflow orchestrator plugin. Tokio +
SQLite-backed job table lands in #89."
```

---

## Task 9: Workspace deps — add `clap`, `serde_json`, `insta`

**Files:**
- Modify: `Cargo.toml` (workspace)

- [ ] **Step 1: Add to `[workspace.dependencies]`**

Edit `Cargo.toml` `[workspace.dependencies]` block (lines 15–24). Append:

```toml
clap = { version = "4.5", features = ["derive", "wrap_help"] }
insta = { version = "1.40", features = ["json", "yaml"] }
```

`serde_json` is already present (line 17). No further changes here.

- [ ] **Step 2: Verify the workspace still builds**

Run: `cargo check --workspace --locked`
Expected: PASS (no consumers yet, but workspace metadata must be valid).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "build: add clap and insta as workspace deps (#143)

Used in the next task by cairn-cli for the plugins subcommand and
snapshot tests."
```

---

## Task 10: `cairn-cli` deps + `plugins::host::register_all()`

**Files:**
- Modify: `crates/cairn-cli/Cargo.toml`
- Create: `crates/cairn-cli/src/plugins/mod.rs`
- Create: `crates/cairn-cli/src/plugins/host.rs`
- Test: in-file `#[cfg(test)] mod tests`

- [ ] **Step 1: Update `crates/cairn-cli/Cargo.toml`**

Replace the `[dependencies]` block:

```toml
[dependencies]
cairn-core = { workspace = true }
cairn-mcp = { workspace = true }
cairn-store-sqlite = { workspace = true }
cairn-sensors-local = { workspace = true }
cairn-workflows = { workspace = true }
anyhow = { workspace = true }
clap = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
```

Drop the four bundled-adapter-crate names (`cairn-mcp`, `cairn-sensors-local`, `cairn-store-sqlite`, `cairn-workflows`) from the machete-ignored block — they become real call sites in `plugins/host.rs` this task. Keep `anyhow`, `clap`, `serde`, `serde_json` ignored until Tasks 11–13 wire their call sites:

```toml
# Forward-declared scaffold deps. anyhow + clap wired by Task 13's main.rs.
# serde + serde_json wired by Tasks 11/12's list/verify renderers.
[package.metadata.cargo-machete]
ignored = ["anyhow", "clap", "serde", "serde_json"]
```

- [ ] **Step 2: Create the `plugins` module skeleton** — `crates/cairn-cli/src/plugins/mod.rs`:

```rust
//! `cairn plugins` subcommand implementation.
//!
//! - [`host`] — wires bundled adapter crates into a `PluginRegistry`.
//! - [`list`] — `cairn plugins list` (Task 11).
//! - [`verify`] — `cairn plugins verify` (Task 12).

pub mod host;
pub mod list;
pub mod verify;
```

- [ ] **Step 3: Write failing test** — create `crates/cairn-cli/src/plugins/host.rs`:

```rust
//! Plugin host: assemble the live `PluginRegistry` from bundled crates.

use cairn_core::contract::registry::{PluginError, PluginRegistry};

/// Register every bundled plugin in alphabetical order. Returns the
/// populated `PluginRegistry` on success, or the first plugin error
/// encountered (fail-closed).
///
/// Bundled plugins (alphabetical):
/// - `cairn-mcp`              → `MCPServer`
/// - `cairn-sensors-local`    → `SensorIngress`
/// - `cairn-store-sqlite`     → `MemoryStore`
/// - `cairn-workflows`        → `WorkflowOrchestrator`
///
/// # Errors
/// Returns the first [`PluginError`] from any bundled plugin's
/// `register()` function. The host fails closed: a single bad plugin
/// aborts startup.
pub fn register_all() -> Result<PluginRegistry, PluginError> {
    let mut reg = PluginRegistry::new();
    cairn_mcp::register(&mut reg)?;
    cairn_sensors_local::register(&mut reg)?;
    cairn_store_sqlite::register(&mut reg)?;
    cairn_workflows::register(&mut reg)?;
    Ok(reg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::contract::registry::PluginName;

    #[test]
    fn register_all_succeeds_and_populates_four_plugins() {
        let reg = register_all().expect("bundled plugins register");

        for name in [
            "cairn-mcp",
            "cairn-sensors-local",
            "cairn-store-sqlite",
            "cairn-workflows",
        ] {
            let pn = PluginName::new(name).expect("valid");
            assert!(
                reg.parsed_manifest(&pn).is_some(),
                "plugin {name} must be registered"
            );
        }
    }

    #[test]
    fn register_all_returns_alphabetically_sorted_manifests() {
        let reg = register_all().expect("registers");
        let sorted: Vec<_> = reg
            .parsed_manifests_sorted()
            .into_iter()
            .map(|(n, _)| n.as_str().to_string())
            .collect();
        assert_eq!(
            sorted,
            vec![
                "cairn-mcp",
                "cairn-sensors-local",
                "cairn-store-sqlite",
                "cairn-workflows",
            ]
        );
    }
}
```

- [ ] **Step 4: Run test, expect compile failure**

`cairn-cli/src/main.rs` does not yet import the `plugins` module, so the host module won't be reachable from `main.rs` for the binary build. The test still runs against the lib target — except `cairn-cli` is currently a binary-only crate (`[[bin]]` in Cargo.toml, no `[lib]`).

Add a library target alongside the binary. Append to `crates/cairn-cli/Cargo.toml` (after the existing `[[bin]]` block):

```toml
[lib]
name = "cairn_cli"
path = "src/lib.rs"
```

Create `crates/cairn-cli/src/lib.rs`:

```rust
//! Library surface for the `cairn` binary. Shared between `main.rs` and
//! integration tests in `crates/cairn-cli/tests/`.
//!
//! Note: this crate is not published — only `cairn-cli`'s own `main.rs`
//! and test targets consume it. `expect()` with documented reasons is
//! tolerated here per CLAUDE.md §6.2 (bins/tests).

pub mod plugins;
```

- [ ] **Step 5: Re-run test**

Run: `cargo test -p cairn-cli --lib plugins::host`
Expected: PASS (both new tests).

- [ ] **Step 6: Lint clean**

Run: `cargo clippy -p cairn-cli --all-targets --locked -- -D warnings`
Expected: no warnings (note: `list` and `verify` modules are still empty stubs at this point but compile fine).

Add the empty stubs first to satisfy the `pub mod` lines:

`crates/cairn-cli/src/plugins/list.rs`:
```rust
//! `cairn plugins list` (filled in Task 11).
```

`crates/cairn-cli/src/plugins/verify.rs`:
```rust
//! `cairn plugins verify` (filled in Task 12).
```

Re-run clippy.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/Cargo.toml crates/cairn-cli/src/lib.rs \
        crates/cairn-cli/src/plugins
git commit -m "feat(cli): plugin host wiring (#143)

cairn-cli grows a [lib] target so plugin module code is reachable from
both main.rs and integration tests. host::register_all() calls each
bundled crate's register() in alphabetical order and fails closed on
the first error."
```

---

## Task 11: `plugins::list` — formatter + JSON

**Files:**
- Modify: `crates/cairn-cli/src/plugins/list.rs`

- [ ] **Step 1: Write failing test** — replace `crates/cairn-cli/src/plugins/list.rs` content with a test-first scaffold:

```rust
//! `cairn plugins list` — render registered plugins as a human table or JSON.

use cairn_core::contract::registry::PluginRegistry;

/// Render the registered plugins as a fixed-column ASCII table.
///
/// Columns: NAME, CONTRACT, VERSION-RANGE, SOURCE.
/// Rows are sorted alphabetically by plugin name. Capabilities are not
/// shown in the table — use `--json` for machine-readable detail.
#[must_use]
pub fn render_human(registry: &PluginRegistry) -> String {
    let _ = registry;
    String::new()
}

/// Render the registered plugins as a JSON document with full
/// capability detail.
#[must_use]
pub fn render_json(registry: &PluginRegistry) -> String {
    let _ = registry;
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::host::register_all;

    #[test]
    fn human_lists_all_four_bundled_plugins() {
        let reg = register_all().expect("registers");
        let text = render_human(&reg);
        assert!(text.contains("cairn-mcp"), "must list mcp");
        assert!(text.contains("cairn-sensors-local"), "must list sensors");
        assert!(text.contains("cairn-store-sqlite"), "must list store");
        assert!(text.contains("cairn-workflows"), "must list workflows");
        assert!(text.contains("MCPServer"));
        assert!(text.contains("MemoryStore"));
        assert!(text.contains("[0.1.0, 0.2.0)"));
        assert!(text.contains("bundled:cairn-store-sqlite"));
    }

    #[test]
    fn json_contains_capabilities() {
        let reg = register_all().expect("registers");
        let json = render_json(&reg);
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        let plugins = v["plugins"].as_array().expect("array");
        assert_eq!(plugins.len(), 4);
        // First plugin alphabetical = cairn-mcp.
        assert_eq!(plugins[0]["name"], "cairn-mcp");
        assert_eq!(plugins[0]["contract"], "MCPServer");
        assert_eq!(plugins[0]["source"], "bundled:cairn-mcp");
        assert!(plugins[0]["capabilities"].is_object());
    }
}
```

- [ ] **Step 2: Run test, expect failure**

Run: `cargo test -p cairn-cli --lib plugins::list`
Expected: tests fail (empty strings).

- [ ] **Step 3: Implement `render_human`**

Replace the `render_human` body:

```rust
#[must_use]
pub fn render_human(registry: &PluginRegistry) -> String {
    use std::fmt::Write;

    let rows: Vec<HumanRow> = registry
        .parsed_manifests_sorted()
        .into_iter()
        .map(|(name, manifest)| HumanRow {
            name: name.as_str().to_string(),
            contract: format!("{:?}", manifest.contract()),
            range: format!(
                "[{}, {})",
                manifest.contract_version_range().min,
                manifest.contract_version_range().max_exclusive
            ),
            source: format!("bundled:{}", name.as_str()),
        })
        .collect();

    // Static-width columns wide enough for current bundled plugin names
    // (longest contract = "WorkflowOrchestrator" = 20 chars; longest plugin
    // name = "cairn-sensors-local" = 19 chars; longest range = "[0.1.0, 0.2.0)" = 14
    // chars; longest source = "bundled:cairn-sensors-local" = 27 chars).
    // Width chosen so headers line up without dynamic measurement.
    let col_name = 22;
    let col_contract = 22;
    let col_range = 18;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<col_name$}{:<col_contract$}{:<col_range$}{}",
        "NAME", "CONTRACT", "VERSION-RANGE", "SOURCE"
    );
    for row in &rows {
        let _ = writeln!(
            out,
            "{:<col_name$}{:<col_contract$}{:<col_range$}{}",
            row.name, row.contract, row.range, row.source
        );
    }
    out
}

struct HumanRow {
    name: String,
    contract: String,
    range: String,
    source: String,
}
```

- [ ] **Step 4: Implement `render_json`**

Replace the `render_json` body:

```rust
#[must_use]
pub fn render_json(registry: &PluginRegistry) -> String {
    let plugins: Vec<_> = registry
        .parsed_manifests_sorted()
        .into_iter()
        .map(|(name, manifest)| {
            serde_json::json!({
                "name": name.as_str(),
                "contract": format!("{:?}", manifest.contract()),
                "contract_version_range": {
                    "min": manifest.contract_version_range().min.to_string(),
                    "max_exclusive": manifest.contract_version_range().max_exclusive.to_string(),
                },
                "source": format!("bundled:{}", name.as_str()),
                "capabilities": capabilities_for(registry, name),
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({"plugins": plugins}))
        .expect("json serialization is infallible for owned values")
}

fn capabilities_for(
    registry: &PluginRegistry,
    name: &cairn_core::contract::registry::PluginName,
) -> serde_json::Value {
    use cairn_core::contract::manifest::ContractKind;
    let Some(manifest) = registry.parsed_manifest(name) else {
        return serde_json::json!({});
    };
    match manifest.contract() {
        ContractKind::MemoryStore => registry.memory_store(name).map_or_else(
            || serde_json::json!({}),
            |p| {
                let c = p.capabilities();
                serde_json::json!({
                    "fts": c.fts,
                    "vector": c.vector,
                    "graph_edges": c.graph_edges,
                    "transactions": c.transactions,
                })
            },
        ),
        ContractKind::MCPServer => registry.mcp_server(name).map_or_else(
            || serde_json::json!({}),
            |p| {
                let c = p.capabilities();
                serde_json::json!({
                    "stdio": c.stdio,
                    "sse": c.sse,
                    "http_streamable": c.http_streamable,
                    "extensions": c.extensions,
                })
            },
        ),
        ContractKind::SensorIngress => registry.sensor_ingress_plugin(name).map_or_else(
            || serde_json::json!({}),
            |p| {
                let c = p.capabilities();
                serde_json::json!({
                    "batches": c.batches,
                    "streaming": c.streaming,
                    "consent_aware": c.consent_aware,
                })
            },
        ),
        ContractKind::WorkflowOrchestrator => registry.workflow_orchestrator(name).map_or_else(
            || serde_json::json!({}),
            |p| {
                let c = p.capabilities();
                serde_json::json!({
                    "durable": c.durable,
                    "crash_safe": c.crash_safe,
                    "cron_schedules": c.cron_schedules,
                })
            },
        ),
        ContractKind::LLMProvider
        | ContractKind::FrontendAdapter
        | ContractKind::AgentProvider => serde_json::json!({}),
    }
}
```

The `expect("json serialization is infallible for owned values")` is the
documented exception in CLAUDE.md §6.2: invariant must hold at runtime
because we're serializing an owned `serde_json::Value` tree with no
custom `Serialize` impls.

- [ ] **Step 5: Run test, expect pass**

Run: `cargo test -p cairn-cli --lib plugins::list`
Expected: PASS.

- [ ] **Step 6: Lint clean**

Run: `cargo clippy -p cairn-cli --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/src/plugins/list.rs
git commit -m "feat(cli): cairn plugins list human + JSON renderers (#143)"
```

---

## Task 12: `plugins::verify` — runner, exit codes, JSON

**Files:**
- Modify: `crates/cairn-cli/src/plugins/verify.rs`

- [ ] **Step 1: Write failing test** — replace `crates/cairn-cli/src/plugins/verify.rs`:

```rust
//! `cairn plugins verify` — run the conformance suite against every
//! registered plugin and emit a summary.

use cairn_core::contract::conformance::{CaseStatus, run_conformance_for_plugin};
use cairn_core::contract::registry::{PluginName, PluginRegistry};

/// Aggregated outcome of a verify run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// Per-plugin block of cases.
    pub plugins: Vec<PluginReport>,
    /// Counts.
    pub summary: Summary,
}

/// Per-plugin sub-report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginReport {
    /// Plugin name.
    pub name: String,
    /// Contract kind, formatted as `Debug` (matches `--json`).
    pub contract: String,
    /// Per-case outcomes (tier-1 first, then tier-2, declaration order).
    pub cases: Vec<cairn_core::contract::conformance::CaseOutcome>,
}

/// Summary counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    /// Cases with `CaseStatus::Ok`.
    pub ok: usize,
    /// Cases with `CaseStatus::Pending { .. }`.
    pub pending: usize,
    /// Cases with `CaseStatus::Failed { .. }`.
    pub failed: usize,
}

/// Build a `VerifyReport` by running conformance for every registered
/// plugin in alphabetical order.
#[must_use]
pub fn run(registry: &PluginRegistry) -> VerifyReport {
    let mut plugins = Vec::new();
    let mut summary = Summary::default();

    for (name, manifest) in registry.parsed_manifests_sorted() {
        let cases = run_conformance_for_plugin(registry, name);
        for c in &cases {
            match c.status {
                CaseStatus::Ok => summary.ok += 1,
                CaseStatus::Pending { .. } => summary.pending += 1,
                CaseStatus::Failed { .. } => summary.failed += 1,
            }
        }
        plugins.push(PluginReport {
            name: name.as_str().to_string(),
            contract: format!("{:?}", manifest.contract()),
            cases,
        });
    }

    VerifyReport { plugins, summary }
}

/// Exit code for the given report under the requested strictness.
///
/// - Default mode: exit 0 unless any case `Failed`.
/// - Strict mode: exit 0 only if every case is `Ok`.
///
/// Exit code constants:
/// - `0`  — success.
/// - `69` (`EX_UNAVAILABLE`) — at least one case failed (or pending in
///   strict mode).
#[must_use]
pub fn exit_code(report: &VerifyReport, strict: bool) -> u8 {
    if report.summary.failed > 0 {
        return 69;
    }
    if strict && report.summary.pending > 0 {
        return 69;
    }
    0
}

/// Render the report as human-readable text.
#[must_use]
pub fn render_human(report: &VerifyReport) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    for plugin in &report.plugins {
        let _ = writeln!(out, "{} ({})", plugin.name, plugin.contract);
        for case in &plugin.cases {
            let tier = match case.tier {
                cairn_core::contract::conformance::Tier::One => "tier-1",
                cairn_core::contract::conformance::Tier::Two => "tier-2",
            };
            let status = match &case.status {
                CaseStatus::Ok => "ok".to_string(),
                CaseStatus::Pending { reason } => format!("pending ({reason})"),
                CaseStatus::Failed { message } => format!("FAILED ({message})"),
            };
            let _ = writeln!(out, "  {tier} {:<40} {status}", case.id);
        }
        let _ = writeln!(out);
    }
    let _ = writeln!(
        out,
        "Summary: {} plugins, {} ok, {} pending, {} failed",
        report.plugins.len(),
        report.summary.ok,
        report.summary.pending,
        report.summary.failed,
    );
    out
}

/// Render the report as JSON.
#[must_use]
pub fn render_json(report: &VerifyReport) -> String {
    let plugins_json: Vec<_> = report
        .plugins
        .iter()
        .map(|plugin| {
            let cases: Vec<_> = plugin
                .cases
                .iter()
                .map(|c| {
                    let mut obj = serde_json::json!({
                        "id": c.id,
                        "tier": c.tier.as_u8(),
                        "status": c.status.as_str(),
                    });
                    match &c.status {
                        CaseStatus::Pending { reason } => {
                            obj["reason"] = serde_json::Value::String((*reason).to_string());
                        }
                        CaseStatus::Failed { message } => {
                            obj["message"] = serde_json::Value::String(message.clone());
                        }
                        CaseStatus::Ok => {}
                    }
                    obj
                })
                .collect();
            serde_json::json!({
                "name": plugin.name,
                "contract": plugin.contract,
                "cases": cases,
            })
        })
        .collect();

    serde_json::to_string_pretty(&serde_json::json!({
        "plugins": plugins_json,
        "summary": {
            "ok": report.summary.ok,
            "pending": report.summary.pending,
            "failed": report.summary.failed,
        }
    }))
    .expect("json serialization is infallible for owned values")
}

/// Convenience: also resolve a `PluginName` for callers that need it.
#[must_use]
pub fn resolve_name(raw: &str) -> Option<PluginName> {
    PluginName::new(raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::host::register_all;

    #[test]
    fn run_reports_four_plugins() {
        let reg = register_all().expect("registers");
        let report = run(&reg);
        assert_eq!(report.plugins.len(), 4);
        assert_eq!(report.summary.failed, 0, "no failures expected");
        // 4 plugins × 3 tier-1 = 12 ok minimum.
        assert!(report.summary.ok >= 12);
        assert!(report.summary.pending >= 4, "tier-2 stubs are pending");
    }

    #[test]
    fn exit_code_default_zero_with_pendings() {
        let reg = register_all().expect("registers");
        let report = run(&reg);
        assert_eq!(exit_code(&report, false), 0);
    }

    #[test]
    fn exit_code_strict_nonzero_with_pendings() {
        let reg = register_all().expect("registers");
        let report = run(&reg);
        assert_eq!(exit_code(&report, true), 69);
    }

    #[test]
    fn human_output_contains_every_plugin_and_summary() {
        let reg = register_all().expect("registers");
        let report = run(&reg);
        let text = render_human(&report);
        for n in [
            "cairn-mcp",
            "cairn-sensors-local",
            "cairn-store-sqlite",
            "cairn-workflows",
        ] {
            assert!(text.contains(n));
        }
        assert!(text.contains("Summary:"));
    }

    #[test]
    fn json_output_round_trips() {
        let reg = register_all().expect("registers");
        let report = run(&reg);
        let json = render_json(&report);
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["plugins"].as_array().unwrap().len(), 4);
        assert_eq!(v["summary"]["failed"], 0);
    }
}
```

- [ ] **Step 2: Run test, expect pass**

Run: `cargo test -p cairn-cli --lib plugins::verify`
Expected: PASS.

- [ ] **Step 3: Lint clean**

Run: `cargo clippy -p cairn-cli --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/src/plugins/verify.rs
git commit -m "feat(cli): cairn plugins verify runner + exit codes (#143)

Default mode exits 0 with pendings allowed; --strict flips pending
to failure. JSON output matches the spec's schema."
```

---

## Task 13: `cairn-cli/src/main.rs` — clap dispatch

**Files:**
- Modify: `crates/cairn-cli/src/main.rs`

- [ ] **Step 1: Write failing test** — defer all CLI-level testing to the snapshot tests in Task 14. Implementation comes first here because main.rs has no test surface of its own; snapshot tests verify the full dispatch.

- [ ] **Step 2: Replace `crates/cairn-cli/src/main.rs`**

```rust
//! `cairn` binary entry point.
//!
//! Adopts `clap` for the `plugins` subcommand. Verb subcommands
//! (`ingest`, `search`, …) remain stubs that exit 2 — they wire into
//! real verb-layer code in a follow-up issue.

use std::io::Write;
use std::process::ExitCode;

use cairn_cli::plugins;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "cairn", version, about = "Cairn — agent memory framework")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Manage and inspect bundled plugins.
    Plugins {
        #[command(subcommand)]
        action: PluginsAction,
    },
}

#[derive(Subcommand, Debug)]
enum PluginsAction {
    /// List loaded plugins.
    List {
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },
    /// Run the conformance suite against every loaded plugin.
    Verify {
        /// Treat tier-2 `pending` cases as failures.
        #[arg(long)]
        strict: bool,
        /// Emit JSON instead of a human-readable report.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // Clap prints the error itself; map its exit code through.
            let _ = e.print();
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayVersion => ExitCode::SUCCESS,
                _ => ExitCode::from(2),
            };
        }
    };

    match cli.command {
        None => {
            // No subcommand: clap printed nothing; emulate the previous scaffold's
            // help shape so existing test expectations don't drift more than needed.
            print_help();
            ExitCode::SUCCESS
        }
        Some(Commands::Plugins { action }) => run_plugins(action),
    }
}

fn run_plugins(action: PluginsAction) -> ExitCode {
    let registry = match plugins::host::register_all() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cairn plugins: startup failed — {e}");
            // EX_UNAVAILABLE
            return ExitCode::from(69);
        }
    };

    match action {
        PluginsAction::List { json } => {
            let mut stdout = std::io::stdout().lock();
            let text = if json {
                plugins::list::render_json(&registry)
            } else {
                plugins::list::render_human(&registry)
            };
            // Newline at end ensures the human table flushes cleanly.
            let _ = writeln!(stdout, "{}", text.trim_end_matches('\n'));
            ExitCode::SUCCESS
        }
        PluginsAction::Verify { strict, json } => {
            let report = plugins::verify::run(&registry);
            let text = if json {
                plugins::verify::render_json(&report)
            } else {
                plugins::verify::render_human(&report)
            };
            let mut stdout = std::io::stdout().lock();
            let _ = writeln!(stdout, "{}", text.trim_end_matches('\n'));
            ExitCode::from(plugins::verify::exit_code(&report, strict))
        }
    }
}

fn print_help() {
    println!("cairn {} — P0 scaffold", env!("CARGO_PKG_VERSION"));
    println!();
    println!("Subcommands:");
    println!("  cairn plugins list       List loaded plugins");
    println!("  cairn plugins verify     Run plugin conformance suite");
    println!();
    println!("Verbs (not yet implemented — every verb exits 2):");
    for v in [
        "ingest",
        "search",
        "retrieve",
        "summarize",
        "assemble_hot",
        "capture_trace",
        "lint",
        "forget",
    ] {
        println!("  cairn {v}");
    }
}
```

- [ ] **Step 3: Build and smoke-test by hand**

Run:
```
cargo build -p cairn-cli --locked
./target/debug/cairn plugins list
./target/debug/cairn plugins list --json
./target/debug/cairn plugins verify
echo "verify exit: $?"
./target/debug/cairn plugins verify --strict
echo "strict exit: $?"
```
Expected:
- `list` shows four rows alphabetical: cairn-mcp, cairn-sensors-local, cairn-store-sqlite, cairn-workflows.
- `list --json` emits valid JSON with 4 plugins.
- `verify` exits 0 (pending cases allowed).
- `verify --strict` exits 69.

- [ ] **Step 4: Run cargo check + clippy on workspace**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-cli/src/main.rs
git commit -m "feat(cli): clap dispatch for cairn plugins subcommand (#143)

Replaces the hand-rolled argv matcher with clap. Verb stubs still
exit 2 via the no-subcommand path; only --help, --version, and
'cairn plugins {list,verify}' are wired."
```

---

## Task 14: Snapshot tests for `list` and `verify`

**Files:**
- Modify: `crates/cairn-cli/Cargo.toml` (add `insta` dev-dep)
- Create: `crates/cairn-cli/tests/plugins_list_snapshot.rs`
- Create: `crates/cairn-cli/tests/plugins_verify_snapshot.rs`
- Create: `crates/cairn-cli/tests/snapshots/` (auto-generated by insta)

- [ ] **Step 1: Add `insta` as dev-dep**

Append to `[dev-dependencies]` in `crates/cairn-cli/Cargo.toml`:

```toml
insta = { workspace = true }
```

- [ ] **Step 2: Create `plugins_list_snapshot.rs`**

```rust
//! Snapshot test: `cairn plugins list` human + JSON outputs are stable.

use cairn_cli::plugins::{host::register_all, list};

#[test]
fn list_human_snapshot() {
    let reg = register_all().expect("registers");
    let text = list::render_human(&reg);
    insta::assert_snapshot!("plugins_list_human", text);
}

#[test]
fn list_json_snapshot() {
    let reg = register_all().expect("registers");
    let json = list::render_json(&reg);
    insta::assert_snapshot!("plugins_list_json", json);
}
```

- [ ] **Step 3: Create `plugins_verify_snapshot.rs`**

```rust
//! Snapshot test: `cairn plugins verify` human + JSON outputs are stable.

use cairn_cli::plugins::{host::register_all, verify};

#[test]
fn verify_human_snapshot() {
    let reg = register_all().expect("registers");
    let report = verify::run(&reg);
    let text = verify::render_human(&report);
    insta::assert_snapshot!("plugins_verify_human", text);
}

#[test]
fn verify_json_snapshot() {
    let reg = register_all().expect("registers");
    let report = verify::run(&reg);
    let json = verify::render_json(&report);
    insta::assert_snapshot!("plugins_verify_json", json);
}

#[test]
fn verify_default_mode_exit_zero() {
    let reg = register_all().expect("registers");
    let report = verify::run(&reg);
    assert_eq!(verify::exit_code(&report, false), 0);
}

#[test]
fn verify_strict_mode_exit_69_with_pendings() {
    let reg = register_all().expect("registers");
    let report = verify::run(&reg);
    assert_eq!(verify::exit_code(&report, true), 69);
}
```

- [ ] **Step 4: Run the tests once with `INSTA_UPDATE=auto` to create the initial snapshots**

Run: `INSTA_UPDATE=auto cargo test -p cairn-cli --test plugins_list_snapshot --test plugins_verify_snapshot`
Expected: PASS — snapshots are created in `crates/cairn-cli/tests/snapshots/`.

- [ ] **Step 5: Inspect the generated snapshots**

Read the four `.snap` files in `crates/cairn-cli/tests/snapshots/`. Confirm:
- Human list: 4 rows, alphabetical.
- JSON list: 4 plugins with capabilities objects.
- Human verify: each plugin block, summary line `4 plugins, ≥12 ok, ≥4 pending, 0 failed`.
- JSON verify: matches the schema in the spec.

If any snapshot looks wrong, fix the renderer; do not edit the snapshot by hand.

- [ ] **Step 6: Re-run without `INSTA_UPDATE`**

Run: `cargo test -p cairn-cli --test plugins_list_snapshot --test plugins_verify_snapshot`
Expected: PASS — snapshots match.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-cli/Cargo.toml \
        crates/cairn-cli/tests/plugins_list_snapshot.rs \
        crates/cairn-cli/tests/plugins_verify_snapshot.rs \
        crates/cairn-cli/tests/snapshots/
git commit -m "test(cli): insta snapshots for plugins list/verify (#143)"
```

---

## Task 15: Cargo-test wrapper that shells out to the binary

**Files:**
- Create: `crates/cairn-cli/tests/plugins_verify.rs`

- [ ] **Step 1: Create the test**

Create `crates/cairn-cli/tests/plugins_verify.rs`:

```rust
//! Integration: shell out to the built `cairn` binary and assert the
//! `plugins verify --json` output. This is the CI-protective wrapper:
//! it runs under `cargo nextest` regardless of any workflow-yaml drift.

use std::process::Command;

fn cairn_binary() -> std::path::PathBuf {
    // Cargo sets CARGO_BIN_EXE_<name> for every binary in the package.
    let raw = env!("CARGO_BIN_EXE_cairn");
    std::path::PathBuf::from(raw)
}

#[test]
fn plugins_verify_json_default_succeeds() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "verify", "--json"])
        .output()
        .expect("spawn cairn binary");

    assert!(
        output.status.success(),
        "cairn plugins verify --json must exit 0 in default mode; got {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(v["summary"]["failed"], 0, "no tier-1 failures expected");
    assert_eq!(
        v["plugins"].as_array().expect("plugins array").len(),
        4,
        "all four bundled plugins must be reported"
    );
}

#[test]
fn plugins_verify_strict_exits_69_with_pendings() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "verify", "--strict"])
        .output()
        .expect("spawn cairn binary");

    let code = output.status.code().expect("exit code present");
    assert_eq!(
        code, 69,
        "verify --strict must exit 69 while tier-2 cases are pending"
    );
}

#[test]
fn plugins_list_emits_alphabetical_rows() {
    let output = Command::new(cairn_binary())
        .args(["plugins", "list"])
        .output()
        .expect("spawn cairn binary");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");

    let mcp_idx = stdout.find("cairn-mcp").expect("mcp present");
    let sensors_idx = stdout
        .find("cairn-sensors-local")
        .expect("sensors present");
    let store_idx = stdout.find("cairn-store-sqlite").expect("store present");
    let workflows_idx = stdout.find("cairn-workflows").expect("workflows present");

    assert!(mcp_idx < sensors_idx);
    assert!(sensors_idx < store_idx);
    assert!(store_idx < workflows_idx);
}
```

This test relies on `serde_json` being a dev-dep — it's already a regular
dep of `cairn-cli` (Task 10), and Cargo allows tests to use regular deps.

- [ ] **Step 2: Run the test**

Run: `cargo test -p cairn-cli --test plugins_verify`
Expected: PASS.

- [ ] **Step 3: Lint clean**

Run: `cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-cli/tests/plugins_verify.rs
git commit -m "test(cli): shell out to built cairn binary for plugins verify (#143)

Catches regressions in the CLI dispatch + exit-code mapping under
'cargo nextest run', so workflow-yaml edits cannot silently drop
the verify gate."
```

---

## Task 16: CI integration — `.github/workflows/ci.yml`

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Read the current workflow**

Run: `cat .github/workflows/ci.yml`

Identify the step that runs `cargo nextest run --workspace` (or similar).

- [ ] **Step 2: Add `plugins verify` steps after the test step**

Insert these YAML stanzas immediately after the existing `cargo nextest run` step (preserve indentation under the matching job's `steps:` list):

```yaml
      - name: cairn plugins verify (default mode)
        run: cargo run -p cairn-cli --locked -- plugins verify
      - name: cairn plugins verify (json artifact)
        run: cargo run -p cairn-cli --locked -- plugins verify --json > plugins-verify.json
      - name: Upload plugins-verify.json
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: plugins-verify-${{ github.run_id }}
          path: plugins-verify.json
          retention-days: 14
```

The default `plugins verify` step uses non-strict mode so the build stays
green while tier-2 cases are `Pending`. The JSON artifact captures the
full report for review.

- [ ] **Step 3: Verify the workflow file is valid YAML**

Run: `python -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo OK`
Expected: `OK`.

(If `python` is not available, run `yq '.' .github/workflows/ci.yml > /dev/null` instead.)

- [ ] **Step 4: Smoke-test the new commands locally**

Run:
```
cargo run -p cairn-cli --locked -- plugins verify
cargo run -p cairn-cli --locked -- plugins verify --json > /tmp/plugins-verify.json
jq '.summary' /tmp/plugins-verify.json
```
Expected: human run prints summary + exit 0; JSON file parses with `jq`.

- [ ] **Step 5: Run full local verification checklist** (CLAUDE.md §8)

Run:
```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh
```
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run cairn plugins verify in CI + upload JSON artifact (#143)

Default-mode invocation gates the build on tier-1 conformance pass
across all bundled plugins. JSON artifact (plugins-verify-<run_id>.json)
captures the full per-case report for reviewer access."
```

---

## Task 17: Final integration sweep + PR

**Files:** none (sweep only)

- [ ] **Step 1: Run the full CLAUDE.md §8 verification checklist**

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --all-targets --locked
cargo nextest run --workspace --locked --no-fail-fast
cargo test --doc --workspace --locked
./scripts/check-core-boundary.sh

RUSTDOCFLAGS="-D warnings -D rustdoc::broken-intra-doc-links" \
  cargo doc --workspace --no-deps --document-private-items --locked

cargo deny check
cargo audit --deny warnings
cargo machete
```

Expected: every command exits 0. If `cargo machete` flags any leftover ignored entry, drop it from the relevant `[package.metadata.cargo-machete]`.

- [ ] **Step 2: Smoke the binary one last time**

```
cargo run -p cairn-cli --locked -- --version
cargo run -p cairn-cli --locked -- --help
cargo run -p cairn-cli --locked -- plugins list
cargo run -p cairn-cli --locked -- plugins list --json | jq '.plugins | length'
cargo run -p cairn-cli --locked -- plugins verify
echo "default exit: $?"
cargo run -p cairn-cli --locked -- plugins verify --strict
echo "strict exit: $?"
```

Expected:
- `--version` prints `cairn 0.0.1`.
- `--help` lists `plugins` subcommand and verb stubs.
- `plugins list --json | jq '.plugins | length'` prints `4`.
- Default verify exit is `0`; strict verify exit is `69`.

- [ ] **Step 3: Open the PR**

Push the branch and run:

```bash
gh pr create --title "feat(plugins): host wiring + cairn plugins list/verify (#143)" \
  --body "$(cat <<'EOF'
## Summary
- Live plugin host: bundled crates register through `register_plugin!` (now manifest-aware) into `PluginRegistry` at startup of `plugins`-subcommand commands.
- `cairn plugins list` (human + `--json`) and `cairn plugins verify` (`--strict`/`--json`).
- Two-tier conformance suite in `cairn-core::contract::conformance`: tier-1 pass for all four bundled stubs; tier-2 cases stub `Pending` until per-impl PRs land.
- CI runs `plugins verify` in default mode and uploads JSON artifact.
- `cargo`-test wrapper at `crates/cairn-cli/tests/plugins_verify.rs` keeps the verify gate in `cargo nextest`.

## Brief sections
- §4.0 Contracts
- §4.1 Plugin architecture
- §13.3 Commands

## Invariants touched (CLAUDE.md §4)
- 1 (harness-agnostic) — preserved; sensors stub keeps no harness assumptions.
- 4 (seven contracts, pure functions) — extended `cairn-core` with conformance helpers (pure).
- 6 (fail closed on capability) — `cairn plugins verify --strict` exits 69 on pendings; default mode keeps CI green during P0 build-out.
- 7 (`#![forbid(unsafe_code)]`) — preserved.

## Test plan
- [ ] `cargo nextest run --workspace --locked` passes locally.
- [ ] `cargo run -p cairn-cli -- plugins list --json | jq '.plugins | length'` returns 4.
- [ ] `cargo run -p cairn-cli -- plugins verify` exits 0.
- [ ] `cargo run -p cairn-cli -- plugins verify --strict` exits 69.
- [ ] CI step uploads `plugins-verify-<run_id>.json` artifact.
- [ ] `cargo deny check`, `cargo audit`, `cargo machete` all clean.

## Out of scope (follow-up issues)
- Real `MemoryStore` / `MCPServer` / `WorkflowOrchestrator` / `SensorIngress` impls (#46, #64, #84, #89).
- `LLMProvider` bundled plugin (no crate yet).
- `.cairn/config.yaml` loader and active-set selection.
- Verb dispatch invoking `register_all()` (lands with verb-layer issue).

Spec: `docs/superpowers/specs/2026-04-25-plugin-host-list-verify-design.md`
EOF
)"
```

- [ ] **Step 4: Mark plan complete**

The plan ends here. PR review then drives any follow-up adjustments.

---

## Self-Review Notes

**Spec coverage:**
- §3.1 crate diagram → Tasks 5–8 + 10.
- §3.2 hard-coded discovery → Task 10 (`register_all`).
- §3.3 manifest convention → Tasks 5–8 each create `plugin.toml` + `MANIFEST_TOML`.
- §3.4 registry extension → Task 1.
- §3.5 CLI clap structure → Task 13.
- §3.6 startup wiring scope → Task 13 (only `plugins` subcommand calls `register_all`).
- §4 conformance suite → Tasks 3–4.
- §5 CLI command outputs → Tasks 11, 12, 14.
- §6.1 macro extension → Task 2.
- §6.3 tests → unit tests in each task + Tasks 14, 15.
- §7 CI integration → Task 16.

**Type / signature consistency check:**
- `register_*_with_manifest(name, manifest, plugin)` argument order — used consistently across Tasks 1, 2, 5–8.
- `parsed_manifest(&PluginName) -> Option<&PluginManifest>` and `parsed_manifests_sorted()` — defined Task 1, used in Tasks 4, 11, 12.
- `CaseOutcome { id, tier, status }` — defined Task 3, used in Tasks 4, 12.
- `CaseStatus` variants `Ok`/`Pending { reason }`/`Failed { message }` — defined Task 3, matched in Tasks 4, 12.
- `Tier::as_u8()` — defined Task 3, used in Task 12 (`render_json`).
- `MANIFEST_TOML` const — defined per-crate in Tasks 5–8, consumed in Task 14 macro expansion.
- `PluginRegistry::sensor_ingress_plugin` — name confirmed against `crates/cairn-core/src/contract/registry.rs:320`.

**Out-of-scope guardrails:**
- No verb-layer code touched.
- No real adapter implementation logic added — every adapter trait method returns the static capability struct only.
- `cairn-test-fixtures` not on the production dep graph.
