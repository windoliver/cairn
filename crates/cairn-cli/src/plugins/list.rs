//! `cairn plugins list` — render registered plugins as a human table or JSON.

use cairn_core::contract::registry::PluginRegistry;

/// Render the registered plugins as a fixed-column ASCII table.
///
/// Columns: NAME, CONTRACT, VERSION-RANGE, SOURCE.
/// Rows are sorted alphabetically by plugin name. Capabilities are not
/// shown in the table — use `--json` for machine-readable detail.
#[must_use]
pub fn render_human(registry: &PluginRegistry) -> String {
    use std::fmt::Write;

    let rows: Vec<HumanRow> = registry
        .parsed_manifests_sorted()
        .into_iter()
        .map(|(name, manifest)| HumanRow {
            name: name.as_str().to_string(),
            contract: manifest.contract().as_static_str().to_string(),
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
        "{:<col_name$}{:<col_contract$}{:<col_range$}SOURCE",
        "NAME", "CONTRACT", "VERSION-RANGE",
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

/// Render the registered plugins as a JSON document with full
/// capability detail.
#[must_use]
pub fn render_json(registry: &PluginRegistry) -> String {
    let plugins: Vec<_> = registry
        .parsed_manifests_sorted()
        .into_iter()
        .map(|(name, manifest)| {
            serde_json::json!({
                "name": name.as_str(),
                "contract": manifest.contract().as_static_str(),
                "contract_version_range": {
                    "min": manifest.contract_version_range().min.to_string(),
                    "max_exclusive": manifest.contract_version_range().max_exclusive.to_string(),
                },
                "source": format!("bundled:{}", name.as_str()),
                "capabilities": capabilities_for(registry, name),
            })
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({ "plugins": plugins }))
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
        // `LLMProvider`, `FrontendAdapter`, and `AgentProvider` have no
        // bundled implementations yet, so their capabilities render as
        // `{}`. `ContractKind` is `#[non_exhaustive]`; the trailing `_`
        // arm is a safety net for future variants until this renderer
        // learns about them.
        ContractKind::LLMProvider
        | ContractKind::FrontendAdapter
        | ContractKind::AgentProvider
        | _ => serde_json::json!({}),
    }
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
