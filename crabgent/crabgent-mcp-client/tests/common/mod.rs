// Shared integration-test helpers. Each test binary pulls in only the subset
// it needs, so an `#[expect(dead_code)]` would be unfulfilled in the binaries
// that do use every item. `#[allow]` is the documented exception for shared
// test helpers (see .claude/rules/rust-quality.md).
#![allow(
    dead_code,
    reason = "shared test helpers, not every binary uses every item"
)]

use std::env;

use httpmock::{HttpMockRequest, HttpMockResponse, Method::POST, MockServer};

pub const TEST_SESSION_ID: &str = "00000000-0000-7000-8000-000000000001";

pub struct McpTestCtx {
    pub base_url: String,
    pub token: String,
    pub server_name: String,
    mock_server: Option<MockServer>,
}

impl McpTestCtx {
    pub const fn mock_server(&self) -> Option<&MockServer> {
        self.mock_server.as_ref()
    }
}

pub async fn mcp_test_ctx() -> McpTestCtx {
    let base_url = env::var("MCP_TEST_BASE_URL").ok();
    let token = env::var("MCP_TEST_TOKEN").ok();
    let server_name = env::var("MCP_TEST_SERVER_NAME").ok();

    if let (Some(base_url), Some(token), Some(server_name)) = (base_url, token, server_name) {
        return McpTestCtx {
            base_url,
            token,
            server_name,
            mock_server: None,
        };
    }

    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/");
        then.status(200)
            .header("content-type", "application/json")
            .respond_with(dispatch_mock_rpc);
    });

    McpTestCtx {
        base_url: server.url("/"),
        token: "secret-test-token-12345".to_string(),
        server_name: "test_server".to_string(),
        mock_server: Some(server),
    }
}

fn dispatch_mock_rpc(req: &HttpMockRequest) -> HttpMockResponse {
    let value = serde_json::from_slice::<serde_json::Value>(req.body_ref()).unwrap_or_default();
    let method = value
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();

    let mut response = HttpMockResponse::builder()
        .status(200)
        .header("content-type", "application/json");

    let response_body = match method {
        "initialize" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": value.get("id").cloned().unwrap_or(serde_json::Value::Null),
            "result": {"protocolVersion": "2025-03-26", "capabilities": {}}
        }),
        "tools/list" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": value.get("id").cloned().unwrap_or(serde_json::Value::Null),
            "result": {"tools": []}
        }),
        "tools/call" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": value.get("id").cloned().unwrap_or(serde_json::Value::Null),
            "result": {
                "content": [{"type": "text", "text": "ok"}],
                "isError": false
            }
        }),
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": value.get("id").cloned().unwrap_or(serde_json::Value::Null),
            "error": {"code": -32601, "message": "method not found"}
        }),
    };
    if method == "initialize" {
        response = response.header("Mcp-Session-Id", TEST_SESSION_ID);
    }

    response
        .body(serde_json::to_vec(&response_body).unwrap_or_default())
        .build()
}

pub fn mount_initialize(server: &MockServer) {
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("initialize");
        then.status(200)
            .header("content-type", "application/json")
            .header("Mcp-Session-Id", TEST_SESSION_ID)
            .json_body(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 0,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {}
                }
            }));
    });
}
