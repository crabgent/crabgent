mod support;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_core::{
    Action, GlobalModelOverrideStore, PolicyDecision, PolicyHook, Subject, Tool, ToolError,
};
use serde_json::json;
use support::*;

#[derive(Default)]
struct CountingPolicy {
    seen: Mutex<usize>,
}

#[async_trait]
impl PolicyHook for CountingPolicy {
    async fn allow(&self, _subject: &Subject, _action: &Action) -> PolicyDecision {
        *self.seen.lock().expect("mutex should not be poisoned") += 1;
        PolicyDecision::Allow
    }
}

#[tokio::test]
async fn unknown_override_model_is_rejected_before_policy() {
    let policy = Arc::new(CountingPolicy::default());
    let (tool, _, _) = tool_with_state(policy.clone());

    let err = execute(&tool, json!({"op": "set_global", "model": "missing"}))
        .await
        .expect_err("invalid model");

    assert!(
        matches!(err, ToolError::InvalidArgs(message) if message.contains("unknown model override"))
    );
    assert_eq!(
        *policy.seen.lock().expect("mutex should not be poisoned"),
        0
    );
}

#[tokio::test]
async fn policy_deny_set_global_returns_permission() {
    let (tool, _, global_store) = tool_with_state(deny_policy());

    let err = execute(&tool, json!({"op": "set_global", "model": "gpt-5.5"}))
        .await
        .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
    assert!(
        global_store
            .get_global_model_override()
            .await
            .expect("test result")
            .is_none()
    );
}

#[tokio::test]
async fn get_missing_id_returns_invalid_args() {
    let err = execute(&tool(allow_policy()), json!({"op": "get"}))
        .await
        .expect_err("invalid args");

    assert!(
        matches!(err, ToolError::InvalidArgs(message) if message.contains("missing required field 'id'"))
    );
}

#[tokio::test]
async fn policy_deny_list_returns_permission() {
    let err = execute(&tool(deny_policy()), json!({"op": "list"}))
        .await
        .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn policy_deny_get_returns_permission() {
    let err = execute(
        &tool(deny_policy()),
        json!({"op": "get", "id": "claude-haiku-4-5"}),
    )
    .await
    .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
}

/// A syntactically valid session id (UUID). The deny short-circuits before any
/// session load, so the id only has to parse.
const SESSION_ID: &str = "0192a000-0000-7000-8000-000000000000";

#[tokio::test]
async fn policy_deny_current_returns_permission() {
    let err = execute(&tool(deny_policy()), json!({"op": "current"}))
        .await
        .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn policy_deny_set_session_returns_permission() {
    let err = execute(
        &tool(deny_policy()),
        json!({"op": "set_session", "session_id": SESSION_ID, "model": "gpt-5.5"}),
    )
    .await
    .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn policy_deny_clear_session_returns_permission() {
    let err = execute(
        &tool(deny_policy()),
        json!({"op": "clear_session", "session_id": SESSION_ID}),
    )
    .await
    .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn policy_deny_set_session_effort_returns_permission() {
    let err = execute(
        &tool(deny_policy()),
        json!({"op": "set_session_effort", "session_id": SESSION_ID, "reasoning_effort": "high"}),
    )
    .await
    .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn policy_deny_clear_session_effort_returns_permission() {
    let err = execute(
        &tool(deny_policy()),
        json!({"op": "clear_session_effort", "session_id": SESSION_ID}),
    )
    .await
    .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn policy_deny_clear_global_returns_permission() {
    let err = execute(&tool(deny_policy()), json!({"op": "clear_global"}))
        .await
        .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn policy_deny_set_global_effort_returns_permission() {
    let err = execute(
        &tool(deny_policy()),
        json!({"op": "set_global_effort", "reasoning_effort": "low"}),
    )
    .await
    .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn policy_deny_clear_global_effort_returns_permission() {
    let err = execute(&tool(deny_policy()), json!({"op": "clear_global_effort"}))
        .await
        .expect_err("permission denied");

    assert!(matches!(err, ToolError::Permission(_)));
}

#[test]
fn description_mentions_list_get_and_cap() {
    let tool = tool(allow_policy());
    let description = tool.description();

    assert!(description.contains("list"));
    assert!(description.contains("get"));
    assert!(description.contains("current"));
    assert!(description.contains("set_session"));
    assert!(description.contains("set_global"));
    assert!(description.contains("200"));
}

#[test]
fn tool_name_is_models() {
    assert_eq!(tool(allow_policy()).name(), "models");
}
