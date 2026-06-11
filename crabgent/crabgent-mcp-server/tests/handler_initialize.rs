mod common;

use common::{TEST_TOKEN, auth_headers, body, handler};
use crabgent_mcp_server::{MCP_SESSION_ID_HEADER, MCP_VERSION_HEADER, McpSessionId};

#[tokio::test]
async fn initialize_issues_session() {
    let handler = handler();
    let response = handler
        .dispatch(
            &auth_headers(),
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        )
        .await;
    let value = body(&response);
    let session_header = response
        .headers
        .get(MCP_SESSION_ID_HEADER)
        .expect("session id response header");
    let session_id = McpSessionId::parse(session_header).expect("session id is valid UUID");

    assert_eq!(response.status_code, 200);
    assert_eq!(session_id.as_uuid().get_version_num(), 7);
    assert_eq!(value["result"]["sessionId"], session_header.as_str());
    assert_eq!(value["result"]["protocolVersion"], "2025-03-26");
}

#[tokio::test]
async fn initialize_rejects_existing_session_header() {
    let handler = handler();
    let mut headers = auth_headers();
    headers.insert(
        MCP_SESSION_ID_HEADER.to_owned(),
        McpSessionId::new().to_string(),
    );

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 400);
    assert_eq!(value["error"]["code"], -32600);
}

#[tokio::test]
async fn initialize_validates_protocol_version() {
    let handler = handler();
    let mut headers = auth_headers();
    headers.insert(MCP_VERSION_HEADER.to_owned(), "2024-11-05".to_owned());

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 400);
    assert_eq!(value["error"]["code"], -32600);
}

#[tokio::test]
async fn initialize_no_bearer_returns_401_no_body_leak() {
    let handler = handler();
    let headers = crabgent_mcp_server::HeaderMap::new();

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
        )
        .await;

    assert_eq!(response.status_code, 401);
    assert!(response.body.is_empty());
    assert!(!format!("{response:?}").contains(TEST_TOKEN));
}
