use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::{AllowAllPolicy, MemoryScope, Owner, Subject, Tool, ToolCtx, ToolError};
use crabgent_store::{
    CronJob, CronJobId, CronJobUpdate, CronSchedule, CronStore, Page, StoreError,
};
use crabgent_tool_cron::CronTool;
use serde_json::{Value, json};

#[derive(Clone, Copy)]
enum FailingOp {
    Create,
    List,
    Get,
    Update,
    Delete,
}

struct FailingCronStore {
    fail: FailingOp,
    job: CronJob,
}

impl FailingCronStore {
    fn new(fail: FailingOp) -> Self {
        let now = Utc::now();
        Self {
            fail,
            job: CronJob {
                id: CronJobId::new(),
                name: "existing".to_owned(),
                scope: MemoryScope::for_owner(Owner::new("alice")),
                prompt: "Run me".to_owned(),
                schedule: CronSchedule::every(60),
                enabled: true,
                run_once: false,
                model_override: None,
                reasoning_effort_override: None,
                pre_command: None,
                delivery_ctx: json!({}),
                last_run: None,
                next_run: now,
                created_at: now,
                claimed_at: None,
            },
        }
    }

    const fn job_id(&self) -> &CronJobId {
        &self.job.id
    }
}

#[async_trait]
impl CronStore for FailingCronStore {
    async fn create(&self, _job: &CronJob) -> Result<(), StoreError> {
        match self.fail {
            FailingOp::Create => Err(sentinel_store_error()),
            _ => Ok(()),
        }
    }

    async fn get(&self, _id: &CronJobId) -> Result<Option<CronJob>, StoreError> {
        match self.fail {
            FailingOp::Get => Err(sentinel_store_error()),
            _ => Ok(Some(self.job.clone())),
        }
    }

    async fn list(&self, _scope: &MemoryScope, _page: Page) -> Result<Vec<CronJob>, StoreError> {
        match self.fail {
            FailingOp::List => Err(sentinel_store_error()),
            _ => Ok(vec![self.job.clone()]),
        }
    }

    async fn update(&self, _id: &CronJobId, _update: &CronJobUpdate) -> Result<bool, StoreError> {
        match self.fail {
            FailingOp::Update => Err(sentinel_store_error()),
            _ => Ok(true),
        }
    }

    async fn delete(&self, _id: &CronJobId) -> Result<bool, StoreError> {
        match self.fail {
            FailingOp::Delete => Err(sentinel_store_error()),
            _ => Ok(true),
        }
    }

    async fn claim_due(
        &self,
        _now: DateTime<Utc>,
        _limit: usize,
    ) -> Result<Vec<CronJob>, StoreError> {
        Ok(vec![])
    }

    async fn finish_claim(
        &self,
        _id: &CronJobId,
        _last_run: DateTime<Utc>,
        _next_run: DateTime<Utc>,
        _disable_run_once: bool,
    ) -> Result<(), StoreError> {
        Ok(())
    }

    async fn release_claim_only(&self, _id: &CronJobId) -> Result<(), StoreError> {
        Ok(())
    }

    async fn recover_stuck(&self, _timeout_secs: i64) -> Result<Vec<CronJobId>, StoreError> {
        Ok(vec![])
    }
}

fn sentinel_store_error() -> StoreError {
    StoreError::Backend("dsn=postgres://secret/internal".to_owned())
}

fn failing_tool(fail: FailingOp) -> (CronTool, String) {
    let store = Arc::new(FailingCronStore::new(fail));
    let id = store.job_id().to_string();
    let store_dyn: Arc<dyn CronStore> = store;
    (CronTool::new(store_dyn, Arc::new(AllowAllPolicy)), id)
}

async fn exec_err(tool: &CronTool, args: Value) -> ToolError {
    tool.execute(args, &ToolCtx::new(Subject::new("alice")))
        .await
        .expect_err("cron tool should fail")
}

fn assert_opaque_backend_error(err: ToolError, expected: &str) {
    assert!(
        matches!(&err, ToolError::Execution(_)),
        "expected execution error, got {err:?}"
    );
    let ToolError::Execution(message) = err else {
        return;
    };
    assert!(message.contains(expected), "got {message:?}");
    assert!(!message.contains("postgres://secret"));
    assert!(!message.contains("dsn="));
}

#[tokio::test]
async fn backend_errors_are_opaque_for_llm_surface() {
    for (fail, args, expected) in [
        (
            FailingOp::Create,
            json!({
                "op": "create",
                "scope": {"owner": "alice"},
                "name": "daily-summary",
                "prompt": "Summarize today.",
                "schedule": {"interval_secs": 60}
            }),
            "cron.create: backend unavailable",
        ),
        (
            FailingOp::List,
            json!({"op": "list", "scope": {"owner": "alice"}}),
            "cron.list: backend unavailable",
        ),
    ] {
        let (tool, _) = failing_tool(fail);
        let err = exec_err(&tool, args).await;
        assert_opaque_backend_error(err, expected);
    }

    for (fail, op, expected) in [
        (FailingOp::Get, "get", "cron.get: backend unavailable"),
        (
            FailingOp::Update,
            "update",
            "cron.update: backend unavailable",
        ),
        (
            FailingOp::Delete,
            "delete",
            "cron.delete: backend unavailable",
        ),
    ] {
        let (tool, id) = failing_tool(fail);
        let err = exec_err(&tool, json!({"op": op, "job_id": id, "name": "renamed"})).await;
        assert_opaque_backend_error(err, expected);
    }
}
