use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::policy::{AllowAllPolicy, DenyAllPolicy};
use crabgent_core::{
    Action, MemoryScope, Owner, PolicyDecision, PolicyHook, Subject, Tool, ToolCtx,
};
use crabgent_store::memory::MemoryCronStore;
use crabgent_store::{
    CronJob, CronJobId, CronJobUpdate, CronSchedule, CronStore, Page, StoreError,
};
use crabgent_tool_cron::CronTool;
use serde_json::{Value, json};

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("alice"))
}

fn scope(owner: &str) -> MemoryScope {
    MemoryScope::for_owner(Owner::new(owner))
}

fn scope_value(owner: &str) -> Value {
    json!({ "owner": owner })
}

fn create_args(owner: &str) -> Value {
    json!({
        "op": "create",
        "scope": scope_value(owner),
        "name": "daily",
        "prompt": "run",
        "schedule": {"interval_secs": 60},
    })
}

fn job(owner: &str) -> CronJob {
    let now = Utc::now();
    CronJob {
        id: CronJobId::new(),
        name: "daily".to_owned(),
        scope: scope(owner),
        prompt: "run".to_owned(),
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
    }
}

#[derive(Default)]
struct CountingCronStore {
    inner: MemoryCronStore,
    gets: AtomicUsize,
    updates: AtomicUsize,
    deletes: AtomicUsize,
}

impl CountingCronStore {
    async fn seed(&self, job: &CronJob) {
        self.inner.create(job).await.expect("seed job");
    }

    fn get_count(&self) -> usize {
        self.gets.load(Ordering::SeqCst)
    }

    fn update_count(&self) -> usize {
        self.updates.load(Ordering::SeqCst)
    }

    fn delete_count(&self) -> usize {
        self.deletes.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CronStore for CountingCronStore {
    async fn create(&self, job: &CronJob) -> Result<(), StoreError> {
        self.inner.create(job).await
    }

    async fn get(&self, id: &CronJobId) -> Result<Option<CronJob>, StoreError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        self.inner.get(id).await
    }

    async fn list(&self, scope: &MemoryScope, page: Page) -> Result<Vec<CronJob>, StoreError> {
        self.inner.list(scope, page).await
    }

    async fn update(&self, id: &CronJobId, update: &CronJobUpdate) -> Result<bool, StoreError> {
        self.updates.fetch_add(1, Ordering::SeqCst);
        self.inner.update(id, update).await
    }

    async fn delete(&self, id: &CronJobId) -> Result<bool, StoreError> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(id).await
    }

    async fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<CronJob>, StoreError> {
        self.inner.claim_due(now, limit).await
    }

    async fn finish_claim(
        &self,
        id: &CronJobId,
        last_run: DateTime<Utc>,
        next_run: DateTime<Utc>,
        disable_run_once: bool,
    ) -> Result<(), StoreError> {
        self.inner
            .finish_claim(id, last_run, next_run, disable_run_once)
            .await
    }

    async fn release_claim_only(&self, id: &CronJobId) -> Result<(), StoreError> {
        self.inner.release_claim_only(id).await
    }

    async fn recover_stuck(&self, timeout_secs: i64) -> Result<Vec<CronJobId>, StoreError> {
        self.inner.recover_stuck(timeout_secs).await
    }
}

#[tokio::test]
async fn denied_get_update_delete_do_not_touch_store() {
    let store = Arc::new(CountingCronStore::default());
    let existing = job("alice");
    store.seed(&existing).await;
    let tool = CronTool::new(store.clone(), Arc::new(DenyAllPolicy));

    for args in [
        json!({"op": "get", "job_id": existing.id.to_string(), "scope": scope_value("alice")}),
        json!({"op": "update", "job_id": existing.id.to_string(), "scope": scope_value("alice"), "name": "renamed"}),
        json!({"op": "delete", "job_id": existing.id.to_string(), "scope": scope_value("alice")}),
    ] {
        let err = tool.execute(args, &ctx()).await.expect_err("denied");
        assert!(matches!(err, crabgent_core::ToolError::Permission(_)));
    }

    assert_eq!(store.get_count(), 0);
    assert_eq!(store.update_count(), 0);
    assert_eq!(store.delete_count(), 0);
}

