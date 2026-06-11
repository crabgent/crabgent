use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::{AllowAllPolicy, PolicyHook, StrictPolicy, Subject};
use crabgent_store::{GoalId, GoalStore, MemoryGoalStore, SessionId, StoreError, ThreadGoal};
use serde_json::json;

use super::*;

fn ctx_with_session(subject: Subject, session: &SessionId) -> ToolCtx {
    ToolCtx::new(subject).with_session_id(session.to_string())
}

fn tool_with(store: Arc<dyn GoalStore>, policy: Arc<dyn PolicyHook>) -> GoalTool {
    GoalTool::new(store, policy)
}

fn allow_tool() -> GoalTool {
    tool_with(
        Arc::new(MemoryGoalStore::default()),
        Arc::new(AllowAllPolicy),
    )
}

#[tokio::test]
async fn create_then_get_roundtrip() {
    let tool = allow_tool();
    let session = SessionId::new();
    let ctx = ctx_with_session(Subject::new("alice"), &session);

    let created = tool
        .execute_result(
            json!({"op": "create", "objective": "ship the feature"}),
            &ctx,
        )
        .await
        .expect("create");
    assert!(!created.is_error);
    assert_eq!(created.output["created"], true);
    assert_eq!(created.output["goal"]["objective"], "ship the feature");
    assert_eq!(created.output["goal"]["status"], "active");
    assert!(created.output["goal"].get("goal_id").is_none());

    let got = tool
        .execute_result(json!({"op": "get"}), &ctx)
        .await
        .expect("get");
    assert_eq!(got.output["goal"]["objective"], "ship the feature");
}

#[tokio::test]
async fn get_without_goal_returns_null() {
    let tool = allow_tool();
    let ctx = ctx_with_session(Subject::new("alice"), &SessionId::new());
    let got = tool
        .execute_result(json!({"op": "get"}), &ctx)
        .await
        .expect("get");
    assert!(got.output["goal"].is_null());
    assert!(!got.is_error);
}

#[tokio::test]
async fn create_conflict_is_soft_error() {
    let tool = allow_tool();
    let ctx = ctx_with_session(Subject::new("alice"), &SessionId::new());
    tool.execute_result(json!({"op": "create", "objective": "first"}), &ctx)
        .await
        .expect("first create");
    let second = tool
        .execute_result(json!({"op": "create", "objective": "second"}), &ctx)
        .await
        .expect("second create returns a result");
    assert!(second.is_error);
    let error = second.output["error"].as_str().expect("error string");
    assert!(error.contains("already has a goal"), "got: {error}");
    // The existing goal is surfaced so the model can decide what to do.
    assert_eq!(second.output["goal"]["objective"], "first");
}

#[tokio::test]
async fn create_requires_objective_and_positive_budget() {
    let tool = allow_tool();
    let ctx = ctx_with_session(Subject::new("alice"), &SessionId::new());

    let missing = tool
        .execute_result(json!({"op": "create"}), &ctx)
        .await
        .expect_err("missing objective");
    assert!(matches!(missing, ToolError::InvalidArgs(_)));

    let bad_budget = tool
        .execute_result(
            json!({"op": "create", "objective": "x", "token_budget": 0}),
            &ctx,
        )
        .await
        .expect_err("zero budget");
    assert!(matches!(bad_budget, ToolError::InvalidArgs(_)));
}

#[tokio::test]
async fn update_only_accepts_complete_or_blocked() {
    let tool = allow_tool();
    let ctx = ctx_with_session(Subject::new("alice"), &SessionId::new());
    tool.execute_result(json!({"op": "create", "objective": "obj"}), &ctx)
        .await
        .expect("create");

    let completed = tool
        .execute_result(json!({"op": "update", "status": "complete"}), &ctx)
        .await
        .expect("update complete");
    assert_eq!(completed.output["updated"], true);
    assert_eq!(completed.output["goal"]["status"], "complete");

    for forbidden in [
        "paused",
        "active",
        "budget_limited",
        "usage_limited",
        "resume",
    ] {
        let err = tool
            .execute_result(json!({"op": "update", "status": forbidden}), &ctx)
            .await
            .expect_err("forbidden status");
        assert!(
            matches!(err, ToolError::InvalidArgs(_)),
            "status {forbidden} must be rejected, got {err:?}"
        );
    }
}

#[tokio::test]
async fn update_blocked_sets_blocked_status() {
    let tool = allow_tool();
    let ctx = ctx_with_session(Subject::new("alice"), &SessionId::new());
    tool.execute_result(json!({"op": "create", "objective": "obj"}), &ctx)
        .await
        .expect("create");
    let blocked = tool
        .execute_result(json!({"op": "update", "status": "blocked"}), &ctx)
        .await
        .expect("update blocked");
    assert_eq!(blocked.output["goal"]["status"], "blocked");
}

#[tokio::test]
async fn update_without_goal_is_soft_error() {
    let tool = allow_tool();
    let ctx = ctx_with_session(Subject::new("alice"), &SessionId::new());
    let out = tool
        .execute_result(json!({"op": "update", "status": "complete"}), &ctx)
        .await
        .expect("update with no goal");
    assert!(out.is_error);
    assert!(
        out.output["error"]
            .as_str()
            .expect("error")
            .contains("no goal")
    );
}

