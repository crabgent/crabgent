mod common;

use std::sync::Arc;

use crabgent_core::{Subject, ToolCtx};
use crabgent_mcp_client::{McpClient, McpClientBuilder, McpError, McpServerConfig, McpToolDef};
use httpmock::Method::POST;
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use common::{mcp_test_ctx, mount_initialize};

#[tokio::test]
async fn factory_produces_correct_count() {
    let ctx = mcp_test_ctx().await;
    let client = Arc::new(client(&ctx));
    let factory = crabgent_mcp_client::McpToolFactory::from_client(
        "sg42",
        defs(&["search_docs", "read_doc"]),
        &client,
        4096,
    )
    .expect("factory should build");

    let tools = factory.into_tools();

    assert_eq!(tools.len(), 2);
}

#[tokio::test]
async fn factory_tool_name_collision_intra_server() {
    let ctx = mcp_test_ctx().await;
    let client = Arc::new(client(&ctx));

    let err = crabgent_mcp_client::McpToolFactory::from_client(
        "sg42",
        defs(&["search_docs", "search_docs"]),
        &client,
        4096,
    )
    .err()
    .expect("duplicate tool name should fail");

    assert!(matches!(err, McpError::InvalidConfig(_)));
}

#[tokio::test]
async fn tool_name_returns_prefixed() {
    let tool = one_tool("sg42", "search_docs", 4096).await;

    assert_eq!(tool.name(), "sg42__search_docs");
}

#[tokio::test]
async fn tool_execute_uses_original_name_not_prefixed() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    let original_name_mock = server.mock(|when, then| {
        when.method(POST)
            .path("/")
            .body_includes("\"name\":\"search_docs\"")
            .body_excludes("sg42__search_docs");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(call_result("ok"));
    });
    let tool = one_tool_with_ctx(&ctx, "sg42", "search_docs", 4096);

    let result = tool
        .execute(json!({"q": "docs"}), &tool_ctx())
        .await
        .expect("tool execution should succeed");

    original_name_mock.assert();
    assert_eq!(result, Value::String("ok".to_string()));
}

#[tokio::test]
async fn tool_execute_propagates_cancellation() {
    let tool = one_tool("sg42", "search_docs", 4096).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let ctx = tool_ctx().with_cancel(cancel);

    let err = tool
        .execute(json!({}), &ctx)
        .await
        .expect_err("cancelled tool execution should fail");

    assert_eq!(err.to_string(), "execution failed: cancelled");
}

#[tokio::test]
async fn tool_execute_returns_value_string() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_call(server, "hello");
    let tool = one_tool_with_ctx(&ctx, "sg42", "search_docs", 4096);

    let result = tool
        .execute(json!({"q": "docs"}), &tool_ctx())
        .await
        .expect("tool execution should succeed");

    assert_eq!(result, Value::String("hello".to_string()));
}

#[tokio::test]
async fn tool_execute_output_capped_at_configured_limit() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_call(server, &"x".repeat(10 * 1024));
    let tool = one_tool_with_ctx(&ctx, "sg42", "search_docs", 1024);

    let result = tool
        .execute(json!({}), &tool_ctx())
        .await
        .expect("tool execution should succeed");

    let Value::String(text) = result else {
        panic!("tool output should be a string");
    };
    assert!(text.len() <= 1024);
}

#[tokio::test]
async fn tool_execute_output_cap_keeps_utf8_boundary() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_call(server, &format!("abcd{}tail", '\u{1F600}'));
    let tool = one_tool_with_ctx(&ctx, "sg42", "search_docs", 6);

    let result = tool
        .execute(json!({}), &tool_ctx())
        .await
        .expect("tool execution should succeed");

    let Value::String(text) = result else {
        panic!("tool output should be a string");
    };
    assert_eq!(text, "abcd");
    assert!(text.len() <= 6);
}

#[tokio::test]
async fn auth_failure_no_token_leak() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    server.mock(|when, then| {
        when.method(POST)
            .path("/")
            .header("authorization", "Bearer secret-foo-987");
        then.status(401)
            .header("content-type", "application/json")
            .body("secret-foo-987 rejected with 401");
    });
    let tool = one_tool_with_token(&ctx, "sg42", "search_docs", "secret-foo-987", 4096);

    let result = tool
        .execute_result(json!({}), &tool_ctx())
        .await
        .expect("execute_result should produce soft error");

    assert!(result.is_error);
    assert!(!result.output.to_string().contains("secret-foo-987"));
    assert!(!result.output.to_string().contains("401"));
}

async fn one_tool(
    server_name: &str,
    tool_name: &str,
    max_output_bytes: usize,
) -> Arc<dyn crabgent_core::Tool> {
    let ctx = mcp_test_ctx().await;
    one_tool_with_ctx(&ctx, server_name, tool_name, max_output_bytes)
}

fn one_tool_with_ctx(
    ctx: &common::McpTestCtx,
    server_name: &str,
    tool_name: &str,
    max_output_bytes: usize,
) -> Arc<dyn crabgent_core::Tool> {
    one_tool_with_token(ctx, server_name, tool_name, &ctx.token, max_output_bytes)
}

fn one_tool_with_token(
    ctx: &common::McpTestCtx,
    server_name: &str,
    tool_name: &str,
    token: &str,
    max_output_bytes: usize,
) -> Arc<dyn crabgent_core::Tool> {
    let client = Arc::new(client_with_token(ctx, token));
    let factory = crabgent_mcp_client::McpToolFactory::from_client(
        server_name,
        defs(&[tool_name]),
        &client,
        max_output_bytes,
    )
    .expect("factory should build");
    factory.into_tools().pop().expect("one tool")
}

fn client(ctx: &common::McpTestCtx) -> McpClient {
    client_with_token(ctx, &ctx.token)
}

fn client_with_token(ctx: &common::McpTestCtx, token: &str) -> McpClient {
    let config = McpServerConfig::new(&ctx.server_name, &ctx.base_url)
        .expect("valid test config")
        .with_token(SecretString::from(token.to_string()));
    let mut builder = McpClientBuilder::new();
    builder
        .add_server(config)
        .expect("test server should be valid");
    builder
        .build()
        .expect("client builds")
        .pop()
        .expect("one client")
        .1
}

fn defs(names: &[&str]) -> Vec<McpToolDef> {
    names
        .iter()
        .map(|name| McpToolDef {
            name: (*name).to_string(),
            description: format!("Run {name}"),
            input_schema: json!({"type": "object"}),
        })
        .collect()
}

fn tool_ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("test-subject"))
}

fn reset_mock_server(ctx: &common::McpTestCtx) -> Option<&httpmock::MockServer> {
    let server = ctx.mock_server()?;
    server.reset();
    mount_initialize(server);
    Some(server)
}

fn mount_call(server: &httpmock::MockServer, text: &str) {
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("tools/call");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(call_result(text));
    });
}

fn call_result(text: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "content": [{"type": "text", "text": text}],
            "isError": false
        }
    })
}
