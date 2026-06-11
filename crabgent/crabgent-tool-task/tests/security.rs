mod common;

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crabgent_core::{Tool, ToolError};
use crabgent_task::TaskExecutor;
use serde_json::json;

use common::{
    FailingStore, HangingProvider, ImmediateProvider, build_harness, build_harness_for_store,
    create_args, ctx, id_args,
};

#[tokio::test]
async fn cancel_output_contains_only_task_id_and_status() {
    let (provider, started) = HangingProvider::new();
    let h = build_harness(
        provider,
        |store| TaskExecutor::new(store).with_shutdown_grace(Duration::from_millis(20)),
        Arc::new(crabgent_core::AllowAllPolicy),
    );
    let out = h
        .tool
        .execute(create_args("hang"), &ctx())
        .await
        .expect("create succeeds");
    let id = out["task_id"]
        .as_str()
        .expect("task id")
        .parse()
        .expect("value should parse");
    tokio::time::timeout(Duration::from_secs(1), started)
        .await
        .expect("provider starts")
        .expect("started signal");

    let out = h
        .tool
        .execute(id_args("cancel", &id), &ctx())
        .await
        .expect("cancel succeeds");

    let keys: BTreeSet<_> = out
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, BTreeSet::from(["cancelled", "status", "task_id"]));
}

#[tokio::test]
async fn store_error_does_not_leak_into_tool_result() {
    let h = build_harness_for_store(
        Arc::new(FailingStore),
        ImmediateProvider::new("done"),
        Arc::new(crabgent_core::AllowAllPolicy),
    );

    let err = h
        .tool
        .execute(json!({"op": "list"}), &ctx())
        .await
        .expect_err("store fails");

    assert!(matches!(err, ToolError::Execution(_)));
    let rendered = err.to_string();
    assert!(rendered.contains("task.list: backend unavailable"));
    assert!(!rendered.contains("postgres://secret"));
    assert!(!rendered.contains("dsn="));
}

#[tokio::test]
async fn lazy_tool_without_kernel_returns_tool_error() {
    let store = Arc::new(crabgent_store::MemoryTaskStore::default());
    let executor = Arc::new(TaskExecutor::new(Arc::clone(&store)));
    let tool = crabgent_tool_task::TaskTool::new_lazy(
        executor,
        Arc::new(OnceLock::new()),
        store,
        Arc::new(crabgent_core::AllowAllPolicy),
    );

    let err = tool
        .execute(create_args("missing kernel"), &ctx())
        .await
        .expect_err("unset lazy kernel should not panic");

    assert!(matches!(err, ToolError::Execution(_)));
    assert!(err.to_string().contains("kernel not initialised"));
}
