#![allow(
    dead_code,
    reason = "shared integration-test helpers are compiled per test binary"
)]

pub mod mock_kernel;

use std::sync::Arc;

use crabgent_core::Kernel;
use crabgent_core::ModelTarget;
use crabgent_mcp_server::{
    AUTHORIZATION_HEADER, HeaderMap, McpHandler, McpResponse, McpServerBuilder, McpServerConfig,
};
use secrecy::SecretString;
use serde_json::Value;

pub const TEST_TOKEN: &str = "secret-test-token-12345";

pub fn auth_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION_HEADER.to_owned(),
        format!("Bearer {TEST_TOKEN}"),
    );
    headers
}

pub fn handler() -> McpHandler {
    handler_with_filter(|_| true)
}

pub fn handler_with_filter(filter: impl Fn(&str) -> bool + Send + Sync + 'static) -> McpHandler {
    handler_with_kernel(mock_kernel::build_test_kernel(), filter)
}

pub fn handler_with_kernel(
    kernel: Arc<Kernel>,
    filter: impl Fn(&str) -> bool + Send + Sync + 'static,
) -> McpHandler {
    let server = McpServerBuilder::new()
        .with_kernel(kernel)
        .with_config(config())
        .with_tool_filter(filter)
        .build()
        .expect("test server has kernel and config");
    McpHandler::new(Arc::new(server))
}

fn config() -> McpServerConfig {
    McpServerConfig::new(
        SecretString::from(TEST_TOKEN),
        ModelTarget::id(mock_kernel::MOCK_MODEL),
    )
}

pub fn body(response: &McpResponse) -> Value {
    serde_json::from_slice(&response.body).expect("response body is JSON")
}

pub async fn initialize(handler: &McpHandler) -> (HeaderMap, Value) {
    let headers = auth_headers();
    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        )
        .await;
    let session_id = response
        .headers
        .get(crabgent_mcp_server::MCP_SESSION_ID_HEADER)
        .expect("initialize returns session header")
        .to_owned();
    let mut next_headers = headers;
    next_headers.insert(
        crabgent_mcp_server::MCP_SESSION_ID_HEADER.to_owned(),
        session_id,
    );

    (next_headers, body(&response))
}
