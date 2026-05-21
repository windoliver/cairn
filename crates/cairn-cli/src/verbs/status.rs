//! `cairn status` handler — capability discovery (§8.0.a).
//!
//! Returns the contract version, advertised capabilities, and server info.
//! For P0 (no daemon), a fresh incarnation ULID is minted per invocation.
//! When the store adapter lands, read the incarnation from the daemon table.
//!
//! Capabilities are advertised only when the runtime can honor them
//! end-to-end. The IDL declares `cairn.mcp.v1.policy_trace` (#95) and
//! store-driven search / retrieve / forget mode capabilities; verb
//! runtime emits the keyword/semantic search capabilities once the
//! store is wired and gates the others (#9 / #61 / #62) until each is
//! honored. Advertising a capability the runtime cannot back would
//! mislead clients that negotiate from `status.capabilities`.

use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use cairn_core::config::{AgentProviderKind, CairnConfig, EmbeddingModelKind, ScreenBackend};
use cairn_core::domain::identity::keys::VaultId;
use cairn_core::domain::{BudgetObservation, LocalSensorName, SensorGateReason};
use cairn_core::generated::common::Capabilities;
use cairn_core::generated::status::{
    StatusResponse, StatusResponseHealth, StatusResponseHealthAuthorityDb,
    StatusResponseHealthAuthorityDbState, StatusResponseHealthNexusProjection,
    StatusResponseHealthNexusProjectionState, StatusResponseMcpGraphTools,
    StatusResponseMcpGraphToolsProbeBasis, StatusResponseMcpGraphToolsReason,
    StatusResponseMcpGraphToolsState, StatusResponseSensors, StatusResponseSensorsLocal,
    StatusResponseSensorsLocalBudget, StatusResponseSensorsLocalConsent,
    StatusResponseSensorsLocalGate, StatusResponseSensorsLocalLastDropReason,
    StatusResponseSensorsLocalRetention, StatusResponseSensorsLocalSensor,
    StatusResponseSensorsScreen, StatusResponseSensorsScreenBackend,
    StatusResponseSensorsScreenDegradation, StatusResponseSensorsScreenDegradationCode,
    StatusResponseSensorsScreenMode, StatusResponseSensorsScreenOcrEngine,
    StatusResponseSensorsScreenPermission, StatusResponseSensorsScreenState,
    StatusResponseServerInfo,
};
use cairn_core::pipeline::dispatch::{DefaultRegistry, pipeline_dispatch_advertisement};
use cairn_sensors_local::screen::{
    self, ResolvedScreenOcrEngine, ScreenDegradationCode, ScreenMode, ScreenPermission,
    ScreenProbe, ScreenState,
};

use crate::nexus::{self, ProjectionStatusState};

use super::envelope::{emit_json, new_operation_id};

const CLI_CONTRACT_PHASE: cairn_core::status::Phase = cairn_core::status::Phase::V0_2;

fn authority_db_health(vault_path: &Path) -> StatusResponseHealthAuthorityDb {
    let db_path = vault_path.join(".cairn/cairn.db");
    let path = db_path.display().to_string();
    if !db_path.exists() {
        return StatusResponseHealthAuthorityDb {
            state: StatusResponseHealthAuthorityDbState::Missing,
            path,
            reason: None,
        };
    }

    let conn = match rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(conn) => conn,
        Err(err) => {
            return StatusResponseHealthAuthorityDb {
                state: StatusResponseHealthAuthorityDbState::Unavailable,
                path,
                reason: Some(err.to_string()),
            };
        }
    };

    match conn.query_row("PRAGMA schema_version", [], |_| Ok(())) {
        Ok(()) => StatusResponseHealthAuthorityDb {
            state: StatusResponseHealthAuthorityDbState::Healthy,
            path,
            reason: None,
        },
        Err(err) => StatusResponseHealthAuthorityDb {
            state: StatusResponseHealthAuthorityDbState::Unavailable,
            path,
            reason: Some(err.to_string()),
        },
    }
}

fn nexus_projection_health(
    vault_path: &Path,
    config: &CairnConfig,
) -> StatusResponseHealthNexusProjection {
    let projection = nexus::evaluate_projection_status(vault_path, config);
    let state = match projection.state {
        ProjectionStatusState::Disabled => StatusResponseHealthNexusProjectionState::Disabled,
        ProjectionStatusState::Healthy => StatusResponseHealthNexusProjectionState::Healthy,
        ProjectionStatusState::Degraded => StatusResponseHealthNexusProjectionState::Degraded,
    };
    StatusResponseHealthNexusProjection {
        state,
        data_dir: projection.data_dir.map(|path| path.display().to_string()),
        endpoint: projection.endpoint,
        projection_detail: None,
        reason: projection.reason,
    }
}

fn render_projection_human(projection: &StatusResponseHealthNexusProjection) -> String {
    match projection.state {
        StatusResponseHealthNexusProjectionState::Disabled => "disabled".to_owned(),
        StatusResponseHealthNexusProjectionState::Healthy => "healthy".to_owned(),
        StatusResponseHealthNexusProjectionState::Degraded => {
            projection.reason.as_ref().map_or_else(
                || "degraded".to_owned(),
                |reason| format!("degraded ({reason})"),
            )
        }
        _ => "unknown".to_owned(),
    }
}

fn render_authority_human(authority: &StatusResponseHealthAuthorityDb) -> String {
    match authority.state {
        StatusResponseHealthAuthorityDbState::Healthy => "healthy".to_owned(),
        StatusResponseHealthAuthorityDbState::Missing => "missing".to_owned(),
        StatusResponseHealthAuthorityDbState::Unavailable => authority.reason.as_ref().map_or_else(
            || "unavailable".to_owned(),
            |reason| format!("unavailable ({reason})"),
        ),
        _ => "unknown".to_owned(),
    }
}

/// Outcome of probing `<vault>/.cairn/vault.id` for the capability gate.
#[derive(Debug, PartialEq, Eq)]
pub enum VaultBinding {
    /// Sentinel exists and parses as a valid `VaultId`.
    Bound,
    /// Sentinel is absent — directory is not a Cairn vault. Fail-closed
    /// at the empty capability list, matching `Sdk::new`.
    Unbound,
    /// Sentinel exists but is not a valid `VaultId` (empty file, garbage,
    /// directory-typed path, …) or could not be read because of an I/O
    /// error other than `NotFound`. Surface as an operator-visible config
    /// error so a damaged vault is not silently treated as a fresh one.
    Invalid(String),
}

