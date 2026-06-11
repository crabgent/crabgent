mod support;

use std::collections::BTreeSet;
use std::sync::Arc;

use crabgent_core::{
    EffortSource, GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId,
    ReasoningEffort, ResolvedEffort, ResolvedSource, StrictPolicy, Subject, ToolCtx, ToolError,
};
use crabgent_store::SessionStore;
use serde_json::json;
use support::*;

#[tokio::test]
async fn list_returns_all_models() {
    let result = execute(&tool(allow_policy()), json!({"op": "list"}))
        .await
        .expect("list");
    let ids: BTreeSet<_> = ids_from(&result).into_iter().collect();

    assert_eq!(
        ids,
        BTreeSet::from([
            "claude-haiku-4-5".to_owned(),
            "claude-sonnet-4-6".to_owned(),
            "gpt-5.5".to_owned(),
        ])
    );
    assert_eq!(result["count"], 3);
    assert_eq!(result["total"], 3);
    assert_eq!(result["truncated"], false);
}

#[tokio::test]
async fn list_filters_by_provider() {
    let result = execute(
        &tool(allow_policy()),
        json!({"op": "list", "provider": "anthropic"}),
    )
    .await
    .expect("list");
    let models = result["models"].as_array().expect("models array");

    assert_eq!(result["count"], 2);
    assert!(models.iter().all(|model| model["provider"] == "anthropic"));
}

#[tokio::test]
async fn list_provider_no_match_returns_empty() {
    let result = execute(
        &tool(allow_policy()),
        json!({"op": "list", "provider": "unknown"}),
    )
    .await
    .expect("list");

    assert_eq!(result["count"], 0);
    assert_eq!(result["total"], 0);
    assert_eq!(result["truncated"], false);
    assert_eq!(result["models"], json!([]));
}

#[tokio::test]
async fn list_sorts_by_id_deterministic() {
    let tool = tool(allow_policy());
    let first = execute(&tool, json!({"op": "list"})).await.expect("list");
    let second = execute(&tool, json!({"op": "list"})).await.expect("list");
    let first_ids = ids_from(&first);
    let second_ids = ids_from(&second);

    assert_eq!(first_ids, second_ids);
    assert_eq!(
        first_ids,
        vec![
            "claude-haiku-4-5".to_owned(),
            "claude-sonnet-4-6".to_owned(),
            "gpt-5.5".to_owned(),
        ]
    );
}

#[tokio::test]
async fn list_truncates_at_cap() {
    let tool = tool_for_kernel(bulk_kernel(250), allow_policy());
    let result = execute(&tool, json!({"op": "list"})).await.expect("list");

    assert_eq!(result["count"], 200);
    assert_eq!(result["total"], 250);
    assert_eq!(result["truncated"], true);
    assert_eq!(
        result["models"].as_array().expect("models array").len(),
        200
    );
}

#[tokio::test]
async fn get_returns_model_by_id() {
    let result = execute(
        &tool(allow_policy()),
        json!({"op": "get", "id": "claude-haiku-4-5"}),
    )
    .await
    .expect("get");

    assert_eq!(result["model"]["id"], "claude-haiku-4-5");
}

#[tokio::test]
async fn get_returns_canonical_model_by_alias() {
    let result = execute(&tool(allow_policy()), json!({"op": "get", "id": "sonnet"}))
        .await
        .expect("get");

    assert_eq!(result["model"]["id"], "claude-sonnet-4-6");
}

#[tokio::test]
async fn get_unknown_id_returns_not_found() {
    let err = execute(&tool(allow_policy()), json!({"op": "get", "id": "foo"}))
        .await
        .expect_err("not found");

    assert!(matches!(err, ToolError::NotFound(message) if message == "model: foo"));
}

#[tokio::test]
async fn current_returns_resolved_model_and_overrides() {
    let (tool, store, global_store) = tool_with_state(allow_policy());
    let session_id = save_session(&store, Some("claude-haiku-4-5")).await;
    let mut session = store
        .load(&session_id)
        .await
        .expect("test result")
        .expect("test result");
    session.reasoning_effort_override = Some(ReasoningEffort::Medium);
    store.save(&session).await.expect("test result");
    global_store
        .set_global_model_override(&ModelId::new("gpt-5.5"))
        .await
        .expect("test result");
    global_store
        .set_global_reasoning_effort_override(ReasoningEffort::High)
        .await
        .expect("test result");
    let ctx = ctx_with_current("claude-haiku-4-5", ResolvedSource::SessionOverride)
        .with_current_effort(ResolvedEffort {
            effort: Some(ReasoningEffort::Medium),
            source: EffortSource::SessionOverride,
        });

    let result = execute_with_ctx(
        &tool,
        json!({"op": "current", "session_id": session_id.to_string()}),
        &ctx,
    )
    .await
    .expect("current");

    assert_eq!(result["model"]["id"], "claude-haiku-4-5");
    assert_eq!(result["source"], "session-override");
    assert_eq!(result["override_session"], "claude-haiku-4-5");
    assert_eq!(result["override_global"], "gpt-5.5");
    assert_eq!(result["reasoning_effort"], "medium");
    assert_eq!(result["reasoning_effort_source"], "session-override");
    assert_eq!(result["override_session_effort"], "medium");
    assert_eq!(result["override_global_effort"], "high");
}

