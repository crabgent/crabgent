mod client_support;
mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crabgent_mcp_client::{McpClientBuilder, McpError};
use httpmock::Method::POST;
use serde_json::{Value, json};

use client_support::*;
use common::{TEST_SESSION_ID, mcp_test_ctx};

#[test]
fn builder_tool_name_collision_duplicate_alias() {
    let first = config("sg42", "http://localhost/mcp", "token-a");
    let duplicate = config("sg42", "http://localhost/other", "token-b");
    let mut builder = McpClientBuilder::new();

    builder
        .add_server(first)
        .expect("first server should be accepted");
    let err = builder
        .add_server(duplicate)
        .err()
        .expect("duplicate server alias should fail");

    assert!(matches!(err, McpError::InvalidConfig(_)));
}

#[test]
fn builder_add_servers_builds_one_client_per_config() {
    let mut builder = McpClientBuilder::new();
    builder
        .add_servers([
            config("alpha", "http://localhost/alpha", "token-a"),
            config("beta", "http://localhost/beta", "token-b"),
        ])
        .expect("distinct servers should be accepted");

    let mut clients = builder.build().expect("client builds");
    clients.sort_by(|a, b| a.0.cmp(&b.0));

    let names = clients
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["alpha", "beta"]);
}

#[test]
fn builder_add_servers_rejects_duplicate_name() {
    let mut builder = McpClientBuilder::new();
    let err = builder
        .add_servers([
            config("dup", "http://localhost/a", "token-a"),
            config("dup", "http://localhost/b", "token-b"),
        ])
        .err()
        .expect("duplicate name in batch should fail");

    assert!(matches!(err, McpError::InvalidConfig(_)));
}

#[test]
fn builder_server_name_invalid_chars() {
    for name in ["bad name", "bad.name"] {
        let mut builder = McpClientBuilder::new();
        let mut config = config("valid", "http://localhost/mcp", "token");
        config.name = name.to_string();

        let err = builder
            .add_server(config)
            .err()
            .expect("invalid server alias should fail");

        assert!(matches!(err, McpError::InvalidConfig(_)));
    }
}

#[tokio::test]
async fn client_discover_happy_path() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_json(
        server,
        "tools/list",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [
                    {"name": "search_docs", "description": "Search docs", "inputSchema": {"type": "object"}},
                    {"name": "create_ticket", "description": "Create ticket", "inputSchema": {"type": "object"}}
                ]
            }
        }),
    );
    let client = client(&ctx);

    let defs = client.discover().await.expect("discover should succeed");

    assert_eq!(defs.tools.len(), 2);
    assert_eq!(defs.tools[0].name, "search_docs");
    assert_eq!(defs.tools[1].name, "create_ticket");
}

#[tokio::test]
async fn client_discover_sends_empty_params_to_tools_list() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("tools/list");
        then.status(200)
            .header("content-type", "application/json")
            .respond_with(|request| {
                let body: Value =
                    serde_json::from_slice(request.body_ref()).expect("request body is JSON");
                if body.get("params") == Some(&json!({})) {
                    json_response(request, &json!({ "tools": [] }))
                } else {
                    json_error_response(request, 200, -32602, "params required")
                }
            });
    });
    let client = client(&ctx);

    let defs = client.discover().await.expect("discover should succeed");

    assert!(defs.tools.is_empty());
}

#[tokio::test]
async fn client_initialize_sends_spec_params() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    server.reset();
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("initialize");
        then.status(200)
            .header("content-type", "application/json")
            .respond_with(|request| {
                let body: Value =
                    serde_json::from_slice(request.body_ref()).expect("request body is JSON");
                if initialize_has_spec_params(&body) {
                    json_session_response(
                        request,
                        &json!({
                            "protocolVersion": "2025-03-26",
                            "capabilities": {}
                        }),
                    )
                } else {
                    json_initialize_params_error_response(request)
                }
            });
    });
    mount_json(
        server,
        "tools/list",
        json_rpc_result(&json!({ "tools": [] })),
    );
    let client = client(&ctx);

    let defs = client.discover().await.expect("discover should succeed");

    assert!(defs.tools.is_empty());
}