/// Probe the vault-binding sentinel without performing other I/O.
///
/// Mirrors the validation `vault/bootstrap.rs::preflight_vault_id` runs
/// on its file-only path:
/// - `vault.id` parses as a `VaultId` → `Bound`.
/// - `vault.id` is absent and no binding sentinels exist → `Unbound`
///   (directory is not a Cairn vault).
/// - `vault.id` is absent but `.cairn/vault.binding{,.pending}` exists
///   → `Invalid`. The identity layer marked this vault as bound, so the
///   missing id is a corruption that must be recovered, not a fresh
///   directory we can silently treat as unbound.
/// - `vault.id` exists but does not parse, or the read fails for a
///   reason other than `NotFound` → `Invalid` with the underlying
///   error.
///
/// Cross-DB validation (`vault_meta` row present, vault id matches
/// committed identity) is intentionally out of scope here — that
/// requires opening the `SQLite` store, which `status` must not do.
#[must_use]
pub fn probe_vault_binding(vault_root: &Path) -> VaultBinding {
    let sentinel = vault_root.join(".cairn").join("vault.id");
    match std::fs::read_to_string(&sentinel) {
        Ok(raw) => match VaultId::parse(raw.trim()) {
            Ok(_) => VaultBinding::Bound,
            Err(e) => VaultBinding::Invalid(format!("invalid vault.id: {e}")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Missing vault.id with a binding sentinel means the identity
            // layer thinks this vault was bound — refuse to treat it as
            // a fresh / unbound directory.
            let cairn_dir = vault_root.join(".cairn");
            if cairn_dir.join("vault.binding").exists()
                || cairn_dir.join("vault.binding.pending").exists()
            {
                return VaultBinding::Invalid(format!(
                    "vault.id lost — binding sentinel exists at {} but \
                     .cairn/vault.id is missing; run `cairn identity \
                     vault-id-recover` to restore it",
                    cairn_dir.display()
                ));
            }
            VaultBinding::Unbound
        }
        Err(e) => VaultBinding::Invalid(format!("read {}: {e}", sentinel.display())),
    }
}

/// Run `cairn status`. Exits 0 on success.
#[must_use]
pub fn run(json: bool) -> ExitCode {
    run_with_context(json, None, None, false)
}

/// Run `cairn status` with optional vault root and config for capability probing.
///
/// When `vault_root` and `config` are supplied, the embedding-model presence
/// is stat-checked and `CapabilitySet::semantic_search` is wired accordingly.
/// Without them (e.g. the `bootstrap` / `vault` / `mcp` fast paths) the old
/// P0 empty list is returned.
///
/// `require_bound` — when `true`, the caller has already confirmed the
/// vault is bound (resolution source ≠ `CwdFallback`). Any non-`Bound`
/// sentinel observed here therefore indicates the binding was lost
/// between the caller's probe and this re-probe (file removed, mount
/// disappeared, race with `cairn forget --vault`); fail closed with
/// `EX_CONFIG` instead of falling through to the empty-capability path.
/// Without this flag a TOCTOU window would let a real-source vault
/// silently downgrade to `capabilities: []` + exit 0 (round-8 review #3).
#[must_use]
#[allow(clippy::too_many_lines)] // status response assembly keeps probe state and human/JSON parity in one place.
pub fn run_with_context(
    json: bool,
    vault_root: Option<&Path>,
    config: Option<&CairnConfig>,
    require_bound: bool,
) -> ExitCode {
    // Single binding probe per invocation — the result feeds both the
    // fail-closed gate below and `compute_capabilities` so the sentinel
    // is only stat'd once per `cairn status` call (was three times
    // previously).
    let binding = vault_root.map(probe_vault_binding);

    if let Some(b) = binding.as_ref() {
        match b {
            VaultBinding::Bound => {}
            VaultBinding::Invalid(reason) => {
                // A damaged sentinel is always fatal: treating it as
                // Unbound would silently advertise zero capabilities for
                // a vault the operator believes is bound (same class of
                // bug as the round-1 `unwrap_or_default` finding).
                eprintln!("cairn status: vault binding error — {reason}");
                return ExitCode::from(78); // EX_CONFIG
            }
            VaultBinding::Unbound => {
                if require_bound {
                    eprintln!(
                        "cairn status: vault at {} lost its binding between \
                         resolution and capability probe (.cairn/vault.id removed?) \
                         — refusing to advertise empty capabilities",
                        vault_root
                            .expect("invariant: binding probed only when vault_root is Some")
                            .display()
                    );
                    return ExitCode::from(78); // EX_CONFIG
                }
                // CwdFallback path: caller intentionally accepted an
                // unbound CWD; the empty capability list below is the
                // documented response.
            }
        }
    }

    let incarnation = new_operation_id();
    let started_at = chrono_like_now();
    let fallback_config = CairnConfig::default();
    let status_config = config.unwrap_or(&fallback_config);
    let status_vault_root = vault_root.unwrap_or_else(|| Path::new("."));

    let bound = matches!(binding, Some(VaultBinding::Bound));
    let caps = compute_capabilities(vault_root, config, bound);
    let health = StatusResponseHealth {
        authority_db: authority_db_health(status_vault_root),
        nexus_projection: nexus_projection_health(status_vault_root, status_config),
    };
    let authority_db_healthy = matches!(
        health.authority_db.state,
        StatusResponseHealthAuthorityDbState::Healthy
    );
    let local_sensor_status =
        match map_local_sensor_status(vault_root, status_config, bound, authority_db_healthy) {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("cairn status: sensor status error — {error:#}");
                return ExitCode::from(78); // EX_CONFIG
            }
        };
    let extensions = cairn_core::status::extension_namespaces(&caps);

    // ── MCP graph-tools availability (issue #190 Plan A) ─────────────
    // The probe only runs against a *bound* vault: an unbound CWD has
    // no `.cairn/vault.id` and no migrated database to peek, so
    // `try_peek_store_capabilities` would surface a generic
    // `store_open_error` even though there is nothing wrong with the
    // deployment. Gate the store-touching probe on `bound`; the
    // resolver/predicate side still runs whenever config is present.
    let probe_vault_root = if bound { vault_root } else { None };
    let (mcp_graph_avail, probe_basis_for_json) = config.map_or_else(
        || (None, ProbeBasis::ConfigOnly),
        |cfg| probe_mcp_graph_tools(cfg, probe_vault_root),
    );

    // `mcp_graph_tools` is optional in the IDL (additive change to
    // keep the `cairn.mcp.v1` wire contract backward-compatible),
    // but in practice both adapter surfaces always emit it: omitting
    // it on one side while the other emits a `NoVault` payload would
    // break cross-surface parity for clients that consume CLI and
    // SDK status interchangeably (round-10 review). When the CLI has
    // no config to drive the probe, synthesize the same `NoVault`
    // wire response the SDK emits — there is no MCP server to
    // negotiate against either way.
    let mcp_graph_tools_field: Option<StatusResponseMcpGraphTools> =
        Some(mcp_graph_avail.as_ref().map_or_else(
            || {
                McpGraphToolsStatus::from_resolved(
                    &ResolvedAvailability::NoVault,
                    ProbeBasis::ConfigOnly,
                )
                .to_wire()
            },
            |(_, wire)| wire.clone(),
        ));

    let resp = StatusResponse {
        contract: "cairn.mcp.v1".to_owned(),
        server_info: StatusResponseServerInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            build: build_profile(),
            started_at: started_at.clone(),
            incarnation: incarnation.clone(),
        },
        capabilities: caps,
        extensions,
        health,
        sensors: map_screen_probe(
            &screen::probe_config(&status_config.sensors.screen),
            Some(local_sensor_status),
        ),
        // Advertise the live routing policy (issue #217). The
        // `capture_trace` verb dispatches through the same
        // `DefaultRegistry` (see `crates/cairn-cli/src/verbs/capture_trace.rs`
        // — `dispatch(event, &DefaultRegistry)`), so this advertisement
        // is exact for the runtime, not a placeholder. When a
        // deployment-supplied `ToolSchemaLookup` lands, the call here
        // and the matching call in `capture_trace` move in lockstep to
        // the live registry; the wire schema's family-granular shape
        // makes the two sides un-divergeable.
        pipeline_dispatch: Some(pipeline_dispatch_advertisement(&DefaultRegistry)),
        mcp_graph_tools: mcp_graph_tools_field,
        workflows: None,
    };

    if json {
        emit_json(&resp);
    } else {
        println!("contract:    {}", resp.contract);
        println!("version:     {}", resp.server_info.version);
        println!("build:       {}", resp.server_info.build);
        println!("started_at:  {started_at}");
        println!("incarnation: {}", incarnation.0);
        if resp.capabilities.is_empty() {
            println!("capabilities: (none advertised — store not wired in this build)");
        } else {
            for cap in &resp.capabilities {
                println!(
                    "  capability: {}",
                    serde_json::to_string(cap).unwrap_or_default()
                );
            }
        }
        // Human output: one line for the mcp graph-tools state
        if let Some((avail, _)) = &mcp_graph_avail {
            println!("{}", render_mcp_graph_line(avail));
        } else {
            // No config — report unavailable with config-only probe
            println!("{}", render_mcp_graph_line(&ResolvedAvailability::NoVault));
        }
        println!(
            "screen:      {} {}",
            status_screen_backend_label(resp.sensors.screen.backend),
            status_screen_state_label(resp.sensors.screen.state)
        );
        println!(
            "authority_db: {}",
            render_authority_human(&resp.health.authority_db)
        );
        println!(
            "nexus_projection: {}",
            render_projection_human(&resp.health.nexus_projection)
        );
    }
    // suppress unused warning when config is None
    let _ = probe_basis_for_json;
    ExitCode::SUCCESS
}

