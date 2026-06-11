mod common;

use crabgent_channel_slack::tools::SlackSearchTool;
use crabgent_core::tool::Tool;
use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, ResponseTemplate};

use common::{allow_policy, deny_policy, slack_client, slack_test_ctx, tool_ctx};

#[tokio::test]
async fn search_happy_with_thread_ts() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_search(server, true, "a".repeat(400)).await;
    let tool = SlackSearchTool::new(slack_client(&ctx), allow_policy());

    let result = tool
        .execute_result(json!({"query": "hello", "thread_ts": "1.1"}), &tool_ctx())
        .await
        .expect("tool result");

    assert!(!result.is_error);
    assert_eq!(result.output["thread_ts"], "1.1");
    assert_eq!(
        result.output["matches"][0]["text"]
            .as_str()
            .expect("value should be a string")
            .len(),
        300
    );
}

#[tokio::test]
async fn search_happy_without_thread_ts() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_search(server, true, "short".to_owned()).await;
    let tool = SlackSearchTool::new(slack_client(&ctx), allow_policy());

    let result = tool
        .execute_result(json!({"query": "hello"}), &tool_ctx())
        .await
        .expect("tool result");

    assert!(!result.is_error);
    assert_eq!(result.output["matches"][0]["text"], "short");
}

#[tokio::test]
async fn search_api_error_is_soft_error() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    mount_search(server, false, String::new()).await;
    let tool = SlackSearchTool::new(slack_client(&ctx), allow_policy());

    let result = tool
        .execute_result(json!({"query": "hello"}), &tool_ctx())
        .await
        .expect("tool result");

    assert!(result.is_error);
}

#[tokio::test]
async fn search_policy_deny_is_soft_error() {
    let ctx = slack_test_ctx().await;
    let tool = SlackSearchTool::new(slack_client(&ctx), deny_policy());

    let result = tool
        .execute_result(json!({"query": "hello"}), &tool_ctx())
        .await
        .expect("tool result");

    assert!(result.is_error);
    assert!(
        result
            .output
            .as_str()
            .unwrap_or_default()
            .contains("DenyAllPolicy")
    );
}

#[tokio::test]
async fn search_count_is_clamped_to_20() {
    let ctx = slack_test_ctx().await;
    let Some(server) = ctx.mock_server() else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/search.messages"))
        .and(body_string_contains("count=20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true, "messages": {"matches": []}
        })))
        .mount(server)
        .await;
    let tool = SlackSearchTool::new(slack_client(&ctx), allow_policy());

    let result = tool
        .execute_result(json!({"query": "hello", "count": 999}), &tool_ctx())
        .await
        .expect("tool result");

    assert!(!result.is_error);
    assert_eq!(
        result.output["matches"].as_array().expect("matches").len(),
        0
    );
}

async fn mount_search(server: &wiremock::MockServer, ok: bool, text: String) {
    let body = if ok {
        json!({"ok": true, "messages": {"matches": [{"text": text, "username": "ada", "ts": "1.1"}]}})
    } else {
        json!({"ok": false, "error": "search_failed"})
    };
    Mock::given(method("POST"))
        .and(path("/search.messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}