#[tokio::test]
async fn client_initializes_session_before_discover() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    let list_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/")
            .body_includes("tools/list")
            .header("Mcp-Session-Id", TEST_SESSION_ID);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json_rpc_result(&json!({ "tools": [] })));
    });
    let client = client(&ctx);

    let defs = client.discover().await.expect("discover should succeed");

    list_mock.assert();
    assert!(defs.tools.is_empty());
}

#[tokio::test]
async fn client_accepts_session_id_from_initialize_body() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    server.reset();
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("initialize");
        then.status(200)
            .header("content-type", "application/json")
            .respond_with(|request| {
                json_response(
                    request,
                    &json!({
                        "protocolVersion": "2025-03-26",
                        "capabilities": {},
                        "sessionId": TEST_SESSION_ID
                    }),
                )
            });
    });
    let list_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/")
            .body_includes("tools/list")
            .header("Mcp-Session-Id", TEST_SESSION_ID);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json_rpc_result(&json!({ "tools": [] })));
    });
    let client = client(&ctx);

    let defs = client.discover().await.expect("discover should succeed");

    list_mock.assert();
    assert!(defs.tools.is_empty());
}

#[tokio::test]
async fn client_omits_session_header_after_stateless_initialize() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    server.reset();
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("initialize");
        then.status(200)
            .header("content-type", "application/json")
            .respond_with(|request| {
                json_response(
                    request,
                    &json!({
                        "protocolVersion": "2025-03-26",
                        "capabilities": {}
                    }),
                )
            });
    });
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("tools/list");
        then.status(200)
            .header("content-type", "application/json")
            .respond_with(|request| {
                if request.headers().contains_key("Mcp-Session-Id") {
                    json_error_response(request, 400, -32602, "unexpected session header")
                } else {
                    json_response(request, &json!({ "tools": [] }))
                }
            });
    });
    let client = client(&ctx);

    let defs = client.discover().await.expect("discover should succeed");

    assert!(defs.tools.is_empty());
}

#[tokio::test]
async fn client_reinitializes_once_after_stale_session() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    server.reset();
    let initialize_count = Arc::new(AtomicUsize::new(0));
    let initialize_count_for_mock = Arc::clone(&initialize_count);
    server.mock(move |when, then| {
        when.method(POST).path("/").body_includes("initialize");
        then.status(200)
            .header("content-type", "application/json")
            .header("Mcp-Session-Id", TEST_SESSION_ID)
            .respond_with(move |request| {
                initialize_count_for_mock.fetch_add(1, Ordering::SeqCst);
                json_session_response(
                    request,
                    &json!({
                        "protocolVersion": "2025-03-26",
                        "capabilities": {}
                    }),
                )
            });
    });
    let list_count = Arc::new(AtomicUsize::new(0));
    let list_count_for_mock = Arc::clone(&list_count);
    server.mock(move |when, then| {
        when.method(POST)
            .path("/")
            .body_includes("tools/list")
            .header("Mcp-Session-Id", TEST_SESSION_ID);
        then.status(200)
            .header("content-type", "application/json")
            .respond_with(move |request| {
                if list_count_for_mock.fetch_add(1, Ordering::SeqCst) == 0 {
                    return json_error_response(request, 404, -32_001, "session not found");
                }
                json_response(request, &json!({ "tools": [] }))
            });
    });
    let client = client(&ctx);

    let defs = client.discover().await.expect("discover should retry");

    assert!(defs.tools.is_empty());
    assert_eq!(initialize_count.load(Ordering::SeqCst), 2);
    assert_eq!(list_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn client_discover_skips_invalid_tool_names() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_json(
        server,
        "tools/list",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "tools": [
                    {"name": "search_docs", "description": "Search docs", "inputSchema": {"type": "object"}},
                    {"name": "weird name", "description": "Skip me", "inputSchema": {"type": "object"}}
                ]
            }
        }),
    );
    let client = client(&ctx);

    let defs = client.discover().await.expect("discover should succeed");

    assert_eq!(defs.tools.len(), 1);
    assert_eq!(defs.tools[0].name, "search_docs");
}