/// Derive the `Capabilities` list from the active config and filesystem state.
///
/// `vault_root` is used only to stat-check the embedding-model directory;
/// no I/O is performed when it is `None`. `bound` is the
/// already-probed binding state from the caller — passing it in keeps
/// the binding sentinel from being re-stat'd here (and removes the
/// TOCTOU window the third probe used to open). When `config` is
/// `None` we fall back to the empty list — the IDL declares
/// `cairn.mcp.v1.policy_trace` (#95) and store-driven mode
/// capabilities, but they're advertised only once verb runtime can
/// honor them end-to-end (#9 / #61 / #62).
fn compute_capabilities(
    vault_root: Option<&Path>,
    config: Option<&CairnConfig>,
    bound: bool,
) -> Vec<Capabilities> {
    let mut caps = if let Some(config) = config {
        let model_present = vault_root.is_some_and(|root| {
            let models_root = root.join(".cairn").join("models");
            let cache = cairn_embeddings_local::ModelCache::new(&models_root);
            let kind: EmbeddingModelKind = config.search.embedding_model;
            cache.is_present(kind)
        });

        // For local providers: embedding_provider_ready == model_present.
        // For cloud providers (OpenAI): requires the `openai` Cargo feature AND
        // OPENAI_API_KEY to be set. A stale local model file on disk with a cloud
        // provider configured must NOT advertise semantic/hybrid (Finding 3, #53).
        let embedding_provider_ready =
            compute_embedding_provider_ready(config, model_present, vault_root);

        // Consolidation runtime readiness mirrors the gates the `cairn mcp`
        // boot path actually checks before constructing a Scheduler: the
        // config must enable consolidation AND the deployment must be
        // running in single_tenant mode with a bound principal (the only
        // arm that constructs the SqliteJobStore + handlers). Without all
        // three, status must not advertise the capability (round-8
        // adversarial review #2).
        let single_tenant_ready =
            config.mcp.stdio.single_tenant && config.mcp.stdio.principal.is_some();
        let consolidation_runtime_ready = config.consolidation.enabled && single_tenant_ready;
        // Issue #91: dream/expiration/evaluation runtime readiness mirrors the
        // boot-path gating used for consolidation (config opt-in + single-tenant
        // mcp serve + bound principal).
        let agent_configured = agent_runtime_configured(config);
        let dream_runtime_ready = dream_runtime_ready_for_config(config, single_tenant_ready);
        let expiration_runtime_ready = config.expiration.enabled && single_tenant_ready;
        let evaluation_runtime_ready = config.evaluation.enabled && single_tenant_ready;

        cairn_core::status::advertise(&cairn_core::status::CapabilityGates {
            config: config.capabilities(embedding_provider_ready),
            // CLI status path stays read-only and never opens the SQLite store.
            // The bound-vault structural backstop in advertise() drives the FTS gate.
            store: None,
            vault_bound: bound,
            model_present,
            embedding_provider_ready,
            llm_configured: config.llm.provider.is_some(),
            agent_configured,
            consolidation_runtime_ready,
            dream_runtime_ready,
            expiration_runtime_ready,
            evaluation_runtime_ready,
            contract_phase: CLI_CONTRACT_PHASE,
        })
    } else {
        // No config available — return only compiled local sensor capabilities.
        vec![]
    };

    for capability in screen::compiled_capabilities() {
        if !caps.contains(&capability) {
            caps.push(capability);
        }
    }
    caps
}

/// Probe MCP graph-tools availability from the given config and optional vault root.
///
/// Returns `(Some((avail, wire_type)), probe_basis)` when config is available,
/// or `(None, ConfigOnly)` when no config is present.
fn probe_mcp_graph_tools(
    cfg: &CairnConfig,
    vault_root: Option<&Path>,
) -> (
    Option<(ResolvedAvailability, StatusResponseMcpGraphTools)>,
    ProbeBasis,
) {
    // Validate `[mcp.*]` config first so a misconfigured deployment
    // surfaces a distinct `ConfigError` state in `status` rather than
    // collapsing into a generic `no scope resolver wired` line. `cairn
    // mcp` exits with EX_CONFIG on the same error; `cairn status`
    // mirrors that diagnosis without requiring the operator to start the
    // server to find out.
    if let Err(err) = cfg.validate_mcp() {
        let avail = ResolvedAvailability::ConfigError {
            error: err.to_string(),
        };
        let mgt = McpGraphToolsStatus::from_resolved(&avail, ProbeBasis::ConfigOnly);
        let wire = mgt.to_wire();
        return (Some((avail, wire)), ProbeBasis::ConfigOnly);
    }

    let scope_components: Option<crate::mcp::ResolvedMcpScope> =
        crate::mcp::resolve_scope_components(cfg);
    let scope_for_predicate: Option<&dyn cairn_core::mcp_auth::McpSessionScope> = scope_components
        .as_ref()
        .map(|r| std::sync::Arc::as_ref(&r.resolver) as &dyn cairn_core::mcp_auth::McpSessionScope);

    let (probe_outcome, probe_basis) = match vault_root {
        Some(root) => match try_peek_store_capabilities(root) {
            Ok(caps) => (ProbeOutcome::Capabilities(caps), ProbeBasis::FullProbe),
            Err(err) => {
                tracing::debug!(
                    ?err,
                    "status: store-cap probe failed; reporting ProbeFailed"
                );
                (
                    ProbeOutcome::Failed {
                        error: err.to_string(),
                    },
                    ProbeBasis::ConfigOnly,
                )
            }
        },
        None => (ProbeOutcome::NoVault, ProbeBasis::ConfigOnly),
    };

    let avail: ResolvedAvailability = match &probe_outcome {
        ProbeOutcome::Capabilities(store_caps) => {
            let predicate = cfg.mcp_graph_tools_available(
                scope_for_predicate,
                cairn_core::mcp_auth::McpTransport::Stdio,
                store_caps,
            );
            match (
                matches!(
                    predicate,
                    cairn_core::mcp_auth::McpGraphAvailability::Available { .. }
                ),
                scope_components.as_ref(),
            ) {
                (true, Some(rs)) => {
                    let ctx = cairn_core::mcp_auth::McpAuthContext::new(
                        &rs.principal,
                        "cairn-status-probe",
                    );
                    match rs.resolver.allowed_scopes(&ctx) {
                        Ok(v) if !v.is_empty() => ResolvedAvailability::Predicate(predicate),
                        Ok(_) => ResolvedAvailability::ResolverEmpty { error: None },
                        Err(e) => ResolvedAvailability::ResolverEmpty {
                            error: Some(e.to_string()),
                        },
                    }
                }
                _ => ResolvedAvailability::Predicate(predicate),
            }
        }
        ProbeOutcome::Failed { error } => ResolvedAvailability::ProbeFailed {
            error: error.clone(),
        },
        ProbeOutcome::NoVault => ResolvedAvailability::NoVault,
    };

    let mgt = McpGraphToolsStatus::from_resolved(&avail, probe_basis);
    let wire = mgt.to_wire();
    (Some((avail, wire)), probe_basis)
}

