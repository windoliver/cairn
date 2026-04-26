//! `cairn plugins verify` — run the conformance suite against every
//! registered plugin and emit a summary.

use cairn_core::contract::conformance::{
    CaseOutcome, CaseStatus, Tier, run_conformance_for_plugin,
};
use cairn_core::contract::registry::PluginRegistry;

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
    /// Contract kind, stable string from `ContractKind::as_static_str()`
    /// (matches `--json`). Same source as `plugins::list` for consistency.
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
        let mut cases = run_conformance_for_plugin(registry, name);
        // Defense-in-depth: if a future contract route forgets to emit any
        // case, synthesize a `Failed` so verify cannot exit 0 against a
        // plugin with zero coverage.
        if cases.is_empty() {
            cases.push(CaseOutcome {
                id: "no_cases_emitted",
                tier: Tier::One,
                status: CaseStatus::Failed {
                    message: format!(
                        "conformance runner returned zero cases for plugin {name}; \
                         this should never happen — file a bug"
                    ),
                },
            });
        }
        for c in &cases {
            match c.status {
                CaseStatus::Ok => summary.ok += 1,
                CaseStatus::Pending { .. } => summary.pending += 1,
                CaseStatus::Failed { .. } => summary.failed += 1,
            }
        }
        plugins.push(PluginReport {
            name: name.as_str().to_string(),
            contract: manifest.contract().as_static_str().to_string(),
            cases,
        });
    }

    // Coverage gate: every typed plugin registration must carry a manifest.
    // Plugins registered through the legacy 3-arg `register_plugin!` form
    // (no manifest) are still a valid unit-test path, but `verify` must
    // surface them as Failed so they cannot pass the CI gate by being
    // invisible to `parsed_manifests_sorted`.
    for (orphan_name, contract_label) in registry.typed_plugins_without_manifests() {
        let case = CaseOutcome {
            id: "manifest_present_for_typed_registration",
            tier: Tier::One,
            status: CaseStatus::Failed {
                message: format!(
                    "typed {contract_label} plugin {orphan_name} has no parsed manifest; \
                     register through the manifest-aware path"
                ),
            },
        };
        summary.failed += 1;
        plugins.push(PluginReport {
            name: orphan_name.as_str().to_string(),
            contract: contract_label.to_string(),
            cases: vec![case],
        });
    }

    // Coverage gate: an empty registry means no plugins were verified at
    // all. This must fail closed — otherwise `cairn plugins verify` would
    // exit 0 in a misconfigured runtime where `register_all` ran but
    // produced no plugins (e.g. all bundled crates compiled out).
    if plugins.is_empty() {
        summary.failed += 1;
        plugins.push(PluginReport {
            name: "<registry>".to_string(),
            contract: "<none>".to_string(),
            cases: vec![CaseOutcome {
                id: "registry_has_at_least_one_plugin",
                tier: Tier::One,
                status: CaseStatus::Failed {
                    message: "PluginRegistry has zero plugins; verify cannot \
                              certify an empty host"
                        .to_string(),
                },
            }],
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
        // 4 plugins × 4 tier-1 cases (manifest_matches_host,
        // arc_pointer_stable, capability_self_consistency_floor,
        // manifest_features_match_capabilities) = 16 ok minimum.
        assert!(report.summary.ok >= 16);
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
    fn empty_registry_fails_closed() {
        let reg = cairn_core::contract::registry::PluginRegistry::new();
        let report = run(&reg);
        assert_eq!(report.plugins.len(), 1, "synthetic registry-empty plugin");
        assert!(report.summary.failed >= 1);
        assert_eq!(exit_code(&report, false), 69);
        assert_eq!(exit_code(&report, true), 69);
    }

    #[test]
    fn typed_registration_without_manifest_is_failed() {
        use cairn_core::contract::memory_store::{MemoryStore, MemoryStoreCapabilities};
        use cairn_core::contract::registry::{PluginName, PluginRegistry};
        use cairn_core::contract::version::{ContractVersion, VersionRange};

        #[derive(Default)]
        struct BareStore;

        #[async_trait::async_trait]
        impl MemoryStore for BareStore {
            fn name(&self) -> &'static str {
                "bare-store"
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

        let mut reg = PluginRegistry::new();
        let name = PluginName::new("bare-store").expect("valid");
        // Bare register (no manifest) — the legacy 3-arg path. Must still
        // be visible to verify as a coverage failure.
        reg.register_memory_store(name, std::sync::Arc::new(BareStore))
            .expect("registers");

        let report = run(&reg);
        assert_eq!(report.plugins.len(), 1);
        assert_eq!(report.plugins[0].name, "bare-store");
        let case = &report.plugins[0].cases[0];
        assert_eq!(case.id, "manifest_present_for_typed_registration");
        assert!(matches!(case.status, CaseStatus::Failed { .. }));
        assert_eq!(exit_code(&report, false), 69);
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