#[tokio::test]
async fn client_call_tool_json_response() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_json(server, "tools/call", call_result("final answer", false));
    let client = client(&ctx);

    let result = client
        .call_tool("search_docs", json!({"q": "mcp"}), None)
        .await
        .expect("tool call should succeed");

    assert_eq!(result.content, Value::String("final answer".to_string()));
    assert!(!result.is_error);
}

#[tokio::test]
async fn client_call_tool_sse_multi_event() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_sse(
        server,
        concat!(
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"intermediate\"}],\"isError\":false}}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"final\"}],\"isError\":false}}\n\n",
        ),
    );
    let client = client(&ctx);

    let result = client
        .call_tool("search_docs", json!({}), None)
        .await
        .expect("SSE tool call should succeed");

    assert_eq!(result.content, Value::String("final".to_string()));
    assert!(!result.is_error);
}

#[tokio::test]
async fn client_call_tool_sse_with_trailing_heartbeat() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_sse(
        server,
        concat!(
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"final\"}],\"isError\":false}}\n\n",
            ": heartbeat\n\n",
        ),
    );
    let client = client(&ctx);

    let result = client
        .call_tool("search_docs", json!({}), None)
        .await
        .expect("SSE tool call should ignore trailing heartbeat");

    assert_eq!(result.content, Value::String("final".to_string()));
    assert!(!result.is_error);
}

#[tokio::test]
async fn client_call_tool_is_error_propagates() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_json(
        server,
        "tools/call",
        call_result("tool rejected args", true),
    );
    let client = client(&ctx);

    let err = client
        .call_tool("search_docs", json!({}), None)
        .await
        .expect_err("isError should become tool call error");

    assert!(matches!(err, McpError::ToolCall(message) if message == "tool rejected args"));
}

#[tokio::test]
async fn client_call_tool_auth_failure_returns_authfailed() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    server.mock(|when, then| {
        when.method(POST)
            .path("/")
            .header("authorization", "Bearer secret-test-token-12345");
        then.status(401)
            .header("content-type", "application/json")
            .body("secret-test-token-12345 token rejected");
    });
    let client = client(&ctx);

    let err = client
        .call_tool("search_docs", json!({}), None)
        .await
        .expect_err("auth failure should fail");
    let message = err.to_string();

    assert!(matches!(err, McpError::AuthFailed));
    assert!(!message.contains("secret-test-token-12345"));
}

#[tokio::test]
async fn client_initialize_auth_failure_returns_authfailed() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    server.reset();
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("initialize");
        then.status(401)
            .header("content-type", "application/json")
            .body("secret-test-token-12345 token rejected");
    });
    let client = client(&ctx);

    let err = client
        .discover()
        .await
        .expect_err("initialize auth failure should fail");

    assert!(matches!(err, McpError::AuthFailed));
    assert!(!err.to_string().contains("secret-test-token-12345"));
}

#[tokio::test]
async fn client_body_size_cap_exceeded() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    let large_body = "x".repeat(1_048_576);
    server.mock(move |when, then| {
        when.method(POST).path("/").body_includes("tools/call");
        then.status(200)
            .header("content-type", "application/json")
            .body(large_body);
    });
    let client = client_with_cap(&ctx, 100 * 1024);

    let err = client
        .call_tool("search_docs", json!({}), None)
        .await
        .expect_err("body above cap should fail");

    assert!(matches!(err, McpError::OutputCapExceeded));
}

#[tokio::test]
async fn client_malformed_sse_returns_decode_error() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_sse(server, "data: not-json\n\n");
    let client = client(&ctx);

    let err = client
        .call_tool("search_docs", json!({}), None)
        .await
        .expect_err("malformed SSE should fail");

    assert!(matches!(err, McpError::Decode(_)));
}
