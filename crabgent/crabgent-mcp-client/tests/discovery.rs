mod common;

use std::sync::{Arc, Mutex};

use crabgent_log::field::{Field, Visit};
use crabgent_log::{Event as TracingEvent, Subscriber};
use crabgent_mcp_client::{McpServerConfig, discover_servers};
use httpmock::Method::POST;
use secrecy::SecretString;
use serde_json::{Value, json};

use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{Layer, Registry};

use common::{mcp_test_ctx, mount_initialize};

#[tokio::test]
async fn discovery_all_healthy_returns_all_factories() {
    let ctx_a = mcp_test_ctx().await;
    let ctx_b = mcp_test_ctx().await;
    let (Some(server_a), Some(server_b)) = (reset_mock_server(&ctx_a), reset_mock_server(&ctx_b))
    else {
        return;
    };
    mount_tools(server_a, &["search_docs"]);
    mount_tools(server_b, &["create_ticket", "read_ticket"]);
    let configs = vec![
        config(
            &format!("{}_a", ctx_a.server_name),
            &ctx_a.base_url,
            &ctx_a.token,
        ),
        config(
            &format!("{}_b", ctx_b.server_name),
            &ctx_b.base_url,
            &ctx_b.token,
        ),
    ];

    let factories = discover_servers(&configs).await;

    assert_eq!(factories.len(), 2);
    assert_eq!(
        factories
            .into_iter()
            .flat_map(crabgent_mcp_client::McpToolFactory::into_tools)
            .count(),
        3
    );
}

#[tokio::test]
async fn discovery_soft_fail_one_server_down() {
    let ctx_a = mcp_test_ctx().await;
    let ctx_b = mcp_test_ctx().await;
    let (Some(server_a), Some(server_b)) = (reset_mock_server(&ctx_a), reset_mock_server(&ctx_b))
    else {
        return;
    };
    mount_tools(server_a, &["search_docs"]);
    mount_status(server_b, 500, "upstream failed");
    let configs = vec![
        config(
            &format!("{}_healthy", ctx_a.server_name),
            &ctx_a.base_url,
            &ctx_a.token,
        ),
        config(
            &format!("{}_broken", ctx_b.server_name),
            &ctx_b.base_url,
            &ctx_b.token,
        ),
    ];

    let logs = CapturedLogs::default();
    let _guard = capture_logs(&logs);

    let factories = discover_servers(&configs).await;
    let logs = logs.joined();

    assert_eq!(factories.len(), 1);
    assert!(logs.contains("MCP server discovery failed - skipped"));
    assert!(logs.contains("broken"));
}

#[tokio::test]
async fn discovery_all_down_returns_empty() {
    let ctx_a = mcp_test_ctx().await;
    let ctx_b = mcp_test_ctx().await;
    let (Some(server_a), Some(server_b)) = (reset_mock_server(&ctx_a), reset_mock_server(&ctx_b))
    else {
        return;
    };
    mount_status(server_a, 500, "server a down");
    mount_status(server_b, 500, "server b down");
    let configs = vec![
        config(
            &format!("{}_a", ctx_a.server_name),
            &ctx_a.base_url,
            &ctx_a.token,
        ),
        config(
            &format!("{}_b", ctx_b.server_name),
            &ctx_b.base_url,
            &ctx_b.token,
        ),
    ];

    let factories = discover_servers(&configs).await;

    assert!(factories.is_empty());
}

#[tokio::test]
async fn discovery_auth_failure_no_token_in_warn_log() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = reset_mock_server(&ctx) else {
        return;
    };
    mount_status(server, 401, "secret-token-987 rejected");
    let configs = vec![config(
        &format!("{}_auth", ctx.server_name),
        &ctx.base_url,
        "secret-token-987",
    )];

    let logs = CapturedLogs::default();
    let _guard = capture_logs(&logs);

    let factories = discover_servers(&configs).await;
    let logs = logs.joined();

    assert!(factories.is_empty());
    assert!(logs.contains("MCP server discovery failed - skipped"));
    assert!(logs.contains("auth"));
    assert!(!logs.contains("secret-token-987"));
}

#[tokio::test]
async fn discovery_duplicate_server_alias_soft_fails_second_config() {
    let ctx_a = mcp_test_ctx().await;
    let ctx_b = mcp_test_ctx().await;
    let (Some(server_a), Some(server_b)) = (reset_mock_server(&ctx_a), reset_mock_server(&ctx_b))
    else {
        return;
    };
    mount_tools(server_a, &["search_docs"]);
    mount_tools(server_b, &["search_docs"]);
    let configs = vec![
        config("duplicate", &ctx_a.base_url, &ctx_a.token),
        config("duplicate", &ctx_b.base_url, &ctx_b.token),
    ];

    let logs = CapturedLogs::default();
    let _guard = capture_logs(&logs);

    let factories = discover_servers(&configs).await;
    let tools = factories
        .into_iter()
        .flat_map(crabgent_mcp_client::McpToolFactory::into_tools)
        .collect::<Vec<_>>();
    let logs = logs.joined();

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "duplicate__search_docs");
    assert!(logs.contains("MCP server discovery failed - skipped"));
    assert!(logs.contains("duplicate"));
}

fn reset_mock_server(ctx: &common::McpTestCtx) -> Option<&httpmock::MockServer> {
    let server = ctx.mock_server()?;
    server.reset();
    mount_initialize(server);
    Some(server)
}

fn config(name: &str, base_url: &str, token: &str) -> McpServerConfig {
    McpServerConfig::new(name, base_url)
        .expect("valid test config")
        .with_token(SecretString::from(token.to_string()))
}

fn mount_tools(server: &httpmock::MockServer, names: &[&str]) {
    let tools = names
        .iter()
        .map(|name| {
            json!({
                "name": name,
                "description": format!("Run {name}"),
                "inputSchema": {"type": "object"}
            })
        })
        .collect::<Vec<_>>();

    server.mock(move |when, then| {
        when.method(POST).path("/").body_includes("tools/list");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json_rpc_result(&json!({ "tools": tools })));
    });
}

fn mount_status(server: &httpmock::MockServer, status: u16, body: &str) {
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("tools/list");
        then.status(status)
            .header("content-type", "application/json")
            .body(body);
    });
}

fn json_rpc_result(result: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": result
    })
}

#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<String>>>);

impl CapturedLogs {
    fn joined(&self) -> String {
        self.0.lock().expect("capture lock poisoned").join("\n")
    }

    fn push(&self, text: String) {
        self.0.lock().expect("capture lock poisoned").push(text);
    }
}

struct CaptureLayer {
    logs: CapturedLogs,
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &TracingEvent<'_>, _ctx: Context<'_, S>) {
        let mut visitor = LogVisitor::default();
        event.record(&mut visitor);
        self.logs.push(visitor.fields.join(" "));
    }
}

#[derive(Default)]
struct LogVisitor {
    fields: Vec<String>,
}

impl Visit for LogVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.fields.push(format!("{}={value:?}", field.name()));
    }
}

fn capture_logs(logs: &CapturedLogs) -> crabgent_log::subscriber::DefaultGuard {
    let subscriber = Registry::default().with(CaptureLayer { logs: logs.clone() });
    crabgent_log::subscriber::set_default(subscriber)
}
