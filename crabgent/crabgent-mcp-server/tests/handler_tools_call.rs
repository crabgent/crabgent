mod common;

use common::{TEST_TOKEN, body, handler, handler_with_filter, handler_with_kernel, initialize};

#[tokio::test]
async fn tools_call_dispatches_kernel_tool() {
    let handler = handler();
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"mock_echo","arguments":{"text":"hi"}}}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 200);
    assert_eq!(value["result"]["content"][0]["text"], "tool:hi");
    assert_eq!(value["result"]["isError"], false);
}

#[tokio::test]
async fn tools_call_filter_denied_returns_method_not_found() {
    let handler = handler_with_filter(|name| name == "chat");
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"mock_echo","arguments":{"text":"hi"}}}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 200);
    assert_eq!(value["error"]["code"], -32601);
}

#[tokio::test]
async fn tools_call_chat_blocked_by_filter_returns_method_not_found() {
    let handler = handler_with_filter(|name| name != "chat");
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"chat","arguments":{"message":"hi"}}}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 200);
    assert_eq!(value["error"]["code"], -32601);
}

#[tokio::test]
async fn tools_call_unknown_tool_returns_method_not_found() {
    let handler = handler();
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"missing","arguments":{}}}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 200);
    assert_eq!(value["error"]["code"], -32601);
}

#[tokio::test]
async fn tools_call_filter_denied_does_not_echo_tool_name() {
    let handler = handler_with_filter(|_| false);
    let (headers, _init) = initialize(&handler).await;
    let request_body = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"{TEST_TOKEN}","arguments":{{}}}}}}"#
    );

    let response = handler.dispatch(&headers, request_body.as_bytes()).await;
    let value = body(&response);
    let body_text = String::from_utf8(response.body.to_vec()).expect("JSON body is UTF-8");

    assert_eq!(response.status_code, 200);
    assert_eq!(value["error"]["code"], -32601);
    assert!(!body_text.contains(TEST_TOKEN));
}

#[tokio::test]
async fn tools_call_policy_denied_returns_soft_error_without_reason() {
    let handler = handler_with_kernel(common::mock_kernel::build_denied_test_kernel(), |_| true);
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"mock_echo","arguments":{"text":"hi"}}}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 200);
    assert_eq!(value["result"]["content"][0]["text"], "policy denied");
    assert_eq!(value["result"]["isError"], true);
}
