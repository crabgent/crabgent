mod common;

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crabgent_core::{ReasoningEffort, Tool};
use crabgent_store::{TaskId, TaskStatus};
use crabgent_task::TaskExecutor;
use serde_json::json;

use common::{
    HangingProvider, ImmediateProvider, assert_permission, build_harness,
    build_harness_with_named_tools, build_immediate_harness, create_args, ctx,
    ctx_with_current_model, ctx_with_current_model_and_effort, ctx_with_parent, insert_task,
    load_task, test_task,
};

#[tokio::test]
async fn create_inserts_task_record_and_returns_id() {
    let h = build_immediate_harness();

    let out = h
        .tool
        .execute(create_args("summarize"), &ctx())
        .await
        .expect("create succeeds");

    assert_eq!(out["status"], "running");
    let id: TaskId = out["task_id"]
        .as_str()
        .expect("task id")
        .parse()
        .expect("value should parse");
    let task = load_task(&h.store, &id).await;
    assert_eq!(task.prompt, "summarize");
    assert_eq!(task.owner.as_str(), "alice");
    assert_eq!(task.status, TaskStatus::Running);
}

#[tokio::test]
async fn create_with_block_returns_final_output() {
    let h = build_immediate_harness();
    let mut args = create_args("finish");
    args["block"] = json!(true);

    let out = h.tool.execute(args, &ctx()).await.expect("create succeeds");

    assert_eq!(out["status"], "done");
    assert_eq!(out["output"], "done");
    assert!(out["error"].is_null());
}

#[tokio::test]
async fn create_without_model_uses_current_model_context() {
    let h = build_immediate_harness();
    let args = json!({
        "op": "create",
        "prompt": "inherit current model",
        "block": true
    });

    let out = h
        .tool
        .execute(
            args,
            &ctx_with_current_model(crabgent_core::ResolvedSource::ConfigDefault),
        )
        .await
        .expect("create succeeds");

    assert_eq!(out["status"], "done");
    assert_eq!(out["output"], "done");
}

#[tokio::test]
async fn create_without_reasoning_effort_snapshots_current_effort() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h = build_harness(
        ImmediateProvider::recording("done", Arc::clone(&seen)),
        TaskExecutor::new,
        Arc::new(crabgent_core::AllowAllPolicy),
    );
    let args = json!({
        "op": "create",
        "prompt": "inherit current effort",
        "block": true
    });

    let out = h
        .tool
        .execute(
            args,
            &ctx_with_current_model_and_effort(
                crabgent_core::ResolvedSource::ConfigDefault,
                ReasoningEffort::High,
            ),
        )
        .await
        .expect("create succeeds");

    let id: TaskId = out["task_id"]
        .as_str()
        .expect("task id")
        .parse()
        .expect("value should parse");
    let task = load_task(&h.store, &id).await;
    assert_eq!(task.reasoning_effort_override, Some(ReasoningEffort::High));
    let captured = seen.lock().expect("seen lock");
    assert_eq!(captured[0].reasoning_effort, Some(ReasoningEffort::High));
}

#[tokio::test]
async fn create_with_unsupported_model_does_not_inherit_current_effort() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h = build_harness(
        ImmediateProvider::recording_without_reasoning("done", Arc::clone(&seen)),
        TaskExecutor::new,
        Arc::new(crabgent_core::AllowAllPolicy),
    );
    let mut args = create_args("do not inherit unsupported effort");
    args["block"] = json!(true);

    let out = h
        .tool
        .execute(
            args,
            &ctx_with_current_model_and_effort(
                crabgent_core::ResolvedSource::SessionOverride,
                ReasoningEffort::High,
            ),
        )
        .await
        .expect("create succeeds");

    let id: TaskId = out["task_id"]
        .as_str()
        .expect("task id")
        .parse()
        .expect("value should parse");
    let task = load_task(&h.store, &id).await;
    assert_eq!(task.reasoning_effort_override, None);
    let captured = seen.lock().expect("seen lock");
    assert_eq!(captured[0].reasoning_effort, None);
}

#[tokio::test]
async fn create_with_null_reasoning_effort_clears_for_unsupported_model() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h = build_harness(
        ImmediateProvider::recording_without_reasoning("done", Arc::clone(&seen)),
        TaskExecutor::new,
        Arc::new(crabgent_core::AllowAllPolicy),
    );
    let mut args = create_args("clear unsupported effort");
    args["block"] = json!(true);
    args["reasoning_effort"] = json!(null);

    h.tool
        .execute(
            args,
            &ctx_with_current_model_and_effort(
                crabgent_core::ResolvedSource::SessionOverride,
                ReasoningEffort::High,
            ),
        )
        .await
        .expect("create succeeds");

    let captured = seen.lock().expect("seen lock");
    assert_eq!(captured[0].reasoning_effort, None);
}

