mod common;

use common::{body, handler, handler_with_filter, initialize};
use crabgent_mcp_server::{MCP_SESSION_ID_HEADER, McpSessionId};

#[tokio::test]
async fn tools_list_includes_chat_first() {
    let handler = handler();
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 200);
    assert_eq!(value["result"]["tools"][0]["name"], "chat");
    assert_eq!(value["result"]["tools"][1]["name"], "mock_echo");
}

#[tokio::test]
async fn tools_list_applies_filter() {
    let handler = handler_with_filter(|name| name == "chat");
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(
        value["result"]["tools"]
            .as_array()
            .expect("tools array")
            .len(),
        1
    );
    assert_eq!(value["result"]["tools"][0]["name"], "chat");
}

#[tokio::test]
async fn tools_list_filter_can_hide_chat() {
    let handler = handler_with_filter(|name| name != "chat");
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        )
        .await;
    let value = body(&response);
    let names = value["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();

    assert_eq!(names, vec!["mock_echo"]);
}

#[tokio::test]
async fn tools_list_unknown_session_returns_session_not_found() {
    let handler = handler();
    let mut headers = common::auth_headers();
    headers.insert(
        MCP_SESSION_ID_HEADER.to_owned(),
        McpSessionId::new().to_string(),
    );

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 404);
    assert_eq!(value["error"]["code"], -32001);
}
