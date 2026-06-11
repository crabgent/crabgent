use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
use crabgent_core::{Action, PolicyDecision, PolicyHook, Subject, Tool, ToolCtx};
use crabgent_store::CronStore;
use crabgent_store::memory::MemoryCronStore;
use crabgent_tool_cron::CronTool;
use serde_json::{Value, json};

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("alice"))
}

fn make_tool(policy: Arc<dyn PolicyHook>) -> (CronTool, Arc<MemoryCronStore>) {
    let store = Arc::new(MemoryCronStore::default());
    (tool_for_store(&store, policy), store)
}

fn tool_for_store(store: &Arc<MemoryCronStore>, policy: Arc<dyn PolicyHook>) -> CronTool {
    let store_dyn: Arc<dyn CronStore> = store.clone();
    CronTool::new(store_dyn, policy)
}

fn interval_schedule(secs: u64) -> Value {
    json!({"interval_secs": secs})
}

fn cron_schedule(expr: &str) -> Value {
    json!({"cron_expr": expr, "cron_tz": "UTC"})
}

fn scope_value(owner: Option<&str>) -> Value {
    owner.map_or_else(|| json!({}), |owner| json!({ "owner": owner }))
}

fn create_args(owner: Option<&str>) -> Value {
    let mut args = json!({
        "op": "create",
        "name": "daily-summary",
        "prompt": "Summarize today.",
        "schedule": interval_schedule(60)
    });
    if owner.is_some() {
        set_arg(&mut args, "scope", scope_value(owner));
    }
    args
}

async fn create_job(tool: &CronTool, owner: Option<&str>) -> Value {
    exec(tool, create_args(owner)).await
}

async fn exec(tool: &CronTool, args: Value) -> Value {
    tool.execute(args, &ctx())
        .await
        .expect("cron tool should succeed")
}

async fn exec_err(tool: &CronTool, args: Value) -> ToolError {
    tool.execute(args, &ctx())
        .await
        .expect_err("cron tool should fail")
}

fn set_arg(args: &mut Value, key: &str, value: Value) {
    args.as_object_mut()
        .expect("args should be a JSON object")
        .insert(key.to_owned(), value);
}

fn at<'a>(value: &'a Value, pointer: &str) -> &'a Value {
    value.pointer(pointer).expect("JSON pointer should exist")
}

fn assert_json(value: &Value, pointer: &str, expected: &Value) {
    assert_eq!(at(value, pointer), expected);
}

fn id_from(result: &Value) -> String {
    at(result, "/job/id")
        .as_str()
        .expect("value should be a string")
        .to_owned()
}

fn assert_invalid(err: ToolError, needle: &str) {
    assert!(matches!(err, ToolError::InvalidArgs(msg) if msg.contains(needle)));
}

#[tokio::test]
async fn create_defaults_enabled_and_run_once() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    let result = create_job(&tool, Some("alice")).await;
    assert_json(&result, "/created", &json!(true));
    assert_json(&result, "/job/scope/owner", &json!("alice"));
    assert_json(&result, "/job/enabled", &json!(true));
    assert_json(&result, "/job/run_once", &json!(false));
    assert_json(&result, "/job/schedule/interval_secs", &json!(60));
    assert!(at(&result, "/job/pre_command").is_null());
    assert!(at(&result, "/job/model_override").is_null());
    assert!(at(&result, "/job/delivery_ctx").is_object());
}

#[tokio::test]
async fn list_filters_scope_owner_and_empty_scope_returns_all() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    create_job(&tool, Some("alice")).await;
    create_job(&tool, None).await;
    let alice = exec(
        &tool,
        json!({"op": "list", "scope": scope_value(Some("alice"))}),
    )
    .await;
    let all = exec(&tool, json!({"op": "list", "scope": {}})).await;
    assert_json(&alice, "/count", &json!(1));
    assert_json(&alice, "/jobs/0/scope/owner", &json!("alice"));
    assert_json(&all, "/count", &json!(2));
}

#[tokio::test]
async fn get_returns_existing_job() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    let created = create_job(&tool, Some("alice")).await;
    let id = id_from(&created);
    let got = exec(&tool, json!({"op": "get", "job_id": id})).await;
    assert_json(&got, "/job/name", &json!("daily-summary"));
    assert_json(&got, "/job/prompt", &json!("Summarize today."));
}