#[tokio::test]
async fn create_with_none_reasoning_effort_clears_for_unsupported_model() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h = build_harness(
        ImmediateProvider::recording_without_reasoning("done", Arc::clone(&seen)),
        TaskExecutor::new,
        Arc::new(crabgent_core::AllowAllPolicy),
    );
    let mut args = create_args("clear disabled unsupported effort");
    args["block"] = json!(true);
    args["reasoning_effort"] = json!("none");

    h.tool
        .execute(
            args,
            &ctx_with_current_model_and_effort(
                crabgent_core::ResolvedSource::SessionOverride,
                ReasoningEffort::High,
            ),
        )
        .await
        .expect("create succeeds");

    let captured = seen.lock().expect("seen lock");
    assert_eq!(captured[0].reasoning_effort, None);
}

#[tokio::test]
async fn create_with_tool_access_none_advertises_no_tools() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h = build_harness_with_named_tools(
        ImmediateProvider::recording("done", Arc::clone(&seen)),
        TaskExecutor::new,
        Arc::new(crabgent_core::AllowAllPolicy),
        &["task", "memory"],
    );
    let mut args = create_args("no tools");
    args["block"] = json!(true);
    args["tool_access"] = json!({"mode": "none"});

    h.tool.execute(args, &ctx()).await.expect("create succeeds");

    let captured = seen.lock().expect("seen lock");
    assert!(captured[0].tools.is_empty());
}

#[tokio::test]
async fn create_with_tool_access_only_advertises_allowed_tools() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h = build_harness_with_named_tools(
        ImmediateProvider::recording("done", Arc::clone(&seen)),
        TaskExecutor::new,
        Arc::new(crabgent_core::AllowAllPolicy),
        &["task", "memory"],
    );
    let mut args = create_args("one tool");
    args["block"] = json!(true);
    args["tool_access"] = json!({"mode": "only", "tools": ["task"]});

    h.tool.execute(args, &ctx()).await.expect("create succeeds");

    let captured = seen.lock().expect("seen lock");
    let names: Vec<_> = captured[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    assert_eq!(names, vec!["task"]);
}

#[tokio::test]
async fn create_rejects_unknown_tool_access_name() {
    let h = build_harness_with_named_tools(
        ImmediateProvider::new("done"),
        TaskExecutor::new,
        Arc::new(crabgent_core::AllowAllPolicy),
        &["task"],
    );
    let mut args = create_args("bad tool");
    args["tool_access"] = json!({"mode": "only", "tools": ["missing"]});

    let err = h
        .tool
        .execute(args, &ctx())
        .await
        .expect_err("unknown tool should fail");

    assert!(
        matches!(err, crabgent_core::ToolError::InvalidArgs(message) if message.contains("unknown tool 'missing'"))
    );
}

#[tokio::test]
async fn create_without_model_requires_current_model_context() {
    let h = build_immediate_harness();
    let args = json!({
        "op": "create",
        "prompt": "missing current model"
    });

    let err = h
        .tool
        .execute(args, &ctx())
        .await
        .expect_err("missing current model denied");

    assert!(
        matches!(err, crabgent_core::ToolError::InvalidArgs(message) if message.contains("current model context unavailable"))
    );
}

#[tokio::test]
async fn create_with_block_timeout_returns_failed_with_timeout_message() {
    let (provider, _started) = HangingProvider::new();
    let h = build_harness(
        provider,
        |store| TaskExecutor::new(store).with_shutdown_grace(Duration::from_millis(10)),
        Arc::new(crabgent_core::AllowAllPolicy),
    );
    let mut args = create_args("wait");
    args["block"] = json!(true);
    args["timeout_secs"] = json!(0);

    let out = h.tool.execute(args, &ctx()).await.expect("create succeeds");

    assert_eq!(out["status"], "failed");
    assert_eq!(out["error"], "task timed out");
}

#[tokio::test]
async fn create_rejects_when_parent_chain_depth_at_3() {
    let h = build_immediate_harness();
    let grandparent = insert_task(&h.store, test_task("alice", TaskStatus::Running, None)).await;
    let parent = insert_task(
        &h.store,
        test_task("alice", TaskStatus::Running, Some(grandparent.id.clone())),
    )
    .await;
    let child = insert_task(
        &h.store,
        test_task("alice", TaskStatus::Running, Some(parent.id.clone())),
    )
    .await;

    let err = h
        .tool
        .execute(create_args("too deep"), &ctx_with_parent(&child.id))
        .await
        .expect_err("depth denied");

    assert_permission(err, "nested task depth limit reached");
}

#[tokio::test]
async fn create_rejects_when_max_parallel_exceeded() {
    let h = build_harness(
        ImmediateProvider::new("done"),
        |store| TaskExecutor::new(store).with_max_parallel(0),
        Arc::new(crabgent_core::AllowAllPolicy),
    );

    let err = h
        .tool
        .execute(create_args("over cap"), &ctx())
        .await
        .expect_err("parallel denied");

    assert_permission(err, "max parallel tasks reached");
}