/// Project a [`CairnConfig`] + model-on-disk state into the wire-format
/// capability list, *without* the vault-presence gate. Used by the
/// `--explain` capability gate at parse time, before any vault is resolved.
/// Mirrors `cairn-sdk`'s `Sdk::advertised_capabilities` derivation by
/// passing through `cairn-core::status::advertise()`.
fn capabilities_for_config(config: &CairnConfig, model_present: bool) -> Vec<Capabilities> {
    let embedding_provider_ready = compute_embedding_provider_ready(config, model_present, None);
    let single_tenant_ready =
        config.mcp.stdio.single_tenant && config.mcp.stdio.principal.is_some();
    let consolidation_runtime_ready = config.consolidation.enabled && single_tenant_ready;
    let agent_configured = agent_runtime_configured(config);
    let dream_runtime_ready = dream_runtime_ready_for_config(config, single_tenant_ready);
    let expiration_runtime_ready = config.expiration.enabled && single_tenant_ready;
    let evaluation_runtime_ready = config.evaluation.enabled && single_tenant_ready;
    cairn_core::status::advertise(&cairn_core::status::CapabilityGates {
        config: config.capabilities(embedding_provider_ready),
        store: None,
        vault_bound: true, // capability surface — used by --explain gate;
        // the gate runs only when caller is in a vault.
        model_present,
        embedding_provider_ready,
        llm_configured: config.llm.provider.is_some(),
        agent_configured,
        consolidation_runtime_ready,
        dream_runtime_ready,
        expiration_runtime_ready,
        evaluation_runtime_ready,
        contract_phase: CLI_CONTRACT_PHASE,
    })
}

/// Determine whether the configured embedding *provider* is ready to produce
/// vectors end-to-end, for use in `CapabilityGates::embedding_provider_ready`.
///
/// Delegates to [`super::embedding_provider_ready`], which is the shared
/// implementation used by both this module and `search.rs`.
fn compute_embedding_provider_ready(
    config: &CairnConfig,
    model_present: bool,
    vault_root: Option<&Path>,
) -> bool {
    super::embedding_provider_ready(config, model_present, vault_root)
}

fn agent_runtime_configured(config: &CairnConfig) -> bool {
    matches!(
        config.agent_provider.kind,
        Some(AgentProviderKind::CairnCore)
    ) && config.llm.provider.is_some()
}

fn dream_runtime_ready_for_config(config: &CairnConfig, single_tenant_ready: bool) -> bool {
    config.dream.enabled
        && single_tenant_ready
        && if config.dream.requires_agent_provider() {
            false
        } else {
            config.llm.provider.is_some()
        }
}

fn map_local_sensor_status(
    vault_root: Option<&Path>,
    config: &CairnConfig,
    bound: bool,
    authority_db_healthy: bool,
) -> anyhow::Result<Vec<StatusResponseSensorsLocal>> {
    let consent_states = if let Some(root) = vault_root.filter(|_| bound && authority_db_healthy) {
        block_on(read_local_sensor_consent_states(root))?
    } else {
        vec![crate::sensor_gate::SensorConsentState::Missing; LocalSensorName::ALL.len()]
    };
    let last_drops = vault_root
        .map(crate::sensor_gate::read_sensor_drop_metrics)
        .transpose()?
        .unwrap_or_default();

    let rows = LocalSensorName::ALL
        .into_iter()
        .zip(consent_states)
        .map(|(sensor, consent)| {
            let enabled = crate::sensor_gate::sensor_enabled(config, sensor);
            let gate = status_gate(config, consent, sensor);
            StatusResponseSensorsLocal {
                budget: status_budget(config, sensor),
                consent: status_consent(consent),
                enabled,
                gate,
                last_drop_reason: last_drops
                    .iter()
                    .rev()
                    .find(|drop| drop.sensor == sensor)
                    .map(|drop| status_last_drop_reason(drop.reason)),
                retention: status_retention(config, sensor),
                sensor: status_sensor(sensor),
            }
        })
        .collect();
    Ok(rows)
}

async fn read_local_sensor_consent_states(
    vault_root: &Path,
) -> anyhow::Result<Vec<crate::sensor_gate::SensorConsentState>> {
    let db_path = vault_root.join(".cairn").join("cairn.db");
    let store = cairn_store_sqlite::open(&db_path)
        .await
        .map_err(anyhow::Error::from)
        .with_context(|| format!("open {}", db_path.display()))?;
    let mut states = Vec::with_capacity(LocalSensorName::ALL.len());
    for sensor in LocalSensorName::ALL {
        states.push(crate::sensor_gate::latest_sensor_consent(&store, sensor).await?);
    }
    Ok(states)
}

fn block_on<T>(future: impl std::future::Future<Output = anyhow::Result<T>>) -> anyhow::Result<T> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build async runtime")?
        .block_on(future)
}

fn status_gate(
    config: &CairnConfig,
    consent: crate::sensor_gate::SensorConsentState,
    sensor: LocalSensorName,
) -> StatusResponseSensorsLocalGate {
    match crate::sensor_gate::evaluate_sensor_gate(
        config,
        consent,
        sensor,
        BudgetObservation { items: 0, bytes: 0 },
    ) {
        Ok(()) => StatusResponseSensorsLocalGate::Allowed,
        Err(SensorGateReason::Disabled) => StatusResponseSensorsLocalGate::Disabled,
        Err(SensorGateReason::PrivacyDenied) => StatusResponseSensorsLocalGate::PrivacyDenied,
        Err(SensorGateReason::BudgetExceeded) => StatusResponseSensorsLocalGate::BudgetExceeded,
    }
}

