mod common;

use crabgent_core::{Tool, ToolError};
use crabgent_store::{TaskId, TaskStatus};

use common::{
    build_immediate_harness, create_args, ctx_with_parent, insert_task, test_task_with_id,
};

#[tokio::test]
async fn depth_limit_returns_tool_error_permission_with_safe_reason() {
    let h = build_immediate_harness();
    let root = insert_task(
        &h.store,
        test_task_with_id(TaskId::new(), "alice", TaskStatus::Running, None),
    )
    .await;
    let mid = insert_task(
        &h.store,
        test_task_with_id(
            TaskId::new(),
            "alice",
            TaskStatus::Running,
            Some(root.id.clone()),
        ),
    )
    .await;
    let leaf = insert_task(
        &h.store,
        test_task_with_id(
            TaskId::new(),
            "alice",
            TaskStatus::Running,
            Some(mid.id.clone()),
        ),
    )
    .await;

    let err = h
        .tool
        .execute(create_args("too deep"), &ctx_with_parent(&leaf.id))
        .await
        .expect_err("depth denied");

    let rendered = err.to_string();
    assert!(matches!(err, ToolError::Permission(_)));
    assert!(rendered.contains("nested task depth limit reached"));
    assert!(!rendered.contains(&root.id.to_string()));
    assert!(!rendered.contains(&mid.id.to_string()));
    assert!(!rendered.contains(&leaf.id.to_string()));
}

#[tokio::test]
async fn parent_chain_with_cycle_rejected_without_infloop() {
    let h = build_immediate_harness();
    let a = TaskId::new();
    let b = TaskId::new();
    insert_task(
        &h.store,
        test_task_with_id(a.clone(), "alice", TaskStatus::Running, Some(b.clone())),
    )
    .await;
    insert_task(
        &h.store,
        test_task_with_id(b.clone(), "alice", TaskStatus::Running, Some(a)),
    )
    .await;

    let err = h
        .tool
        .execute(create_args("cycle"), &ctx_with_parent(&b))
        .await
        .expect_err("cycle denied");

    assert!(matches!(err, ToolError::Permission(_)));
}
