mod common;

use std::time::Duration;

use crabgent_mcp_client::{McpClient, McpClientBuilder, McpError, McpServerConfig};
use httpmock::Method::POST;
use secrecy::SecretString;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use common::{TEST_SESSION_ID, mcp_test_ctx, mount_initialize};

fn client(ctx: &common::McpTestCtx) -> McpClient {
    let config = McpServerConfig::new(&ctx.server_name, &ctx.base_url)
        .expect("valid test config")
        .with_token(SecretString::from(ctx.token.clone()));
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

#[tokio::test]
async fn client_json_rpc_authentication_required_returns_authfailed() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    server.reset();
    mount_initialize(server);
    // crabgent-mcp-server returns a JSON-RPC error (HTTP 200) with this message
    // for unauthenticated requests; it must classify as AuthFailed, not a
    // retriable JsonRpc error.
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("tools/call");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32603, "message": "authentication required"}
            }));
    });
    let client = client(&ctx);

    let err = client
        .call_tool("search_docs", json!({}), None)
        .await
        .expect_err("authentication required should fail");

    assert!(matches!(err, McpError::AuthFailed));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_concurrent_first_calls_share_one_session() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    server.reset();

    // A slow initialize widens the window in which several concurrent
    // first-calls observe an empty session. ensure_session must not hold the
    // session Mutex across this round-trip (that would serialize unrelated
    // callers for the full network latency); the refactored path checks/copies
    // under the lock, releases it, performs the I/O, then re-acquires to store.
    // This test pins the concurrent path: every caller completes, exactly one
    // wins the store race, and there is no deadlock.
    //
    // Note: httpmock 0.8 serializes a blocking `respond_with` on its async
    // worker, so the harness cannot directly measure true I/O overlap. This
    // test guards the concurrent code path against deadlock and store races
    // rather than asserting wall-clock overlap.
    let initialize = server.mock(|when, then| {
        when.method(POST).path("/").body_includes("initialize");
        then.status(200)
            .header("content-type", "application/json")
            .header("Mcp-Session-Id", TEST_SESSION_ID)
            .delay(std::time::Duration::from_millis(120))
            .json_body(json!({
                "jsonrpc": "2.0",
                "id": 0,
                "result": {"protocolVersion": "2025-03-26", "capabilities": {}}
            }));
    });
    let tool_call = server.mock(|when, then| {
        when.method(POST)
            .path("/")
            .body_includes("tools/call")
            .header("Mcp-Session-Id", TEST_SESSION_ID);
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{"type": "text", "text": "ok"}],
                    "isError": false
                }
            }));
    });

    let client = client(&ctx);
    let handles = (0..4)
        .map(|_| {
            let client = client.clone();
            tokio::spawn(async move { client.call_tool("search_docs", json!({}), None).await })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        let result = handle
            .await
            .expect("concurrent task joins without panic")
            .expect("concurrent tool call succeeds");
        assert_eq!(result.content, json!("ok"));
    }

    // All four tool calls carried the session header negotiated by initialize.
    assert_eq!(tool_call.calls(), 4);
    // The store race resolves to a single shared session: at most a handful of
    // initialize round-trips, never one per call indefinitely (no deadlock).
    let initialize_calls = initialize.calls();
    assert!(
        (1..=4).contains(&initialize_calls),
        "initialize fired {initialize_calls} times"
    );
}

// SSRF guard: a 3xx from the MCP server must never be auto-followed. With
// `redirect::Policy::none()` reqwest surfaces the 302 as a normal response, so
// the client reports an HTTP error and the redirect target is never contacted.
// Without the guard reqwest would follow the Location to an internal address
// (cloud metadata, link-local) and pipe that body back into the kernel/LLM.
#[tokio::test]
async fn client_does_not_follow_http_redirect() {
    let ctx = mcp_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    server.reset();
    mount_initialize(server);

    // The forbidden internal target. It must register zero hits.
    let redirect_target = server.mock(|when, then| {
        when.method(POST).path("/redirect-target");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "content": [{"type": "text", "text": "internal-secret"}],
                    "isError": false
                }
            }));
    });
    server.mock(|when, then| {
        when.method(POST).path("/").body_includes("tools/call");
        then.status(302)
            .header("location", server.url("/redirect-target"));
    });
    let client = client(&ctx);

    let err = client
        .call_tool("search_docs", json!({}), None)
        .await
        .expect_err("3xx must not be followed and must surface as an error");

    // The client saw the 3xx itself, not the redirected internal body.
    assert!(
        matches!(&err, McpError::ToolCall(message) if message.contains("302")),
        "expected an HTTP 302 error, got {err:?}"
    );
    let rendered = err.to_string();
    assert!(!rendered.contains("internal-secret"));
    // The redirect target was never contacted.
    assert_eq!(redirect_target.calls(), 0);
}

// Slow-loris guard: a server that sends the response head, then trickles the
// body slower than the per-read idle deadline must trip `read_timeout` instead
// of pinning the worker forever. httpmock cannot model inter-chunk gaps (its
// delay precedes the whole response), so this uses a raw TCP server that writes
// the head immediately and then stalls past a short configured idle deadline.
#[tokio::test]
async fn client_slow_body_trips_read_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let addr = listener.local_addr().expect("listener address");

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept connection");
        // Drain the request so the client finishes sending before we stall.
        let mut buf = [0_u8; 1024];
        let read = socket.read(&mut buf).await.expect("read request bytes");
        assert!(read > 0, "client should have sent the request");
        // Send the head announcing a body, then stall longer than the idle
        // deadline without ever writing the promised body bytes.
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 64\r\n\r\n",
            )
            .await
            .expect("write response head");
        socket.flush().await.expect("flush head");
        tokio::time::sleep(Duration::from_secs(2)).await;
    });

    let config = McpServerConfig::new("slow_server", format!("http://{addr}/"))
        .expect("valid test config")
        .with_read_idle_timeout(Duration::from_millis(150));
    let mut builder = McpClientBuilder::new();
    builder
        .add_server(config)
        .expect("test server should be valid");
    let client = builder
        .build()
        .expect("client builds")
        .pop()
        .expect("one client")
        .1;

    let err = client
        .discover()
        .await
        .expect_err("a stalled body must trip the read-idle timeout");

    assert!(
        matches!(&err, McpError::Http(inner) if inner.is_timeout()),
        "expected a timeout error, got {err:?}"
    );

    server.abort();
}