fn status_budget(
    config: &CairnConfig,
    sensor: LocalSensorName,
) -> StatusResponseSensorsLocalBudget {
    match sensor {
        LocalSensorName::Screen => StatusResponseSensorsLocalBudget {
            max_bytes: Some(i64::from(
                config.sensors.screen.budget.max_text_bytes_per_event,
            )),
            max_items: Some(i64::from(
                config.sensors.screen.budget.max_frames_per_minute,
            )),
        },
        LocalSensorName::Hook => shared_budget(&config.sensors.hooks.budget),
        LocalSensorName::Ide => shared_budget(&config.sensors.ide.budget),
        LocalSensorName::Terminal => shared_budget(&config.sensors.terminal.budget),
        LocalSensorName::Clipboard => shared_budget(&config.sensors.clipboard.budget),
        LocalSensorName::Voice => shared_budget(&config.sensors.voice.budget),
        LocalSensorName::Recording => shared_budget(&config.sensors.recording.budget),
    }
}

fn shared_budget(
    budget: &cairn_core::config::SensorCaptureBudget,
) -> StatusResponseSensorsLocalBudget {
    StatusResponseSensorsLocalBudget {
        max_bytes: budget.max_bytes.and_then(|value| i64::try_from(value).ok()),
        max_items: budget.max_items.and_then(|value| i64::try_from(value).ok()),
    }
}

fn status_retention(
    config: &CairnConfig,
    sensor: LocalSensorName,
) -> StatusResponseSensorsLocalRetention {
    let max_days = match sensor {
        LocalSensorName::Hook => config.sensors.hooks.retention.max_days,
        LocalSensorName::Ide => config.sensors.ide.retention.max_days,
        LocalSensorName::Terminal => config.sensors.terminal.retention.max_days,
        LocalSensorName::Clipboard => config.sensors.clipboard.retention.max_days,
        LocalSensorName::Voice => config.sensors.voice.retention.max_days,
        LocalSensorName::Screen => config.sensors.screen.retention.max_days,
        LocalSensorName::Recording => config.sensors.recording.retention.max_days,
    };
    StatusResponseSensorsLocalRetention {
        max_days: max_days.map(i64::from),
    }
}

fn status_consent(
    consent: crate::sensor_gate::SensorConsentState,
) -> StatusResponseSensorsLocalConsent {
    match consent {
        crate::sensor_gate::SensorConsentState::Enabled => {
            StatusResponseSensorsLocalConsent::Enabled
        }
        crate::sensor_gate::SensorConsentState::Disabled => {
            StatusResponseSensorsLocalConsent::Disabled
        }
        crate::sensor_gate::SensorConsentState::Missing => {
            StatusResponseSensorsLocalConsent::Missing
        }
    }
}

fn status_sensor(sensor: LocalSensorName) -> StatusResponseSensorsLocalSensor {
    match sensor {
        LocalSensorName::Hook => StatusResponseSensorsLocalSensor::Hook,
        LocalSensorName::Ide => StatusResponseSensorsLocalSensor::Ide,
        LocalSensorName::Terminal => StatusResponseSensorsLocalSensor::Terminal,
        LocalSensorName::Clipboard => StatusResponseSensorsLocalSensor::Clipboard,
        LocalSensorName::Voice => StatusResponseSensorsLocalSensor::Voice,
        LocalSensorName::Screen => StatusResponseSensorsLocalSensor::Screen,
        LocalSensorName::Recording => StatusResponseSensorsLocalSensor::Recording,
    }
}

fn status_last_drop_reason(reason: SensorGateReason) -> StatusResponseSensorsLocalLastDropReason {
    match reason {
        SensorGateReason::Disabled => StatusResponseSensorsLocalLastDropReason::Disabled,
        SensorGateReason::PrivacyDenied => StatusResponseSensorsLocalLastDropReason::PrivacyDenied,
        SensorGateReason::BudgetExceeded => {
            StatusResponseSensorsLocalLastDropReason::BudgetExceeded
        }
    }
}

fn map_screen_probe(
    probe: &ScreenProbe,
    local: Option<Vec<StatusResponseSensorsLocal>>,
) -> StatusResponseSensors {
    StatusResponseSensors {
        local,
        screen: StatusResponseSensorsScreen {
            backend: map_screen_backend(probe.backend),
            degradation: probe.degradation.as_ref().map(|degradation| {
                StatusResponseSensorsScreenDegradation {
                    code: map_screen_degradation_code(degradation.code),
                    message: degradation.message.clone(),
                }
            }),
            mode: map_screen_mode(probe.mode),
            ocr_engine: map_screen_ocr_engine(probe.ocr_engine),
            permission: map_screen_permission(probe.permission),
            state: map_screen_state(probe.state),
        },
    }
}

fn map_screen_backend(backend: ScreenBackend) -> StatusResponseSensorsScreenBackend {
    if matches!(backend, ScreenBackend::Screenpipe) {
        StatusResponseSensorsScreenBackend::Screenpipe
    } else {
        // The current status schema is closed; unknown config backends are
        // reported as xcap while `probe_config` separately degrades them.
        StatusResponseSensorsScreenBackend::Xcap
    }
}

fn status_screen_backend_label(backend: StatusResponseSensorsScreenBackend) -> &'static str {
    if matches!(backend, StatusResponseSensorsScreenBackend::Screenpipe) {
        "screenpipe"
    } else {
        "xcap"
    }
}

fn status_screen_state_label(state: StatusResponseSensorsScreenState) -> &'static str {
    match state {
        StatusResponseSensorsScreenState::Disabled => "disabled",
        StatusResponseSensorsScreenState::Enabled => "enabled",
        StatusResponseSensorsScreenState::PermissionMissing => "permission_missing",
        _ => "degraded",
    }
}

fn map_screen_state(state: ScreenState) -> StatusResponseSensorsScreenState {
    match state {
        ScreenState::Disabled => StatusResponseSensorsScreenState::Disabled,
        ScreenState::Enabled => StatusResponseSensorsScreenState::Enabled,
        ScreenState::PermissionMissing => StatusResponseSensorsScreenState::PermissionMissing,
        ScreenState::Degraded => StatusResponseSensorsScreenState::Degraded,
    }
}

fn map_screen_mode(mode: ScreenMode) -> StatusResponseSensorsScreenMode {
    match mode {
        ScreenMode::Off => StatusResponseSensorsScreenMode::Off,
        ScreenMode::Snapshot => StatusResponseSensorsScreenMode::Snapshot,
        ScreenMode::Continuous => StatusResponseSensorsScreenMode::Continuous,
    }
}

fn map_screen_ocr_engine(
    ocr_engine: ResolvedScreenOcrEngine,
) -> StatusResponseSensorsScreenOcrEngine {
    match ocr_engine {
        ResolvedScreenOcrEngine::Vision => StatusResponseSensorsScreenOcrEngine::Vision,
        ResolvedScreenOcrEngine::Winrt => StatusResponseSensorsScreenOcrEngine::Winrt,
        ResolvedScreenOcrEngine::Tesseract => StatusResponseSensorsScreenOcrEngine::Tesseract,
        ResolvedScreenOcrEngine::Off => StatusResponseSensorsScreenOcrEngine::Off,
    }
}

