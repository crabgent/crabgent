mod common;

use std::sync::Arc;

use crabgent_core::{Tool, ToolError};
use crabgent_store::TaskStatus;
use serde_json::json;

use common::{DenyNamedPolicy, build_harness, create_args, ctx, id_args, insert_task, test_task};

#[tokio::test]
async fn policy_deny_create_returns_tool_error_permission() {
    let h = build_harness(
        common::ImmediateProvider::new("done"),
        crabgent_task::TaskExecutor::new,
        Arc::new(DenyNamedPolicy::new(["task.create"])),
    );

    let err = h
        .tool
        .execute(create_args("denied"), &ctx())
        .await
        .expect_err("create denied");

    assert!(matches!(err, ToolError::Permission(reason) if reason == "denied task.create"));
}

#[tokio::test]
async fn policy_deny_list_returns_tool_error_permission() {
    let h = build_harness(
        common::ImmediateProvider::new("done"),
        crabgent_task::TaskExecutor::new,
        Arc::new(DenyNamedPolicy::new(["task.list"])),
    );

    let err = h
        .tool
        .execute(json!({"op": "list"}), &ctx())
        .await
        .expect_err("list denied");

    assert!(matches!(err, ToolError::Permission(reason) if reason == "denied task.list"));
}

#[tokio::test]
async fn policy_deny_get_returns_tool_error_permission() {
    let h = build_harness(
        common::ImmediateProvider::new("done"),
        crabgent_task::TaskExecutor::new,
        Arc::new(DenyNamedPolicy::new(["task.get"])),
    );
    let task = insert_task(&h.store, test_task("alice", TaskStatus::Running, None)).await;

    let err = h
        .tool
        .execute(id_args("get", &task.id), &ctx())
        .await
        .expect_err("get denied");

    assert!(matches!(err, ToolError::Permission(reason) if reason == "denied task.get"));
}

#[tokio::test]
async fn policy_deny_cancel_returns_tool_error_permission() {
    let h = build_harness(
        common::ImmediateProvider::new("done"),
        crabgent_task::TaskExecutor::new,
        Arc::new(DenyNamedPolicy::new(["task.cancel"])),
    );
    let task = insert_task(&h.store, test_task("alice", TaskStatus::Running, None)).await;

    let err = h
        .tool
        .execute(id_args("cancel", &task.id), &ctx())
        .await
        .expect_err("cancel denied");

    assert!(matches!(err, ToolError::Permission(reason) if reason == "denied task.cancel"));
}