#[derive(Default)]
struct RecordingPolicy {
    actions: std::sync::Mutex<Vec<Action>>,
}

impl RecordingPolicy {
    fn actions(&self) -> Vec<Action> {
        self.actions.lock().expect("actions lock").clone()
    }
}

#[async_trait]
impl PolicyHook for RecordingPolicy {
    async fn allow(&self, _: &Subject, action: &Action) -> PolicyDecision {
        self.actions
            .lock()
            .expect("actions lock")
            .push(action.clone());
        PolicyDecision::Allow
    }
}

#[tokio::test]
async fn get_policy_uses_requested_scope_before_read() {
    let store = Arc::new(CountingCronStore::default());
    let existing = job("alice");
    store.seed(&existing).await;
    let policy = Arc::new(RecordingPolicy::default());
    let tool = CronTool::new(store, policy.clone());

    let err = tool
        .execute(
            json!({"op": "get", "job_id": existing.id.to_string(), "scope": scope_value("bob")}),
            &ctx(),
        )
        .await
        .expect_err("scope mismatch");

    assert!(matches!(err, crabgent_core::ToolError::NotFound(_)));
    assert_eq!(
        policy.actions(),
        vec![Action::CronGet {
            id: existing.id.to_string(),
            scope: scope("bob"),
        }]
    );
}

#[tokio::test]
async fn typed_actions_include_scope_for_all_ops() {
    let store = Arc::new(CountingCronStore::default());
    let policy = Arc::new(RecordingPolicy::default());
    let tool = CronTool::new(store, policy.clone());
    let created = tool
        .execute(create_args("alice"), &ctx())
        .await
        .expect("create job");
    let id = created["job"]["id"].as_str().expect("job id").to_owned();

    tool.execute(json!({"op": "list", "scope": scope_value("alice")}), &ctx())
        .await
        .expect("list jobs");
    tool.execute(
        json!({"op": "get", "job_id": id, "scope": scope_value("alice")}),
        &ctx(),
    )
    .await
    .expect("get job");
    tool.execute(
        json!({"op": "update", "job_id": id, "scope": scope_value("alice"), "name": "renamed"}),
        &ctx(),
    )
    .await
    .expect("update job");
    tool.execute(
        json!({"op": "delete", "job_id": id, "scope": scope_value("alice")}),
        &ctx(),
    )
    .await
    .expect("delete job");

    let scope = scope("alice");
    assert_eq!(
        policy.actions(),
        vec![
            Action::CronCreate {
                scope: scope.clone()
            },
            Action::CronList {
                scope: scope.clone()
            },
            Action::CronGet {
                id: id.clone(),
                scope: scope.clone()
            },
            Action::CronGet {
                id: id.clone(),
                scope: scope.clone()
            },
            Action::CronUpdate {
                id: id.clone(),
                scope: scope.clone()
            },
            Action::CronUpdate {
                id: id.clone(),
                scope: scope.clone()
            },
            Action::CronDelete {
                id: id.clone(),
                scope: scope.clone()
            },
            Action::CronDelete { id, scope },
        ]
    );
}

#[tokio::test]
async fn scope_mismatch_returns_not_found_without_mutation() {
    let store = Arc::new(CountingCronStore::default());
    let existing = job("alice");
    store.seed(&existing).await;
    let tool = CronTool::new(store.clone(), Arc::new(AllowAllPolicy));

    let update_err = tool
        .execute(
            json!({"op": "update", "job_id": existing.id.to_string(), "scope": scope_value("bob"), "name": "renamed"}),
            &ctx(),
        )
        .await
        .expect_err("scope mismatch update");
    assert!(matches!(update_err, crabgent_core::ToolError::NotFound(_)));

    let delete_err = tool
        .execute(
            json!({"op": "delete", "job_id": existing.id.to_string(), "scope": scope_value("bob")}),
            &ctx(),
        )
        .await
        .expect_err("scope mismatch delete");
    assert!(matches!(delete_err, crabgent_core::ToolError::NotFound(_)));

    let stored = store
        .inner
        .get(&existing.id)
        .await
        .expect("load job")
        .expect("job still exists");
    assert_eq!(stored.name, "daily");
    assert_eq!(store.update_count(), 0);
    assert_eq!(store.delete_count(), 0);
}
