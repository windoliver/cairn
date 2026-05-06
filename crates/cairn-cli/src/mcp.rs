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
/// Exit codes: 0 success, 69 `EX_UNAVAILABLE` (transport/IO/store), 78 `EX_CONFIG`.
#[must_use]
pub fn run(vault_root: &Path, config: CairnConfig) -> ExitCode {
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
    let result = match resolve_scope_components(&config) {
        Some(ResolvedMcpScope {
            resolver,
            principal,
        }) => {
            let sqlite_store: Arc<cairn_store_sqlite::SqliteMemoryStore> =
                match rt.block_on(cairn_store_sqlite::open(&store_db_path(vault_root))) {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        eprintln!("cairn mcp: failed to open SQLite store: {e}");
                        return ExitCode::from(69);
                    }
                };
            let store: Arc<dyn cairn_core::contract::memory_store::MemoryStore> =
                sqlite_store.clone();
            // Plan A signature (4 params). Plan C Task 19 will add
            // `sqlite_store` as a separate parameter; the unused
            // binding is kept here to make the Plan C diff additive.
            #[allow(unused_variables)]
            let _sqlite_store = sqlite_store;
            rt.block_on(cairn_mcp::serve_stdio_with_store(
                store, resolver, config, principal,
            ))
        }
        None => {
            // No opt-in: deprecated unwired path. NO SQLite open.
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
