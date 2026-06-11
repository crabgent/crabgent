use serde_json::{Value, json};

use crate::McpServer;
use crate::tools::chat::{CHAT_INPUT_SCHEMA, CHAT_OUTPUT_SCHEMA, CHAT_TOOL_NAME};
use crate::wire::{JsonRpcRequest, JsonRpcResponse, success_response};

pub fn handle_tools_list(server: &McpServer, request: &JsonRpcRequest) -> JsonRpcResponse {
    let mut tools = Vec::with_capacity(server.kernel().tool_count() + 1);
    if server.exposes_chat_tool() {
        tools.push(chat_tool_def());
    }

    for tool in server.visible_kernel_tools() {
        tools.push(json!({
            "name": tool.name(),
            "description": tool.description(),
            "inputSchema": tool.parameters_schema(),
        }));
    }

    success_response(request.id.clone(), json!({ "tools": tools }))
}

fn chat_tool_def() -> Value {
    json!({
        "name": CHAT_TOOL_NAME,
        "description": "Send a message into the crabgent kernel and return the assistant reply.",
        "inputSchema": CHAT_INPUT_SCHEMA.clone(),
        "outputSchema": CHAT_OUTPUT_SCHEMA.clone(),
    })
}
