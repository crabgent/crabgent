use crate::McpServer;
use crate::session::McpSessionEntry;
use crate::wire::{JsonRpcRequest, JsonRpcResponse};

pub mod call;
pub mod chat;
pub mod list;

pub async fn handle_tools_dispatch(
    server: &McpServer,
    session: &McpSessionEntry,
    request: &JsonRpcRequest,
) -> JsonRpcResponse {
    if request.method == "tools/list" {
        return list::handle_tools_list(server, request);
    }

    call::handle_tools_call(server, session, request).await
}
