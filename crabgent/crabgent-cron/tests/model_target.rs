use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_core::{
    AllowAllPolicy, GlobalModelOverrideStore, Kernel, LlmRequest, LlmResponse, MemoryScope,
    ModelId, ModelInfo, ModelTarget, Owner, Provider, ProviderCapabilities, ProviderError,
    ReasoningEffort, RunCtx, StopReason, Usage,
};
use crabgent_cron::{CronExecCtx, CronExecutor, KernelCronExecutor};
use crabgent_store::{CronJob, CronJobId, CronSchedule, MemoryGlobalOverrideStore, ModelTargetDto};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct SameModelProvider {
    provider: &'static str,
    text: &'static str,
}

struct EchoModelProvider;

struct EchoEffortProvider;

#[async_trait]
impl Provider for SameModelProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Ok(LlmResponse {
            text: self.text.into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        self.provider
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("opus", self.provider)]
    }
}

#[async_trait]
impl Provider for EchoModelProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Ok(LlmResponse {
            text: req.model.to_string(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "stub"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo::minimal("default", "stub"),
            ModelInfo::minimal("explicit", "stub"),
            ModelInfo::minimal("global", "stub"),
        ]
    }
}

#[async_trait]
impl Provider for EchoEffortProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Ok(LlmResponse {
            text: req
                .reasoning_effort
                .map_or("none", ReasoningEffort::as_str)
                .to_owned(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "stub"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        let mut model = ModelInfo::minimal("default", "stub");
        model.caps.reasoning_effort = Some(ReasoningEffort::Low);
        vec![model]
    }
}

fn kernel() -> Arc<Kernel> {
    Arc::new(
        Kernel::builder()
            .provider(SameModelProvider {
                provider: "anthropic",
                text: "anthropic",
            })
            .provider(SameModelProvider {
                provider: "openai",
                text: "openai",
            })
            .policy(AllowAllPolicy)
            .build(),
    )
}

fn job(model_override: Option<ModelTargetDto>) -> CronJob {
    let now = Utc::now();
    CronJob {
        id: CronJobId::new(),
        name: "demo".into(),
        scope: MemoryScope::for_owner(Owner::new("alice")),
        prompt: "say hi".into(),
        schedule: CronSchedule::every(60),
        enabled: true,
        run_once: false,
        model_override,
        reasoning_effort_override: None,
        pre_command: None,
        delivery_ctx: json!({}),
        last_run: None,
        next_run: now,
        created_at: now,
        claimed_at: None,
    }
}

#[tokio::test]
async fn cron_model_override_preserves_provider_qualified_target() {
    let executor = KernelCronExecutor::new(ModelTarget::new("anthropic", "opus"));
    let job = job(Some(ModelTargetDto::Provider {
        provider: "openai".into(),
        id: "opus".into(),
    }));
    let result = executor
        .execute(CronExecCtx {
            job: &job,
            kernel: kernel(),
            prompt: "say hi",
            cancel: CancellationToken::new(),
        })
        .await
        .expect("cron executes");

    assert_eq!(result.final_text.as_deref(), Some("openai"));
    assert!(result.error.is_none());
}

#[tokio::test]
async fn cron_default_model_can_be_provider_qualified() {
    let executor = KernelCronExecutor::new(ModelTarget::new("openai", "opus"));
    let job = job(None);
    let result = executor
        .execute(CronExecCtx {
            job: &job,
            kernel: kernel(),
            prompt: "say hi",
            cancel: CancellationToken::new(),
        })
        .await
        .expect("cron executes");

    assert_eq!(result.final_text.as_deref(), Some("openai"));
    assert!(result.error.is_none());
}

#[tokio::test]
async fn cron_job_model_override_beats_global_override() {
    let global_store = Arc::new(MemoryGlobalOverrideStore::default());
    global_store
        .set_global_model_override(&ModelId::new("global"))
        .await
        .expect("set global override");
    let kernel = Arc::new(
        Kernel::builder()
            .provider(EchoModelProvider)
            .with_global_override_store(Arc::clone(&global_store))
            .policy(AllowAllPolicy)
            .build(),
    );
    let executor = KernelCronExecutor::new("default");
    let job = job(Some(ModelTargetDto::Id("explicit".into())));

    let result = executor
        .execute(CronExecCtx {
            job: &job,
            kernel,
            prompt: "say hi",
            cancel: CancellationToken::new(),
        })
        .await
        .expect("cron executes");

    assert_eq!(result.final_text.as_deref(), Some("explicit"));
    assert!(result.error.is_none());
}

#[tokio::test]
async fn cron_job_reasoning_effort_override_reaches_kernel_run() {
    let kernel = Arc::new(
        Kernel::builder()
            .provider(EchoEffortProvider)
            .policy(AllowAllPolicy)
            .build(),
    );
    let executor = KernelCronExecutor::new("default");
    let mut job = job(None);
    job.reasoning_effort_override = Some(ReasoningEffort::High);

    let result = executor
        .execute(CronExecCtx {
            job: &job,
            kernel,
            prompt: "say hi",
            cancel: CancellationToken::new(),
        })
        .await
        .expect("cron executes");

    assert_eq!(result.final_text.as_deref(), Some("high"));
    assert!(result.error.is_none());
}
