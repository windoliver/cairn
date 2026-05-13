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
use cairn_workflows::scheduler::HandlerRegistryBuilder;
use cairn_workflows::{
    ConsolidationForgetCleanupHandler, ConsolidationHandler, Scheduler, SchedulerConfig,
    SqliteJobStore, SystemClock,
};

/// Outcome of resolving the `[mcp.stdio]` block into runtime components.
pub struct ResolvedMcpScope {
    /// The scope resolver derived from the configured principal.
    pub resolver: Arc<dyn McpSessionScope>,
    /// The principal `ScopeTuple` extracted from config.
    pub principal: ScopeTuple,
}

/// Resolve the `[mcp.stdio]` config block into runtime scope components.
///
/// Returns `None` when `single_tenant` is off or no principal is configured.
#[must_use]
pub fn resolve_scope_components(config: &CairnConfig) -> Option<ResolvedMcpScope> {
    if !config.mcp.stdio.single_tenant {
        return None;
    }
    let principal = config.mcp.stdio.principal.clone()?;
    let resolver: Arc<dyn McpSessionScope> = Arc::new(ConfigBackedScope::new(principal.clone()));
    Some(ResolvedMcpScope {
        resolver,
        principal,
    })
}

/// Path to the on-disk `SQLite` store, derived from the vault root.
/// Single source of truth — Task 6 (status probe) reuses this to avoid
/// status/`cairn mcp` split-brain.
#[must_use]
pub(crate) fn store_db_path(vault_root: &std::path::Path) -> std::path::PathBuf {
    vault_root.join(".cairn/cairn.db")
}