#[tokio::test]
async fn update_keeps_clears_and_sets_optional_fields() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    let created = exec(
        &tool,
        json!({
            "op": "create",
            "scope": scope_value(Some("alice")),
            "name": "with-options",
            "prompt": "Run me",
            "schedule": interval_schedule(60),
            "model_override": "old-model",
            "pre_command": "old command"
        }),
    )
    .await;
    let id = id_from(&created);
    let kept = exec(
        &tool,
        json!({"op": "update", "job_id": id, "name": "renamed"}),
    )
    .await;
    assert_json(&kept, "/job/name", &json!("renamed"));
    assert_json(&kept, "/job/model_override", &json!("old-model"));
    assert_json(&kept, "/job/pre_command", &json!("old command"));

    let cleared = exec(
        &tool,
        json!({"op": "update", "job_id": id, "model_override": null, "pre_command": null}),
    )
    .await;
    assert!(at(&cleared, "/job/model_override").is_null());
    assert!(at(&cleared, "/job/pre_command").is_null());

    let set = exec(
        &tool,
        json!({
            "op": "update",
            "job_id": id,
            "model_override": {"provider": "anthropic", "id": "claude-haiku-4-5"},
            "pre_command": "new command",
            "schedule": cron_schedule("0 9 * * *")
        }),
    )
    .await;
    assert_json(&set, "/job/model_override/provider", &json!("anthropic"));
    assert_json(&set, "/job/pre_command", &json!("new command"));
    assert_json(&set, "/job/schedule/cron_expr", &json!("0 9 * * *"));
}

#[tokio::test]
async fn delivery_ctx_must_be_a_json_object() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    for delivery_ctx in [json!(["slack"]), json!("slack")] {
        let mut args = create_args(Some("alice"));
        set_arg(&mut args, "delivery_ctx", delivery_ctx);
        let err = exec_err(&tool, args).await;
        assert_invalid(err, "delivery_ctx must be a JSON object");
    }
}

#[tokio::test]
async fn update_sets_delivery_ctx_and_rejects_non_object() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    let created = create_job(&tool, Some("alice")).await;
    let id = id_from(&created);
    let updated = exec(
        &tool,
        json!({
            "op": "update",
            "job_id": id,
            "delivery_ctx": {"channel": "slack"}
        }),
    )
    .await;
    assert_json(&updated, "/job/delivery_ctx/channel", &json!("slack"));

    let err = exec_err(
        &tool,
        json!({
            "op": "update",
            "job_id": id,
            "delivery_ctx": ["slack"]
        }),
    )
    .await;
    assert_invalid(err, "delivery_ctx must be a JSON object");
}

#[tokio::test]
async fn delete_removes_existing_job() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    let created = create_job(&tool, Some("alice")).await;
    let id = id_from(&created);

    let deleted = exec(&tool, json!({"op": "delete", "job_id": id})).await;
    let err = exec_err(&tool, json!({"op": "get", "job_id": id})).await;

    assert_json(&deleted, "/deleted", &json!(true));
    assert!(matches!(err, ToolError::NotFound(_)));
}

#[tokio::test]
async fn missing_create_fields_are_invalid_args() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    let cases = [
        (
            json!({"op": "create", "name": "n", "prompt": "p"}),
            "schedule",
        ),
        (
            json!({"op": "create", "prompt": "p", "schedule": interval_schedule(60)}),
            "name",
        ),
        (
            json!({"op": "create", "name": "n", "schedule": interval_schedule(60)}),
            "prompt",
        ),
    ];

    for (args, needle) in cases {
        let err = exec_err(&tool, args).await;
        assert_invalid(err, needle);
    }
}

#[tokio::test]
async fn missing_job_id_is_invalid_for_get_update_delete() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));

    for op in ["get", "update", "delete"] {
        let err = exec_err(&tool, json!({"op": op})).await;
        assert_invalid(err, "job_id");
    }
}

#[tokio::test]
async fn malformed_args_missing_op_is_invalid() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    let err = exec_err(&tool, json!({})).await;
    assert_invalid(err, "op");
}