#[tokio::test]
async fn no_session_is_soft_error() {
    let tool = allow_tool();
    let ctx = ToolCtx::new(Subject::new("alice")); // no session bound
    let out = tool
        .execute_result(json!({"op": "get"}), &ctx)
        .await
        .expect("get without session");
    assert!(out.is_error);
    assert!(
        out.output["error"]
            .as_str()
            .expect("error")
            .contains("session")
    );
}

#[tokio::test]
async fn create_denied_returns_permission() {
    let tool = tool_with(
        Arc::new(MemoryGoalStore::default()),
        Arc::new(StrictPolicy::builder().build()), // deny by default
    );
    let ctx = ctx_with_session(Subject::new("alice"), &SessionId::new());
    let err = tool
        .execute_result(json!({"op": "create", "objective": "x"}), &ctx)
        .await
        .expect_err("create denied");
    assert!(matches!(err, ToolError::Permission(_)));
}

#[tokio::test]
async fn origin_gate_denies_model_allows_stamped_subject() {
    let policy = Arc::new(
        StrictPolicy::builder()
            .allow_goal_create_for(GOAL_ORIGIN_ATTR, GOAL_ORIGIN_USER)
            .allow_goal_get()
            .build(),
    );
    let store = Arc::new(MemoryGoalStore::default());
    let tool = tool_with(store, policy);
    let session = SessionId::new();

    // Model-initiated create (no origin attr) is denied.
    let model_ctx = ctx_with_session(Subject::new("agent"), &session);
    let denied = tool
        .execute_result(json!({"op": "create", "objective": "x"}), &model_ctx)
        .await
        .expect_err("model create denied");
    assert!(matches!(denied, ToolError::Permission(_)));

    // A trusted path that stamps the origin attr is allowed.
    let host_ctx = ctx_with_session(
        Subject::new("alice").with_attr(GOAL_ORIGIN_ATTR, GOAL_ORIGIN_USER),
        &session,
    );
    let created = tool
        .execute_result(json!({"op": "create", "objective": "x"}), &host_ctx)
        .await
        .expect("host create allowed");
    assert!(!created.is_error);
}

/// Store double whose every method fails with a schema-revealing message,
/// to prove the tool never leaks backend detail to the LLM.
struct LeakyGoalStore;

#[async_trait]
impl GoalStore for LeakyGoalStore {
    async fn create(&self, _goal: &ThreadGoal) -> Result<(), StoreError> {
        Err(StoreError::Backend(
            "dsn=postgres://secret@host/db sensitive".to_owned(),
        ))
    }
    async fn get(&self, _id: &GoalId) -> Result<Option<ThreadGoal>, StoreError> {
        Err(StoreError::Backend("table thread_goals leaked".to_owned()))
    }
    async fn get_for_session(
        &self,
        _session: &SessionId,
    ) -> Result<Option<ThreadGoal>, StoreError> {
        Err(StoreError::Backend("table thread_goals leaked".to_owned()))
    }
    async fn update(
        &self,
        _id: &GoalId,
        _update: &crabgent_store::ThreadGoalUpdate,
    ) -> Result<bool, StoreError> {
        Err(StoreError::Backend("update leaked".to_owned()))
    }
    async fn account_usage(
        &self,
        _id: &GoalId,
        _token_delta: i64,
        _time_delta_seconds: i64,
        _at: crabgent_store::DateTime<crabgent_store::Utc>,
    ) -> Result<Option<ThreadGoal>, StoreError> {
        Err(StoreError::Backend("account leaked".to_owned()))
    }
    async fn delete(&self, _id: &GoalId) -> Result<bool, StoreError> {
        Err(StoreError::Backend("delete leaked".to_owned()))
    }
    async fn list_by_status(
        &self,
        _status: crabgent_store::GoalStatus,
        _page: crabgent_store::Page,
    ) -> Result<Vec<ThreadGoal>, StoreError> {
        Err(StoreError::Backend("list leaked".to_owned()))
    }
    async fn resume_suspended(
        &self,
        _at: crabgent_store::DateTime<crabgent_store::Utc>,
    ) -> Result<Vec<ThreadGoal>, StoreError> {
        Err(StoreError::Backend("resume leaked".to_owned()))
    }
}

#[tokio::test]
async fn store_failure_does_not_leak_backend_detail() {
    let tool = tool_with(Arc::new(LeakyGoalStore), Arc::new(AllowAllPolicy));
    let ctx = ctx_with_session(Subject::new("alice"), &SessionId::new());
    let err = tool
        .execute_result(json!({"op": "create", "objective": "x"}), &ctx)
        .await
        .expect_err("backend failure");
    let ToolError::Execution(message) = err else {
        panic!("expected execution error, got something else");
    };
    assert_eq!(message, "goal.create: backend unavailable");
    assert!(!message.contains("secret"), "leaked secret: {message}");
    assert!(!message.contains("postgres"), "leaked dsn: {message}");
    assert!(!message.contains("thread_goals"), "leaked table: {message}");
}
