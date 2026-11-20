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
            .or_else(|| {
                let listed = self.adapter.browser.as_ref()?.listed()?;
                catalog::relayed(listed.iter().find(|tool| tool["name"] == name)?)
            })
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
        let mut tools = catalog::TOOLS.clone();
        // Tools that cannot be listed are left out; the host's own still work.
        if let Some(relay) = self.adapter.browser.clone()
            && let Ok(Ok(listed)) =
                tokio::task::spawn_blocking(move || relay.tools().map(<[_]>::to_vec)).await
        {
            tools.extend(listed.iter().filter_map(catalog::relayed));
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tools = self.adapter.tools.clone();
        let selected = selected_target(&ctx.meta)?;
        let args = Value::Object(request.arguments.unwrap_or_default());
        let name = request.name.to_string();
        let result = match name.as_str() {
            catalog::DESCRIBE => Ok(tools.describe()),
            catalog::EXEC_RUN => {
                cancellable(ctx.ct.cancelled(), move |left| {
                    tools.run(args, selected, left)
                })
                .await
            }
            _ => match self.adapter.browser.clone() {
                Some(relay) if relay.owns(&name) => {
                    cancellable(ctx.ct.cancelled(), move |left| {
                        tools.gated(selected, left, |stop| relay.call(&name, args, stop))
                    })
                    .await
                }
                _ => return Err(ErrorData::invalid_params("Unknown Windows tool.", None)),
            },
        };
        Ok(tool_result(ctx.id.to_string(), result))
    }
}

/// Runs a call off the async threads. MCP cancels a call when its response stream closes, as
/// when a client stops waiting: only that call is stopped, and the lease stays.
async fn cancellable(
    cancelled: impl Future<Output = ()>,
    work: impl FnOnce(&AtomicBool) -> RpcResult + Send + 'static,
) -> RpcResult {
    let left = Arc::new(AtomicBool::new(false));
    let seen = left.clone();
    let running = tokio::task::spawn_blocking(move || work(&seen));
    tokio::select! {
        result = running => result.unwrap_or_else(|_| {
            Err(RpcError::unknown("internal_error", "The outcome is unknown."))
        }),
        _ = cancelled => {
            left.store(true, Ordering::SeqCst);
            Err(RpcError::unknown("cancelled", "Cancelled; check what happened before running it again."))
        }
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
