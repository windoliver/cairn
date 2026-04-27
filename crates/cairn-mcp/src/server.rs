//! rmcp `ServerHandler` implementation for the Cairn MCP stdio adapter.
//!
//! Transport selection lives in `cairn-cli`; this module is protocol-only
//! (brief §4 `MCPServer` contract, CLAUDE.md §6.12).

use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServiceExt as _};
use tracing::instrument;

use cairn_core::config::CairnConfig;

use crate::dispatch;
use crate::error::McpTransportError;
use crate::generated::TOOLS;

/// Cairn MCP server handler.
///
/// Implements `rmcp::ServerHandler` with the eight core verbs from the
/// generated `TOOLS` registry. Config is held for vault-path and capability
/// resolution (store wiring in issue #9).
#[derive(Clone)]
pub struct CairnMcpHandler {
    #[allow(dead_code)] // used in #9 when store wiring lands
    config: Arc<CairnConfig>,
}

impl CairnMcpHandler {
    /// Construct a handler backed by the given Cairn configuration.
    #[must_use]
    pub fn new(config: CairnConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }
}

impl ServerHandler for CairnMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("cairn-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Cairn agent-memory framework — eight verbs over the cairn.mcp.v1 envelope.",
            )
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = TOOLS
            .iter()
            .map(|decl| {
                let schema_val: serde_json::Value = serde_json::from_slice(decl.input_schema)
                    .map_err(|e| McpError::invalid_params(e.to_string(), None))?;
                let schema_obj = schema_val.as_object().cloned().unwrap_or_default();
                Ok(Tool::new(decl.name, decl.description, Arc::new(schema_obj)))
            })
            .collect::<Result<Vec<Tool>, McpError>>()?;
        Ok(ListToolsResult::with_all_items(tools))
    }

    #[instrument(skip(self, _ctx), fields(verb = %params.name))]
    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let response = dispatch::dispatch(&params.name, params.arguments.as_ref());
        let json = serde_json::to_string(&response)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        tracing::info!(
            operation_id = %response.operation_id.0,
            status = ?response.status,
            "cairn verb dispatched over MCP"
        );
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }
}

/// Start the Cairn MCP server on stdio and run until stdin closes.
///
/// Called by `cairn-cli` from within a multi-thread tokio runtime.
/// Transport selection (stdio vs SSE) lives in the caller.
pub async fn serve_stdio(config: CairnConfig) -> Result<(), McpTransportError> {
    let handler = CairnMcpHandler::new(config);
    let running = handler
        .serve(rmcp::transport::io::stdio())
        .await
        .map_err(|e| McpTransportError::Initialize(e.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|e| McpTransportError::Initialize(e.to_string()))?;
    Ok(())
}
