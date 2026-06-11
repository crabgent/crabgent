mod common;

use common::{auth_headers, body, handler, initialize};

#[tokio::test]
async fn invalid_method_returns_minus32601() {
    let handler = handler();
    let (headers, _init) = initialize(&handler).await;

    let response = handler
        .dispatch(
            &headers,
            br#"{"jsonrpc":"2.0","id":9,"method":"prompts/list","params":{}}"#,
        )
        .await;
    let value = body(&response);

    assert_eq!(response.status_code, 200);
    assert_eq!(value["error"]["code"], -32601);
    assert_eq!(
        value["error"]["message"]
            .as_str()
            .expect("error message is a string"),
        "method not found: prompts/list"
    );
}

#[tokio::test]
async fn notifications_initialized_accepted_without_body() {
    let handler = handler();

    let response = handler
        .dispatch(
            &auth_headers(),
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;

    assert_eq!(response.status_code, 202);
    assert!(response.body.is_empty());
}

#[tokio::test]
async fn notification_requires_bearer() {
    let handler = handler();

    let response = handler
        .dispatch(
            &crabgent_mcp_server::HeaderMap::new(),
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        )
        .await;

    assert_eq!(response.status_code, 401);
    assert!(response.body.is_empty());
}
