//! MCP `tools/list` and `tools/call`. Policy lives in `Adapter`; this file only translates.
use super::{Adapter, catalog};
use rmcp::{
    ErrorData, ServerHandler,
    model::*,
    service::{RequestContext, RoleServer},
};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use windows_tools_core::{Response, RpcError, RpcResult, Target};

/// `_meta` key carrying the connector-selected target (vendor-prefixed per MCP `_meta` rules).
pub const TARGET_META: &str = "windows-tools/target";

const INSTRUCTIONS: &str = "Commands on a Windows session. Listing tools grants nothing: a connector holds a lease of a few seconds through /connector/control and renews it, and a command runs only while it is held. Nothing is replayed: after a lost reply, check what happened before running a command again.";

#[derive(Clone)]
pub struct Handler {
    pub adapter: Arc<Adapter>,
}

impl ServerHandler for Handler {
    fn get_tool(&self, name: &str) -> Option<Tool> {
        catalog::TOOLS
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(catalog::TOOLS.clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tools = self.adapter.tools.clone();
        let selected = selected_target(&ctx.meta)?;
        let result = match request.name.as_ref() {
            catalog::DESCRIBE => Ok(tools.describe()),
            catalog::EXEC_RUN => {
                let args = Value::Object(request.arguments.unwrap_or_default());
                // MCP cancels a call when its response stream closes, as when a client stops
                // waiting. Only that call is stopped; the lease stays.
                let left = Arc::new(AtomicBool::new(false));
                let seen = left.clone();
                let work = tokio::task::spawn_blocking(move || tools.run(args, selected, &seen));
                tokio::select! {
                    result = work => result.unwrap_or_else(|_| {
                        Err(RpcError::unknown("internal_error", "The outcome is unknown."))
                    }),
                    _ = ctx.ct.cancelled() => {
                        left.store(true, Ordering::SeqCst);
                        Err(RpcError::unknown("cancelled", "Cancelled; check what happened before running it again."))
                    }
                }
            }
            _ => return Err(ErrorData::invalid_params("Unknown Windows tool.", None)),
        };
        Ok(tool_result(ctx.id.to_string(), result))
    }
}

fn selected_target(meta: &RequestMetaObject) -> Result<Option<Target>, ErrorData> {
    meta.get(TARGET_META)
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|_| ErrorData::invalid_params("Invalid target metadata.", None))
}

/// The host's envelope as structured content; a failed call also sets `isError`.
fn tool_result(id: String, result: RpcResult) -> CallToolResponse {
    let response = Response::new(id, result);
    let failed = response.error.is_some();
    let value = serde_json::to_value(response).expect("response");
    if failed {
        CallToolResult::structured_error(value)
    } else {
        CallToolResult::structured(value)
    }
    .into()
}