fn map_screen_permission(permission: ScreenPermission) -> StatusResponseSensorsScreenPermission {
    match permission {
        ScreenPermission::NotRequested => StatusResponseSensorsScreenPermission::NotRequested,
        ScreenPermission::Granted => StatusResponseSensorsScreenPermission::Granted,
        ScreenPermission::Denied => StatusResponseSensorsScreenPermission::Denied,
        ScreenPermission::Revoked => StatusResponseSensorsScreenPermission::Revoked,
    }
}

fn map_screen_degradation_code(
    code: ScreenDegradationCode,
) -> StatusResponseSensorsScreenDegradationCode {
    match code {
        ScreenDegradationCode::Disabled => {
            StatusResponseSensorsScreenDegradationCode::ScreenDisabled
        }
        ScreenDegradationCode::PermissionMissing => {
            StatusResponseSensorsScreenDegradationCode::ScreenPermissionMissing
        }
        ScreenDegradationCode::BackendUnavailable => {
            StatusResponseSensorsScreenDegradationCode::ScreenBackendUnavailable
        }
        ScreenDegradationCode::Degraded => {
            StatusResponseSensorsScreenDegradationCode::ScreenDegraded
        }
    }
}

/// True if `capability` is in the current `status.capabilities` list.
/// Used by capability-gated args (e.g. `search --explain`) to fail closed
/// before verb dispatch when the required capability is not advertised
/// (CLAUDE.md §4.6).
///
/// Uses the P0 default config for the capability check. This includes
/// `cairn.mcp.v1.policy_trace`, which is always `true` at P0 (the config
/// unconditionally sets `policy_trace: true` — see `CairnConfig::capabilities`
/// and its tests). Semantic/hybrid search require a model on disk and are
/// therefore absent when probed without a vault root.
///
/// Bypasses the vault-presence gate because this is a per-build
/// capability probe (called at arg-parse time, before any vault is
/// resolved). Without that bypass, `--explain` would be rejected even
/// for users running inside a real bound vault.
#[must_use]
pub fn p0_capabilities_advertises(capability: &str) -> bool {
    let default_config = CairnConfig::default();
    capabilities_for_config(&default_config, false)
        .iter()
        .any(|c| {
            serde_json::to_value(c)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .as_deref()
                == Some(capability)
        })
}

/// Return the current UTC time as an RFC-3339 string without sub-second precision.
fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("invariant: system clock is after Unix epoch")
        .as_secs();
    let (y, mo, d, h, mi, s) = secs_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn secs_to_ymdhms(mut s: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = s % 60;
    s /= 60;
    let min = s % 60;
    s /= 60;
    let hour = s % 24;
    s /= 24;
    let mut days = s;
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = is_leap(year);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &m in &months {
        if days < m {
            break;
        }
        days -= m;
        month += 1;
    }
    (year, month, days + 1, hour, min, sec)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

fn build_profile() -> String {
    if cfg!(debug_assertions) {
        "debug".to_owned()
    } else {
        "release".to_owned()
    }
}

/// Result of the store-capability probe — full open succeeded vs.
/// fell back to config-only. Surfaced in `status` output so an
/// "unavailable" verdict is never mistaken for an authoritative
/// negative when the store could not actually be inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeBasis {
    /// The on-disk store was successfully opened and capabilities read.
    FullProbe,
    /// The store could not be opened; capabilities derived from config only.
    ConfigOnly,
}

/// Outcome of attempting to inspect the on-disk store. Distinct
/// from `MemoryStoreCapabilities::default()` so probe failures
/// cannot collapse into a synthetic `UnavailableNoStoreCapability`
/// verdict. `NoVault` and `Failed` short-circuit the predicate
/// entirely.
enum ProbeOutcome {
    Capabilities(cairn_core::contract::memory_store::MemoryStoreCapabilities),
    Failed { error: String },
    NoVault,
}

/// What `cairn status` actually has to render. The four
/// `McpGraphAvailability` cells come from the predicate when the
/// probe succeeded; `ProbeFailed` and `NoVault` are status-level
/// outcomes that do not exist in the predicate's enum because
/// `cairn mcp` always has a concrete answer (it opened the
/// store) and never needs them.
pub(crate) enum ResolvedAvailability {
    Predicate(cairn_core::mcp_auth::McpGraphAvailability),
    ProbeFailed {
        error: String,
    },
    NoVault,
    /// The static predicate said Available, but the wired resolver
    /// returned `Ok(empty)` or `Err(_)` for the synthetic context
    /// the status probe constructed.
    ResolverEmpty {
        error: Option<String>,
    },
    /// `[mcp.*]` config did not pass `validate_mcp` — surfaced as a
    /// distinct state so misconfiguration is not silently masked as
    /// "no resolver wired" (`cairn mcp` exits with `EX_CONFIG` on the
    /// same error).
    ConfigError {
        error: String,
    },
}

/// Render a single human-readable status line for the `mcp.graph_tools` state.
#[must_use]
pub(crate) fn render_mcp_graph_line(avail: &ResolvedAvailability) -> String {
    use cairn_core::mcp_auth::McpGraphAvailability;
    match avail {
        ResolvedAvailability::Predicate(p) => match p {
            McpGraphAvailability::Available { tool_count } => {
                format!("mcp.graph_tools: available ({tool_count} tools)")
            }
            McpGraphAvailability::UnavailableSingleTenantOff => {
                "mcp.graph_tools: unavailable (single-tenant mode off)".to_owned()
            }
            McpGraphAvailability::UnavailableNoStoreCapability => {
                "mcp.graph_tools: unavailable (store does not advertise graph_edges)".to_owned()
            }
            McpGraphAvailability::UnavailableNoScopeResolver => {
                "mcp.graph_tools: unavailable (no scope resolver wired)".to_owned()
            }
            _ => "mcp.graph_tools: unavailable (unknown predicate state)".to_owned(),
        },
        ResolvedAvailability::ProbeFailed { error } => {
            format!("mcp.graph_tools: probe-failed ({error})")
        }
        ResolvedAvailability::NoVault => {
            "mcp.graph_tools: probe-skipped (no vault bound)".to_owned()
        }
        ResolvedAvailability::ResolverEmpty { error: Some(e) } => {
            format!("mcp.graph_tools: unavailable (resolver error: {e})")
        }
        ResolvedAvailability::ResolverEmpty { error: None } => {
            "mcp.graph_tools: unavailable (resolver returned no allowed scopes)".to_owned()
        }
        ResolvedAvailability::ConfigError { error } => {
            format!("mcp.graph_tools: config-error ({error})")
        }
    }
}

