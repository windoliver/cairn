//! Plugin host: assemble the live `PluginRegistry` from bundled crates.

use cairn_core::contract::registry::{PluginError, PluginRegistry};
use std::path::Path;

/// Register every bundled plugin in alphabetical order. Returns the
/// populated `PluginRegistry` on success, or the first plugin error
/// encountered (fail-closed).
///
/// **Capability-discovery only.** This path uses each plugin's
/// macro-generated `register()` entry, which constructs the
/// implementation via `Default::default()`. For `cairn-store-sqlite`
/// that produces a *probe* instance: it advertises the manifest and
/// capabilities but rejects every storage-touching call. CLI commands
/// like `plugins list` and `plugins verify` only need probe-level
/// metadata, so this entry point is sufficient for them.
///
/// Production code paths that need to actually read or write the
/// store must use [`register_all_runtime`] instead, which opens a real
/// on-disk database before registering.
///
/// Bundled plugins (alphabetical):
/// - `cairn-mcp`              → `MCPServer`
/// - `cairn-sensors-local`    → `SensorIngress`
/// - `cairn-store-sqlite`     → `MemoryStore` (**probe**)
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

/// Errors from [`register_all_runtime`]: either a generic
/// [`PluginError`] from one of the macro-registered plugins, or a
/// [`cairn_store_sqlite::RegisterRuntimeError`] from opening / migrating /
/// registering the on-disk `SQLite` store.
#[derive(Debug, thiserror::Error)]
pub enum RegisterRuntimeError {
    /// Bundled-plugin macro registration failed.
    #[error("plugin register: {0}")]
    Plugin(#[from] PluginError),
    /// Opening or registering the on-disk `SQLite` store failed.
    #[error("store runtime: {0}")]
    Store(#[from] cairn_store_sqlite::RegisterRuntimeError),
}

/// Production wiring: register every bundled plugin AND open the
/// `SQLite` store at `store_path`, registering it as the live
/// `MemoryStore` Arc that resolves through the registry.
///
/// Use this from any code path that needs to actually read or write
/// records (every verb except `status`/`plugins list`/`plugins verify`).
///
/// # Errors
/// Returns [`RegisterRuntimeError`] if any plugin registration fails or
/// the `SQLite` store cannot be opened / migrated / registered.
pub async fn register_all_runtime(
    store_path: &Path,
) -> Result<PluginRegistry, RegisterRuntimeError> {
    let mut reg = PluginRegistry::new();
    cairn_mcp::register(&mut reg)?;
    cairn_sensors_local::register(&mut reg)?;
    cairn_store_sqlite::register_runtime(&mut reg, store_path).await?;
    cairn_workflows::register(&mut reg)?;
    Ok(reg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_core::contract::memory_store::ListQuery;
    use cairn_core::contract::registry::PluginName;
    use cairn_core::domain::{Principal, identity::Identity};
    use tempfile::tempdir;

    #[tokio::test]
    async fn register_all_runtime_resolves_a_real_on_disk_store() {
        // Regression for the round-7..9 review thread: the bundled host
        // must wire the real on-disk `SQLite` store, not a probe. After
        // calling `register_all_runtime`, resolving `cairn-store-sqlite`
        // from the registry must yield a store whose read methods
        // succeed against the database created at `store_path`.
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("cairn.db");
        let reg = register_all_runtime(&path)
            .await
            .expect("runtime registration succeeds");

        let name = PluginName::new("cairn-store-sqlite").expect("valid");
        let store = reg
            .memory_store(&name)
            .expect("registry returns the live MemoryStore Arc");

        // The probe rejects `list` with `StoreError::Invariant`; a real
        // on-disk store returns an empty `ListResult` for an empty db.
        let id = Identity::parse("usr:bootstrap").expect("valid");
        let q = ListQuery::new(Principal::from_identity(id));
        let result = store
            .list(&q)
            .await
            .expect("list against real store must succeed");
        assert_eq!(result.rows.len(), 0);
        assert_eq!(result.hidden, 0);
    }

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