#[tokio::test]
async fn current_uses_ctx_session_for_lookup_but_not_policy_target() {
    let (tool, store, _) = tool_with_state(Arc::new(
        StrictPolicy::builder()
            .allow_models_current()
            .allow_reasoning_effort_current()
            .build(),
    ));
    let session_id = save_session(&store, Some("sonnet")).await;
    let ctx = ctx_with_current("claude-haiku-4-5", ResolvedSource::ConfigDefault)
        .with_session_id(session_id.to_string());

    let result = execute_with_ctx(&tool, json!({"op": "current"}), &ctx)
        .await
        .expect("current session read should be allowed");

    assert_eq!(result["override_session"], "sonnet");
}

#[tokio::test]
async fn current_explicit_session_id_is_policy_targeted() {
    let (tool, store, _) = tool_with_state(Arc::new(
        StrictPolicy::builder()
            .allow_models_current()
            .allow_reasoning_effort_current()
            .build(),
    ));
    let session_id = save_session(&store, Some("sonnet")).await;
    let ctx = ctx_with_current("claude-haiku-4-5", ResolvedSource::ConfigDefault);

    let err = execute_with_ctx(
        &tool,
        json!({"op": "current", "session_id": session_id.to_string()}),
        &ctx,
    )
    .await
    .expect_err("explicit session id should require targeted policy");

    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn set_session_writes_session_override() {
    let (tool, store, _) = tool_with_state(allow_policy());
    let session_id = save_session(&store, None).await;

    let result = execute(
        &tool,
        json!({
            "op": "set_session",
            "session_id": session_id.to_string(),
            "model": "sonnet"
        }),
    )
    .await
    .expect("set_session");

    assert_eq!(result["session_id"], session_id.to_string());
    assert_eq!(result["model"], "sonnet");
    let session = store
        .load(&session_id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(session.model_override.as_deref(), Some("sonnet"));
}

#[tokio::test]
async fn clear_session_removes_session_override() {
    let (tool, store, _) = tool_with_state(allow_policy());
    let session_id = save_session(&store, Some("sonnet")).await;

    let result = execute(
        &tool,
        json!({"op": "clear_session", "session_id": session_id.to_string()}),
    )
    .await
    .expect("clear_session");

    assert_eq!(result["session_id"], session_id.to_string());
    assert!(result["model"].is_null());
    let session = store
        .load(&session_id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(session.model_override, None);
}

#[tokio::test]
async fn set_session_effort_writes_session_override() {
    let (tool, store, _) = tool_with_state(allow_policy());
    let session_id = save_session(&store, None).await;

    let result = execute(
        &tool,
        json!({
            "op": "set_session_effort",
            "session_id": session_id.to_string(),
            "reasoning_effort": "high"
        }),
    )
    .await
    .expect("set_session_effort");

    assert_eq!(result["session_id"], session_id.to_string());
    assert_eq!(result["reasoning_effort"], "high");
    let session = store
        .load(&session_id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(
        session.reasoning_effort_override,
        Some(ReasoningEffort::High)
    );
}

#[tokio::test]
async fn clear_session_effort_removes_session_override() {
    let (tool, store, _) = tool_with_state(allow_policy());
    let session_id = save_session(&store, None).await;
    let mut session = store
        .load(&session_id)
        .await
        .expect("test result")
        .expect("test result");
    session.reasoning_effort_override = Some(ReasoningEffort::Low);
    store.save(&session).await.expect("test result");

    let result = execute(
        &tool,
        json!({"op": "clear_session_effort", "session_id": session_id.to_string()}),
    )
    .await
    .expect("clear_session_effort");

    assert_eq!(result["session_id"], session_id.to_string());
    assert!(result["reasoning_effort"].is_null());
    let session = store
        .load(&session_id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(session.reasoning_effort_override, None);
}

#[tokio::test]
async fn set_session_falls_back_to_ctx_session_id() {
    let (tool, store, _) = tool_with_state(allow_policy());
    let session_id = save_session(&store, None).await;
    let ctx = ToolCtx::new(Subject::new("alice")).with_session_id(session_id.to_string());

    let result = execute_with_ctx(&tool, json!({"op": "set_session", "model": "sonnet"}), &ctx)
        .await
        .expect("set_session via ctx fallback");

    assert_eq!(result["session_id"], session_id.to_string());
    assert_eq!(result["model"], "sonnet");
    let stored = store
        .load(&session_id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(stored.model_override.as_deref(), Some("sonnet"));
}

#[tokio::test]
async fn clear_session_falls_back_to_ctx_session_id() {
    let (tool, store, _) = tool_with_state(allow_policy());
    let session_id = save_session(&store, Some("sonnet")).await;
    let ctx = ToolCtx::new(Subject::new("alice")).with_session_id(session_id.to_string());

    let result = execute_with_ctx(&tool, json!({"op": "clear_session"}), &ctx)
        .await
        .expect("clear_session via ctx fallback");

    assert_eq!(result["session_id"], session_id.to_string());
    assert!(result["model"].is_null());
    let stored = store
        .load(&session_id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(stored.model_override, None);
}

#[tokio::test]
async fn set_session_without_args_or_ctx_returns_clear_error() {
    let tool = tool(allow_policy());

    let err = execute(&tool, json!({"op": "set_session", "model": "sonnet"}))
        .await
        .expect_err("no session id source");

    let message = match err {
        ToolError::InvalidArgs(message) => message,
        other => panic!("expected InvalidArgs, got {other:?}"),
    };
    assert!(message.contains("models.set_session"));
    assert!(message.contains("missing required field 'session_id'"));
    assert!(message.contains("no current session in context"));
}

#[tokio::test]
async fn explicit_session_id_wins_over_ctx_fallback() {
    let (tool, store, _) = tool_with_state(allow_policy());
    let target = save_session_for(&store, "alice", None).await;
    let other = save_session_for(&store, "bob", None).await;
    let ctx = ToolCtx::new(Subject::new("alice")).with_session_id(other.to_string());

    let result = execute_with_ctx(
        &tool,
        json!({
            "op": "set_session",
            "session_id": target.to_string(),
            "model": "sonnet"
        }),
        &ctx,
    )
    .await
    .expect("set_session targets explicit id");

    assert_eq!(result["session_id"], target.to_string());
    let targeted = store
        .load(&target)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(targeted.model_override.as_deref(), Some("sonnet"));
    let untouched = store
        .load(&other)
        .await
        .expect("test result")
        .expect("test result");
    assert!(untouched.model_override.is_none());
}

#[tokio::test]
async fn set_global_writes_global_override() {
    let (tool, _, global_store) = tool_with_state(allow_policy());

    let result = execute(&tool, json!({"op": "set_global", "model": "gpt-5.5"}))
        .await
        .expect("set_global");

    assert_eq!(result["model"], "gpt-5.5");
    assert_eq!(
        global_store
            .get_global_model_override()
            .await
            .expect("test result"),
        Some(ModelId::new("gpt-5.5"))
    );
}

#[tokio::test]
async fn clear_global_removes_global_override() {
    let (tool, _, global_store) = tool_with_state(allow_policy());
    global_store
        .set_global_model_override(&ModelId::new("gpt-5.5"))
        .await
        .expect("test result");

    let result = execute(&tool, json!({"op": "clear_global"}))
        .await
        .expect("clear_global");

    assert!(result["model"].is_null());
    assert!(
        global_store
            .get_global_model_override()
            .await
            .expect("test result")
            .is_none()
    );
}

#[tokio::test]
async fn set_global_effort_writes_global_override() {
    let (tool, _, global_store) = tool_with_state(allow_policy());

    let result = execute(
        &tool,
        json!({"op": "set_global_effort", "reasoning_effort": "medium"}),
    )
    .await
    .expect("set_global_effort");

    assert_eq!(result["reasoning_effort"], "medium");
    assert_eq!(
        global_store
            .get_global_reasoning_effort_override()
            .await
            .expect("test result"),
        Some(ReasoningEffort::Medium)
    );
}

#[tokio::test]
async fn clear_global_effort_removes_global_override() {
    let (tool, _, global_store) = tool_with_state(allow_policy());
    global_store
        .set_global_reasoning_effort_override(ReasoningEffort::High)
        .await
        .expect("test result");

    let result = execute(&tool, json!({"op": "clear_global_effort"}))
        .await
        .expect("clear_global_effort");

    assert!(result["reasoning_effort"].is_null());
    assert!(
        global_store
            .get_global_reasoning_effort_override()
            .await
            .expect("test result")
            .is_none()
    );
}