/// Domain type used by `render_mcp_graph_line` and unit tests.
/// Distinct from the IDL-generated `StatusResponseMcpGraphTools`
/// which is the serialised wire type.
#[derive(serde::Serialize)]
pub(crate) struct McpGraphToolsStatus {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_count: Option<u32>,
    pub probe_basis: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
                tool_count: Some(u32::from(*tool_count)),
                probe_basis: basis,
                error: None,
            },
            McpGraphAvailability::UnavailableSingleTenantOff => Self {
                state: "unavailable",
                reason: Some("single_tenant_off"),
                tool_count: None,
                probe_basis: basis,
                error: None,
            },
            McpGraphAvailability::UnavailableNoStoreCapability => Self {
                state: "unavailable",
                reason: Some("no_store_capability"),
                tool_count: None,
                probe_basis: basis,
                error: None,
            },
            McpGraphAvailability::UnavailableNoScopeResolver => Self {
                state: "unavailable",
                reason: Some("no_scope_resolver"),
                tool_count: None,
                probe_basis: basis,
                error: None,
            },
            _ => Self {
                state: "unavailable",
                reason: None,
                tool_count: None,
                probe_basis: basis,
                error: None,
            },
        }
    }

    pub fn from_resolved(avail: &ResolvedAvailability, probe_basis: ProbeBasis) -> Self {
        let basis = match probe_basis {
            ProbeBasis::FullProbe => "full",
            ProbeBasis::ConfigOnly => "config_only",
        };
        match avail {
            ResolvedAvailability::Predicate(p) => Self::from_predicate(p, probe_basis),
            ResolvedAvailability::ProbeFailed { error } => Self {
                state: "probe_failed",
                reason: Some("store_open_error"),
                tool_count: None,
                probe_basis: basis,
                error: Some(error.clone()),
            },
            ResolvedAvailability::NoVault => Self {
                state: "no_vault",
                reason: Some("vault_not_bound"),
                tool_count: None,
                probe_basis: basis,
                error: None,
            },
            ResolvedAvailability::ResolverEmpty { error } => Self {
                state: "unavailable",
                reason: Some("resolver_empty"),
                tool_count: None,
                probe_basis: basis,
                error: error.clone(),
            },
            ResolvedAvailability::ConfigError { error } => Self {
                // The IDL state enum does not have a dedicated config-error
                // discriminant; surface as `unavailable` with the underlying
                // ConfigError text in the optional `error` field. The human
                // renderer prints a distinct `config-error (...)` line.
                state: "unavailable",
                reason: None,
                tool_count: None,
                probe_basis: basis,
                error: Some(error.clone()),
            },
        }
    }

    /// Convert to the IDL-generated wire type.
    pub fn to_wire(&self) -> StatusResponseMcpGraphTools {
        use StatusResponseMcpGraphToolsProbeBasis as PB;
        use StatusResponseMcpGraphToolsReason as R;
        use StatusResponseMcpGraphToolsState as S;

        let state = match self.state {
            "available" => S::Available,
            "probe_failed" => S::ProbeFailed,
            "no_vault" => S::NoVault,
            _ => S::Unavailable,
        };
        let reason = self.reason.map(|r| match r {
            "no_store_capability" => R::NoStoreCapability,
            "no_scope_resolver" => R::NoScopeResolver,
            "store_open_error" => R::StoreOpenError,
            "vault_not_bound" => R::VaultNotBound,
            "resolver_empty" => R::ResolverEmpty,
            _ => R::SingleTenantOff,
        });
        let probe_basis = match self.probe_basis {
            "full" => PB::Full,
            _ => PB::ConfigOnly,
        };
        StatusResponseMcpGraphTools {
            state,
            reason,
            tool_count: self.tool_count.map(u64::from),
            probe_basis,
            error: self.error.clone(),
        }
    }
}

