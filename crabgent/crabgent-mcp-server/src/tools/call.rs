use serde::Deserialize;
use serde_json::{Value, json};

use crate::session::McpSessionEntry;
use crate::tools::chat::{CHAT_TOOL_NAME, handle_chat};
use crate::wire::{
    ERR_INVALID_PARAMS, ERR_METHOD_NOT_FOUND, JsonRpcRequest, JsonRpcResponse, error_response,
    success_response,
};
use crate::{McpServer, McpServerError};

#[derive(Debug, Deserialize)]
struct ToolsCallParams {
    name: String,
    arguments: Value,
}

pub async fn handle_tools_call(
    server: &McpServer,
    session: &McpSessionEntry,
    request: &JsonRpcRequest,
) -> JsonRpcResponse {
    match execute_tools_call(server, session, request).await {
        Ok(result) => success_response(request.id.clone(), result),
        Err(err) => tool_call_error_response(request, &err),
    }
}

async fn execute_tools_call(
    server: &McpServer,
    session: &McpSessionEntry,
    request: &JsonRpcRequest,
) -> Result<Value, McpServerError> {
    let params = parse_params(request.params.clone())?;
    if params.name == CHAT_TOOL_NAME {
        if !server.exposes_chat_tool() {
            return Err(McpServerError::MethodNotFound(params.name));
        }
        if tool_policy_denied(server, session, CHAT_TOOL_NAME).await {
            return Ok(policy_denied_result());
        }
        return handle_chat(server, session, params.arguments).await;
    }

    let tool = server
        .visible_kernel_tool(&params.name)
        .ok_or_else(|| McpServerError::ToolNotFound(params.name.clone()))?;
    if tool_policy_denied(server, session, &params.name).await {
        return Ok(policy_denied_result());
    }
    let ctx = crabgent_core::ToolCtx::new(session.subject.clone())
        .with_cancel(session.cancel_token.clone());
    let output = tool
        .execute_result(params.arguments, &ctx)
        .await
        .map_err(|err| McpServerError::ToolExecution(err.to_string()))?;

    Ok(json!({
        "content": [{
            "type": "text",
            "text": output_to_text(&output.output),
        }],
        "isError": output.is_error,
    }))
}

fn parse_params(params: Value) -> Result<ToolsCallParams, McpServerError> {
    serde_json::from_value(params)
        .map_err(|err| McpServerError::InvalidParams(format!("invalid tools/call params: {err}")))
}

async fn tool_policy_denied(server: &McpServer, session: &McpSessionEntry, name: &str) -> bool {
    let policy_result = server
        .kernel()
        .policy()
        .allow(
            &session.subject,
            &crabgent_core::Action::tool(name.to_owned()),
        )
        .await;

    matches!(policy_result, crabgent_core::PolicyDecision::Deny(_))
}

fn policy_denied_result() -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": "policy denied",
        }],
        "isError": true,
    })
}

fn tool_call_error_response(request: &JsonRpcRequest, err: &McpServerError) -> JsonRpcResponse {
    match err {
        McpServerError::MethodNotFound(name) | McpServerError::ToolNotFound(name) => {
            error_response(
                request.id.clone(),
                ERR_METHOD_NOT_FOUND,
                method_not_found_message(name),
                None,
            )
        }
        McpServerError::InvalidParams(message) => error_response(
            request.id.clone(),
            ERR_INVALID_PARAMS,
            message.clone(),
            None,
        ),
        _ => error_response(
            request.id.clone(),
            crate::ERR_INTERNAL,
            "tool execution failed",
            None,
        ),
    }
}

fn method_not_found_message(_name: &str) -> String {
    "method not found".into()
}

fn output_to_text(output: &Value) -> String {
    output
        .as_str()
        .map_or_else(|| output.to_string(), ToOwned::to_owned)
}