#[tokio::test]
async fn deny_all_blocks_every_op() {
    let (seed_tool, store) = make_tool(Arc::new(AllowAllPolicy));
    let created = create_job(&seed_tool, Some("alice")).await;
    let id = id_from(&created);
    let denied_tool = tool_for_store(&store, Arc::new(DenyAllPolicy));
    let cases = [
        create_args(Some("alice")),
        json!({"op": "list", "scope": scope_value(Some("alice"))}),
        json!({"op": "get", "job_id": id}),
        json!({"op": "update", "job_id": id, "name": "blocked"}),
        json!({"op": "delete", "job_id": id}),
    ];

    for args in cases {
        let err = denied_tool
            .execute(args, &ctx())
            .await
            .expect_err("expected error");
        assert!(matches!(err, ToolError::Permission(_)));
    }
}

#[tokio::test]
async fn unknown_id_returns_not_found_for_read_write_delete() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    let missing = crabgent_store::CronJobId::new().to_string();

    for args in [
        json!({"op": "get", "job_id": missing}),
        json!({"op": "update", "job_id": missing, "name": "new"}),
        json!({"op": "delete", "job_id": missing}),
    ] {
        let err = tool
            .execute(args, &ctx())
            .await
            .expect_err("expected error");
        assert!(matches!(err, ToolError::NotFound(_)));
    }
}

#[tokio::test]
async fn schedule_validation_rejects_invalid_shapes() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    let cases = [
        (
            json!({"interval_secs": 60, "cron_expr": "0 9 * * *"}),
            "either interval_secs or cron_expr",
        ),
        (json!({}), "interval_secs or cron_expr"),
        (json!({"interval_secs": 0}), "at least 1"),
        (json!({"cron_tz": "UTC"}), "cron_expr"),
        (json!({"cron_expr": "garbage"}), "invalid cron expression"),
    ];

    for (schedule, needle) in cases {
        let mut args = create_args(Some("alice"));
        set_arg(&mut args, "schedule", schedule);
        let err = tool
            .execute(args, &ctx())
            .await
            .expect_err("expected error");
        assert_invalid(err, needle);
    }
}

#[tokio::test]
async fn bad_timezone_is_accepted_like_scheduler_fallback() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    let result = tool
        .execute(
            json!({
                "op": "create",
                "scope": scope_value(Some("alice")),
                "name": "bad-tz",
                "prompt": "Run me",
                "schedule": {"cron_expr": "0 9 * * *", "cron_tz": "Mars/Olympus_Mons"}
            }),
            &ctx(),
        )
        .await
        .expect("test result");

    assert_json(&result, "/created", &json!(true));
    assert_json(
        &result,
        "/job/schedule/cron_tz",
        &json!("Mars/Olympus_Mons"),
    );
}

#[tokio::test]
async fn limit_zero_is_invalid_and_large_limit_is_clamped() {
    let (tool, _) = make_tool(Arc::new(AllowAllPolicy));
    for _ in 0..12 {
        create_job(&tool, Some("alice")).await;
    }

    let zero = tool
        .execute(
            json!({"op": "list", "scope": scope_value(Some("alice")), "limit": 0}),
            &ctx(),
        )
        .await
        .expect_err("expected error");
    let large = tool
        .execute(
            json!({"op": "list", "scope": scope_value(Some("alice")), "limit": 999}),
            &ctx(),
        )
        .await
        .expect("test result");

    assert_invalid(zero, "limit");
    assert_json(&large, "/count", &json!(12));
}

struct OnlyCreatePolicy;

#[async_trait]
impl PolicyHook for OnlyCreatePolicy {
    async fn allow(&self, _: &Subject, action: &Action) -> PolicyDecision {
        if matches!(action, Action::CronCreate { .. }) {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny(format!("only create allowed, got {}", action.name()))
        }
    }
}

#[tokio::test]
async fn typed_action_lets_policy_distinguish_cron_ops() {
    let (tool, _) = make_tool(Arc::new(OnlyCreatePolicy));
    tool.execute(create_args(Some("alice")), &ctx())
        .await
        .expect("test result");

    let err = tool
        .execute(
            json!({"op": "list", "scope": scope_value(Some("alice"))}),
            &ctx(),
        )
        .await
        .expect_err("expected error");

    assert!(matches!(err, ToolError::Permission(msg) if msg.contains("only create allowed")));
}