/// Open the `SQLite` store at `vault_root` read-only and read its
/// `MemoryStoreCapabilities`. Sync wrapper — `status` stays sync; no
/// tokio runtime is built here.
///
/// A freshly bootstrapped vault has no `cairn.db` yet (the file is
/// created on the first store-opening verb run). `peek_capabilities`
/// reports that as [`cairn_store_sqlite::StoreError::SchemaNotInitialized`];
/// we translate it into an empty
/// [`cairn_core::contract::memory_store::MemoryStoreCapabilities`] so
/// the predicate surfaces the post-bootstrap state as `unavailable /
/// no_store_capability` rather than the alarming `probe_failed /
/// store_open_error / sqlite error` an operator otherwise sees on a
/// perfectly healthy vault (e2e finding).
fn try_peek_store_capabilities(
    vault_root: &std::path::Path,
) -> Result<cairn_core::contract::memory_store::MemoryStoreCapabilities, Box<dyn std::error::Error>>
{
    let db_path = crate::mcp::store_db_path(vault_root);
    match cairn_store_sqlite::peek_capabilities(&db_path) {
        Ok(caps) => Ok(caps),
        Err(cairn_store_sqlite::StoreError::SchemaNotInitialized) => {
            Ok(cairn_core::contract::memory_store::MemoryStoreCapabilities::default())
        }
        Err(other) => Err(other.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrono_like_now_is_valid_rfc3339() {
        let now = chrono_like_now();
        // Format: YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(now.len(), 20, "RFC-3339 must be 20 chars: {now}");
        assert!(now.ends_with('Z'), "RFC-3339 must end with Z: {now}");
        assert!(
            now.contains('T'),
            "RFC-3339 must contain T separator: {now}"
        );
        // Simple validation of structure
        let parts: Vec<&str> = now.split('T').collect();
        assert_eq!(parts.len(), 2, "RFC-3339 must have exactly one T: {now}");
        let date_part = parts[0];
        assert!(date_part.contains('-'), "date must have dashes: {now}");
    }

    #[test]
    fn secs_to_ymdhms_epoch() {
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(0);
        assert_eq!(y, 1970);
        assert_eq!(mo, 1);
        assert_eq!(d, 1);
        assert_eq!(h, 0);
        assert_eq!(mi, 0);
        assert_eq!(s, 0);
    }

    #[test]
    fn secs_to_ymdhms_day_boundary() {
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(86400); // One day after epoch
        assert_eq!(y, 1970);
        assert_eq!(mo, 1);
        assert_eq!(d, 2);
        assert_eq!(h, 0);
        assert_eq!(mi, 0);
        assert_eq!(s, 0);
    }

    #[test]
    fn secs_to_ymdhms_dec31_non_leap() {
        // 1999-12-31 23:59:59 UTC — day before Y2K
        // secs from epoch: 946684799
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(946_684_799);
        assert_eq!((y, mo, d, h, mi, s), (1999, 12, 31, 23, 59, 59));
    }

    #[test]
    fn secs_to_ymdhms_leap_day_2000() {
        // 2000-02-29 00:00:00 UTC — century leap year
        // secs from epoch: 951782400
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(951_782_400);
        assert_eq!((y, mo, d), (2000, 2, 29));
        assert_eq!((h, mi, s), (0, 0, 0));
    }

    #[test]
    fn secs_to_ymdhms_year_boundary_y2k() {
        // 2000-01-01 00:00:00 UTC
        // secs from epoch: 946684800
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(946_684_800);
        assert_eq!((y, mo, d, h, mi, s), (2000, 1, 1, 0, 0, 0));
    }

    #[test]
    fn is_leap_known_values() {
        assert!(is_leap(2000), "2000 is leap");
        assert!(!is_leap(1900), "1900 is not leap");
        assert!(is_leap(2004), "2004 is leap");
        assert!(!is_leap(2001), "2001 is not leap");
    }

    #[test]
    fn build_profile_returns_string() {
        let profile = build_profile();
        assert!(!profile.is_empty());
        assert!(profile == "debug" || profile == "release");
    }

    #[test]
    fn compute_capabilities_no_config_returns_compiled_screen_caps() {
        let caps = compute_capabilities(None, None, false);
        assert!(
            caps.contains(&Capabilities::CairnSensorV1ScreenXcap),
            "no config still advertises compiled screen sensor capabilities"
        );
        assert!(
            caps.iter()
                .all(|cap| !matches!(cap, Capabilities::CairnMcpV1SearchKeyword)),
            "no config must not advertise store-backed capabilities; got {caps:?}"
        );
    }

    #[test]
    fn compute_capabilities_default_config_no_vault_returns_compiled_screen_caps() {
        let config = CairnConfig::default();
        // No vault_root + bound=false → vault-presence gate fails
        // closed for store-backed capabilities, but compiled local
        // screen sensor capabilities are independent of vault binding.
        let caps = compute_capabilities(None, Some(&config), false);
        assert!(
            caps.contains(&Capabilities::CairnSensorV1ScreenXcap),
            "no vault root → compiled screen caps only; got {caps:?}"
        );
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SearchKeyword),
            "no vault root must not advertise search; got {caps:?}"
        );
    }

    #[test]
    fn compute_capabilities_unbound_vault_dir_returns_compiled_screen_caps() {
        // A tempdir without `.cairn/vault.id` is not a Cairn vault.
        // Caller passes `bound=false`; the CLI's status surface must
        // return only compiled local sensor capabilities so clients do
        // not negotiate against a non-existent store backend.
        let config = CairnConfig::default();
        let tmp = tempfile::tempdir().unwrap();
        let caps = compute_capabilities(Some(tmp.path()), Some(&config), false);
        assert!(
            caps.contains(&Capabilities::CairnSensorV1ScreenXcap),
            "tempdir without vault.id → compiled screen caps only; got {caps:?}"
        );
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SearchKeyword),
            "tempdir without vault.id must not advertise search; got {caps:?}"
        );
    }

    #[test]
    fn compute_capabilities_bound_vault_includes_keyword() {
        let config = CairnConfig::default();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".cairn")).unwrap();
        std::fs::write(
            tmp.path().join(".cairn").join("vault.id"),
            b"01HZZ0000000000000000000AB\n",
        )
        .unwrap();
        let caps = compute_capabilities(Some(tmp.path()), Some(&config), true);
        assert!(
            caps.contains(&Capabilities::CairnMcpV1SearchKeyword),
            "keyword present once vault is bound; got {caps:?}"
        );
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SearchSemantic),
            "semantic absent when model not on disk; got {caps:?}"
        );
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SearchHybrid),
            "hybrid gates on model presence too (round-2 fix); got {caps:?}"
        );
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SensorsPreCompact),
            "pre-compact must stay hidden until a runtime caller dispatches the hook; got {caps:?}"
        );
    }

    #[test]
    fn compute_capabilities_bound_vault_with_llm_includes_summarize_narrative() {
        let mut config = CairnConfig::default();
        config.llm.provider = Some(cairn_core::config::LlmProvider::OpenaiCompatible);
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".cairn")).unwrap();
        std::fs::write(
            tmp.path().join(".cairn").join("vault.id"),
            b"01HZZ0000000000000000000AB\n",
        )
        .unwrap();

        let caps = compute_capabilities(Some(tmp.path()), Some(&config), true);

        assert!(
            caps.contains(&Capabilities::CairnMcpV1SummarizeNarrative),
            "summarize.narrative present once v0.2 CLI has an LLM provider configured; got {caps:?}"
        );
    }

    #[test]
    fn compute_capabilities_agent_dream_is_withheld_until_agent_dispatch_exists() {
        let mut config = CairnConfig::default();
        config.llm.provider = Some(cairn_core::config::LlmProvider::OpenaiCompatible);
        config.agent_provider.kind = Some(cairn_core::config::AgentProviderKind::CairnCore);
        config.mcp.stdio.single_tenant = true;
        config.mcp.stdio.principal = Some(cairn_core::domain::ScopeTuple {
            tenant: Some("acme".into()),
            ..cairn_core::domain::ScopeTuple::default()
        });
        config.dream.enabled = true;
        config.dream.deep_dreaming.worker = cairn_core::config::DreamWorkerMode::Agent;
        config.dream.deep_dreaming.max_tool_calls = 1;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".cairn")).unwrap();
        std::fs::write(
            tmp.path().join(".cairn").join("vault.id"),
            b"01HZZ0000000000000000000AB\n",
        )
        .unwrap();

        let caps = compute_capabilities(Some(tmp.path()), Some(&config), true);
        assert!(
            !caps.contains(&Capabilities::CairnWorkflowsV1Dream),
            "agent dream must stay withheld until Task 6 routes DreamWorkerMode::Agent through AgentProvider::spawn; got {caps:?}"
        );

        config.llm.provider = None;
        let caps = compute_capabilities(Some(tmp.path()), Some(&config), true);
        assert!(
            !caps.contains(&Capabilities::CairnWorkflowsV1Dream),
            "agent dream must fail closed without the LLM backing the bundled runtime; got {caps:?}"
        );

        config.llm.provider = Some(cairn_core::config::LlmProvider::OpenaiCompatible);
        config.agent_provider.kind = Some(cairn_core::config::AgentProviderKind::Custom(
            "external-agent".to_string(),
        ));
        let caps = compute_capabilities(Some(tmp.path()), Some(&config), true);
        assert!(
            !caps.contains(&Capabilities::CairnWorkflowsV1Dream),
            "custom agent providers must fail closed until actual provider resolution exists; got {caps:?}"
        );
    }

    #[test]
    fn compute_capabilities_local_embeddings_off_no_semantic() {
        let mut config = CairnConfig::default();
        config.search.local_embeddings = false;
        // Bind the vault so the presence gate doesn't short-circuit.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".cairn")).unwrap();
        std::fs::write(
            tmp.path().join(".cairn").join("vault.id"),
            b"01HZZ0000000000000000000AB\n",
        )
        .unwrap();
        let caps = compute_capabilities(Some(tmp.path()), Some(&config), true);
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SearchSemantic),
            "semantic absent when local_embeddings: false"
        );
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SearchHybrid),
            "hybrid absent when local_embeddings: false"
        );
    }
}

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
            graph_search: g,
        }
    }

    #[test]
    fn render_label_single_tenant_off() {
        let cfg = CairnConfig::default();
        let caps = caps_with_graph(true);
        let s = ConfigBackedScope::new(ScopeTuple::default());
        let dyn_s: &dyn McpSessionScope = &s;
        let avail = cfg.mcp_graph_tools_available(Some(dyn_s), McpTransport::Stdio, &caps);
        assert_eq!(
            render_mcp_graph_line(&ResolvedAvailability::Predicate(avail)),
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
        let avail = cfg.mcp_graph_tools_available(None, McpTransport::Stdio, &caps);
        assert_eq!(
            render_mcp_graph_line(&ResolvedAvailability::Predicate(avail)),
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
        let avail = cfg.mcp_graph_tools_available(Some(dyn_s), McpTransport::Stdio, &caps);
        assert_eq!(
            render_mcp_graph_line(&ResolvedAvailability::Predicate(avail)),
            "mcp.graph_tools: unavailable (store does not advertise graph_edges)",
        );
    }

    #[test]
    fn render_label_available() {
        // Plan A never produces this state from the predicate, but the
        // formatter must still handle it for Plan C forward-compat.
        let avail = McpGraphAvailability::Available { tool_count: 5 };
        assert_eq!(
            render_mcp_graph_line(&ResolvedAvailability::Predicate(avail)),
            "mcp.graph_tools: available (5 tools)",
        );
    }
}
