use crabgent_mcp_client::{McpClient, McpClientBuilder, McpServerConfig};
use httpmock::{HttpMockRequest, HttpMockResponse, Method::POST};
use secrecy::SecretString;
use serde_json::{Value, json};

use crate::common::{self, TEST_SESSION_ID, mount_initialize};

pub fn client(ctx: &common::McpTestCtx) -> McpClient {
    client_with_cap(ctx, 1_048_576)
}

pub fn reset_mock_server(ctx: &common::McpTestCtx) -> Option<&httpmock::MockServer> {
    let server = ctx.mock_server()?;
    server.reset();
    mount_initialize(server);
    Some(server)
}

pub fn client_with_cap(ctx: &common::McpTestCtx, max_response_bytes: usize) -> McpClient {
    let mut builder = McpClientBuilder::new();
    builder
        .add_server(
            config(&ctx.server_name, &ctx.base_url, &ctx.token)
                .with_max_response_bytes(max_response_bytes),
        )
        .expect("test server should be valid");
    let mut clients = builder.build().expect("client builds");

    clients.pop().expect("one client").1
}

pub fn config(name: &str, base_url: &str, token: &str) -> McpServerConfig {
    McpServerConfig::new(name, base_url)
        .expect("valid test config")
        .with_token(SecretString::from(token.to_string()))
}

pub fn mount_json(server: &httpmock::MockServer, method: &str, response: Value) {
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes(method);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(response);
    });
}

pub fn mount_sse(server: &httpmock::MockServer, body: &str) {
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("tools/call");
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(body);
    });
}

pub fn call_result(text: &str, is_error: bool) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "content": [{"type": "text", "text": text}],
            "isError": is_error
        }
    })
}

pub fn json_rpc_result(result: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": result
    })
}

pub fn json_response(request: &HttpMockRequest, result: &Value) -> HttpMockResponse {
    HttpMockResponse::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": rpc_id(request),
                "result": result,
            }))
            .expect("JSON-RPC response serializes"),
        )
        .build()
}

pub fn json_session_response(request: &HttpMockRequest, result: &Value) -> HttpMockResponse {
    HttpMockResponse::builder()
        .status(200)
        .header("content-type", "application/json")
        .header("Mcp-Session-Id", TEST_SESSION_ID)
        .body(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": rpc_id(request),
                "result": result,
            }))
            .expect("JSON-RPC response serializes"),
        )
        .build()
}

pub fn json_error_response(
    request: &HttpMockRequest,
    status: u16,
    code: i64,
    message: &str,
) -> HttpMockResponse {
    HttpMockResponse::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": rpc_id(request),
                "error": {"code": code, "message": message},
            }))
            .expect("JSON-RPC error response serializes"),
        )
        .build()
}

pub fn json_initialize_params_error_response(request: &HttpMockRequest) -> HttpMockResponse {
    HttpMockResponse::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(
            serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": rpc_id(request),
                "error": {
                    "code": -32603,
                    "message": "initialize params required",
                    "data": [
                        {
                            "code": "invalid_type",
                            "expected": "string",
                            "received": "undefined",
                            "path": ["params", "protocolVersion"],
                            "message": "Required"
                        },
                        {
                            "code": "invalid_type",
                            "expected": "object",
                            "received": "undefined",
                            "path": ["params", "capabilities"],
                            "message": "Required"
                        },
                        {
                            "code": "invalid_type",
                            "expected": "object",
                            "received": "undefined",
                            "path": ["params", "clientInfo"],
                            "message": "Required"
                        }
                    ]
                },
            }))
            .expect("JSON-RPC initialize error response serializes"),
        )
        .build()
}

pub fn initialize_has_spec_params(body: &Value) -> bool {
    let Some(params) = body.get("params").and_then(Value::as_object) else {
        return false;
    };
    let Some(client_info) = params.get("clientInfo").and_then(Value::as_object) else {
        return false;
    };

    params.get("protocolVersion") == Some(&json!("2025-03-26"))
        && params.get("capabilities") == Some(&json!({}))
        && client_info.get("name") == Some(&json!("crabgent-mcp-client"))
        && client_info.get("version") == Some(&json!(env!("CARGO_PKG_VERSION")))
}

fn rpc_id(request: &HttpMockRequest) -> Value {
    serde_json::from_slice::<Value>(request.body_ref())
        .ok()
        .and_then(|value| value.get("id").cloned())
        .unwrap_or(Value::Null)
}
