mod common;

use common::{body, handler, handler_with_kernel, initialize};

#[tokio::test]
async fn chat_returns_reply_text() {
    let handler = handler();
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"chat","arguments":{"message":"hello"}}}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 200);
    assert_eq!(value["result"]["reply_text"], "mock-reply");
    assert_eq!(value["result"]["content"][0]["text"], "mock-reply");
}

#[tokio::test]
async fn chat_uses_session_kernel() {
    let handler = handler();
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"chat","arguments":{"message":"hello"}}}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(value["result"]["reply_text"], "mock-reply");
}

#[tokio::test]
async fn chat_includes_session_id_in_response() {
    let handler = handler();
    let (headers, _init) = initialize(&handler).await;
    let session_id = headers
        .get(crabgent_mcp_server::MCP_SESSION_ID_HEADER)
        .expect("session header exists");

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"chat","arguments":{"message":"hello"}}}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(value["result"]["session_id"], session_id.as_str());
}

#[tokio::test]
async fn tools_call_chat_runs_policy_gate() {
    let handler = handler_with_kernel(common::mock_kernel::build_denied_test_kernel(), |_| true);
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"chat","arguments":{"message":"hello"}}}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 200);
    assert_eq!(value["result"]["content"][0]["text"], "policy denied");
    assert_eq!(value["result"]["isError"], true);
}
