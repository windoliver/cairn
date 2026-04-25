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