/// Run the MCP stdio server.
///
/// `probe_binding` and `binding_origin` come from the CLI's vault
/// resolver: when `probe_binding` is `true` and the opt-in
/// `single_tenant=true` path is taken, the vault must already be
/// bound (`.cairn/vault.id` present) before `SQLite` is opened —
/// otherwise the open would silently materialize a fresh database
/// in an unbound directory. The legacy `single_tenant=false`
/// manifest path skips the gate to keep pre-Plan-A deployments
/// byte-identical.
///
/// Exit codes: 0 success, 69 `EX_UNAVAILABLE` (transport/IO/store), 78 `EX_CONFIG`.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "opt-in path: vault-binding gate + store open + scheduler boot + MCP serve is \
              intentionally linear so the startup sequence is visible in one place"
)]
pub fn run(
    vault_root: &Path,
    config: &CairnConfig,
    probe_binding: bool,
    binding_origin: &str,
) -> ExitCode {
    if let Err(e) = config.validate_mcp() {
        eprintln!("cairn mcp: config error — {e}");
        return ExitCode::from(78);
    }

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cairn mcp: failed to build tokio runtime: {e}");
            return ExitCode::from(69);
        }
    };

    // Compatibility invariant: opt-in branch resolves FIRST. The
    // single_tenant=false fallback must NOT open SQLite or scan the
    // vault — pre-Plan-A `cairn mcp` deployments stay byte-identical.
    let result = match resolve_scope_components(config) {
        Some(ResolvedMcpScope {
            resolver,
            principal,
        }) => {
            // Vault-binding gate: the graph-tools path is about to
            // open and migrate <vault>/.cairn/cairn.db, which must
            // not run against a directory that is not actually a
            // bound Cairn vault. Mirrors the gate `cairn status`
            // applies, but on the opt-in path the gate is mandatory
            // — even on `CwdFallback`. `cairn_store_sqlite::open` is
            // create-and-migrate, so a `cairn mcp` invocation in a
            // random working directory would otherwise materialize a
            // fresh `.cairn/cairn.db` and silently fork the operator's
            // memory state across two vaults (round-2 review). The
            // legacy `single_tenant=false` arm above bypasses this
            // gate because it never opens SQLite.
            match crate::verbs::status::probe_vault_binding(vault_root) {
                crate::verbs::status::VaultBinding::Bound => {}
                crate::verbs::status::VaultBinding::Unbound => {
                    eprintln!(
                        "cairn mcp: {binding_origin} {} is not a Cairn vault \
                         (no .cairn/vault.id) — run `cairn bootstrap` first",
                        vault_root.display()
                    );
                    return ExitCode::from(78);
                }
                crate::verbs::status::VaultBinding::Invalid(reason) => {
                    eprintln!("cairn mcp: vault binding error — {reason}");
                    return ExitCode::from(78);
                }
            }
            // probe_binding is no longer load-bearing on the opt-in
            // path; suppress the unused-variable warning explicitly.
            let _ = probe_binding;

            // Open the memory store, boot the scheduler, serve MCP, then
            // shut the scheduler down — all inside one `block_on` so
            // `Scheduler::start` (which calls `tokio::spawn`) runs within
            // the tokio context.
            let db_path = store_db_path(vault_root);
            rt.block_on(async move {
                let sqlite_store: Arc<cairn_store_sqlite::SqliteMemoryStore> =
                    match cairn_store_sqlite::open(&db_path).await {
                        Ok(s) => Arc::new(s),
                        Err(e) => {
                            eprintln!("cairn mcp: failed to open SQLite store: {e}");
                            // Return an Err so the outer `match result` prints
                            // the exit code consistently.
                            return Err(cairn_mcp::TransportError::Service(format!(
                                "store open: {e}"
                            )));
                        }
                    };

                // Open a second rusqlite connection to the same DB for the
                // job store. The async `open` above (tokio-rusqlite) already
                // ran all migrations (including 0020 which provisions
                // `workflow_jobs`), so `SqliteJobStore::new` finds the
                // required schema objects. A dedicated connection avoids
                // contention with tokio-rusqlite's async connection pool;
                // `SQLite` WAL mode lets readers and the job-store writer
                // co-exist without blocking.
                let jobs_conn = match rusqlite::Connection::open(&db_path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("cairn mcp: failed to open job-store connection: {e}");
                        return Err(cairn_mcp::TransportError::Service(format!(
                            "job-store connection: {e}"
                        )));
                    }
                };
                let job_store: Arc<dyn cairn_core::contract::job_store::JobStore> =
                    match SqliteJobStore::new(jobs_conn) {
                        Ok(s) => Arc::new(s),
                        Err(e) => {
                            eprintln!("cairn mcp: failed to initialize job store: {e}");
                            return Err(cairn_mcp::TransportError::Service(format!(
                                "job-store init: {e}"
                            )));
                        }
                    };

                // Build the handler registry with both consolidation handlers.
                let consolidation_handler =
                    ConsolidationHandler::new(sqlite_store.clone(), config.consolidation);
                let forget_cleanup_handler =
                    ConsolidationForgetCleanupHandler::new(sqlite_store.clone());
                let registry = HandlerRegistryBuilder::default()
                    .with(Arc::new(consolidation_handler))
                    .with(Arc::new(forget_cleanup_handler))
                    .build();

                // Start the scheduler. `Scheduler::start` spawns tokio tasks
                // — must be called inside the tokio context (hence this async
                // block rather than before `block_on`).
                let incarnation_id = ulid::Ulid::new().to_string();
                let clock: Arc<dyn cairn_workflows::Clock> = Arc::new(SystemClock);
                let scheduler = Scheduler::start(
                    &incarnation_id,
                    job_store,
                    &registry,
                    clock,
                    SchedulerConfig::p0(),
                );
                tracing::info!("scheduler started (incarnation={incarnation_id})");

                // Upcast to dyn MemoryStore for the verb layer; keep the
                // concrete Arc<SqliteMemoryStore> for the graph-tool layer
                // (Plan C Task 19).
                let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
                    sqlite_store.clone();
                let serve_result = cairn_mcp::serve_stdio_with_store(
                    store,
                    sqlite_store,
                    resolver,
                    config.clone(),
                    principal,
                )
                .await;

                // Graceful scheduler shutdown — cancel tasks and await them.
                tracing::info!("shutting down scheduler");
                scheduler.shutdown().await;

                serve_result
            })
        }
        None => {
            // No opt-in: deprecated unwired path. NO SQLite open.
            // Vault binding intentionally not probed — this surface
            // does not touch the filesystem.
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
            ExitCode::from(69)
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
        assert!(resolve_scope_components(&cfg).is_none());
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
