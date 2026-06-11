use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::Utc;
use crabgent_core::error::{ProviderError, ToolError};
use crabgent_core::model::ModelInfo;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::types::{LlmRequest, LlmResponse};
use crabgent_core::{MemoryScope, RunCtx};
use crabgent_store::CronJobId;
use crabgent_store::records::CronSchedule;
use crabgent_test_support::{done, tool_call, tool_use};
use serde_json::{Value, json};

fn fixture(owner: Option<&str>, model_override: Option<&str>, prompt: &str) -> CronJob {
    CronJob {
        id: CronJobId::new(),
        name: "demo".into(),
        scope: MemoryScope {
            owner: owner.map(Owner::new),
            ..MemoryScope::default()
        },
        prompt: prompt.into(),
        schedule: CronSchedule::every(60),
        enabled: true,
        run_once: false,
        model_override: model_override.map(|id| crabgent_store::ModelTargetDto::Id(id.into())),
        reasoning_effort_override: None,
        pre_command: None,
        delivery_ctx: json!({}),
        last_run: None,
        next_run: Utc::now(),
        created_at: Utc::now(),
        claimed_at: None,
    }
}

#[test]
fn build_request_uses_default_model_when_override_absent() {
    let exec = KernelCronExecutor::new("claude-haiku-4-5");
    let job = fixture(None, None, "p");
    let req = exec.build_request(&job, "p").expect("valid subject");
    assert_eq!(req.model.as_str(), "claude-haiku-4-5");
    assert!(req.explicit_model.is_none());
}

#[test]
fn build_request_uses_override_as_explicit_model_when_set() {
    let exec = KernelCronExecutor::new("default");
    let job = fixture(None, Some("opus"), "p");
    let req = exec.build_request(&job, "p").expect("valid subject");
    assert_eq!(req.model.as_str(), "default");
    assert_eq!(
        req.explicit_model.as_ref().map(ModelTarget::as_str),
        Some("opus")
    );
}

#[test]
fn build_request_subject_defaults_to_cron_for_global() {
    let exec = KernelCronExecutor::new("m");
    let job = fixture(None, None, "p");
    let req = exec.build_request(&job, "p").expect("valid subject");
    assert_eq!(req.subject.id(), "cron");
}

#[test]
fn build_request_subject_uses_owner_for_user_jobs() {
    let exec = KernelCronExecutor::new("m");
    let job = fixture(Some("alice"), None, "p");
    let req = exec.build_request(&job, "p").expect("valid subject");
    assert_eq!(req.subject.id(), "alice");
}

#[test]
fn cron_with_no_agent_scope_resolves_without_panic() {
    let exec = KernelCronExecutor::new("m");
    let job = fixture(Some("alice"), None, "p");
    let req = exec.build_request(&job, "p").expect("valid subject");
    assert_eq!(req.subject.id(), "alice");
    assert_eq!(req.subject.attr("agent"), None);
}

#[test]
fn cron_with_agent_scope_stamps_subject_attr() {
    let exec = KernelCronExecutor::new("m");
    let mut job = fixture(Some("alice"), None, "p");
    job.scope.agent = Some("X".to_owned());

    let req = exec.build_request(&job, "p").expect("valid subject");

    assert_eq!(req.subject.id(), "alice");
    assert_eq!(req.subject.attr("agent"), Some("X"));
}

#[test]
fn cron_with_channel_scope_stamps_delivery_attrs() {
    let exec = KernelCronExecutor::new("m");
    let mut job = fixture(Some("telegram:478376391"), None, "p");
    job.scope = MemoryScope::for_owner(Owner::new("telegram:478376391"))
        .with_channel("telegram")
        .with_conv("telegram:478376391")
        .with_kind("direct");

    let req = exec.build_request(&job, "p").expect("valid subject");

    assert_eq!(req.subject.id(), "telegram:478376391");
    assert_eq!(req.subject.attr("channel"), Some("telegram"));
    assert_eq!(req.subject.attr("conv"), Some("telegram:478376391"));
    assert_eq!(req.subject.attr("channel_kind"), Some("direct"));
}

#[test]
fn build_request_rejects_empty_owner_subject() {
    let exec = KernelCronExecutor::new("m");
    let job = fixture(Some(""), None, "p");
    assert!(matches!(
        exec.build_request(&job, "p"),
        Err(CronError::InvalidSubject(_))
    ));
}

#[test]
fn build_request_subject_resolver_override() {
    let exec = KernelCronExecutor::new("m").with_subject_resolver(|_| Subject::new("custom"));
    let job = fixture(Some("alice"), None, "p");
    let req = exec.build_request(&job, "p").expect("valid subject");
    assert_eq!(req.subject.id(), "custom");
}

#[test]
fn build_request_fallible_subject_resolver_can_reject() {
    let exec =
        KernelCronExecutor::new("m").with_fallible_subject_resolver(|_| Subject::try_new(""));
    let job = fixture(Some("alice"), None, "p");
    assert!(matches!(
        exec.build_request(&job, "p"),
        Err(CronError::InvalidSubject(_))
    ));
}

#[test]
fn build_request_carries_augmented_prompt_as_user_message() {
    let exec = KernelCronExecutor::new("m");
    let job = fixture(None, None, "raw");
    let req = exec
        .build_request(&job, "augmented")
        .expect("valid subject");
    assert_eq!(req.messages.len(), 1);
    match &req.messages[0] {
        Message::User { content, .. } => match &content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "augmented"),
            other => panic!("expected text block, got {other:?}"),
        },
        other => panic!("expected user message, got {other:?}"),
    }
}

#[test]
fn build_request_includes_system_prompt_when_set() {
    let exec = KernelCronExecutor::new("m").with_system_prompt("Be concise.");
    let job = fixture(None, None, "p");
    let req = exec.build_request(&job, "p").expect("valid subject");
    assert_eq!(req.system_prompt.as_deref(), Some("Be concise."));
}

#[test]
fn build_request_max_turns_propagates() {
    let exec = KernelCronExecutor::new("m").with_max_turns(3);
    let job = fixture(None, None, "p");
    let req = exec.build_request(&job, "p").expect("valid subject");
    assert_eq!(req.max_turns, Some(3));
}

#[test]
fn builder_adds_observer() {
    let exec =
        KernelCronExecutor::new("m").with_observer(Arc::new(crate::observer::NoopCronObserver));
    assert_eq!(exec.observers.len(), 1);
}

struct NamedCountingTool {
    name: &'static str,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for NamedCountingTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "test tool"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, _args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"ok": true}))
    }
}

struct ScriptedProvider {
    requests: Arc<Mutex<Vec<LlmRequest>>>,
    responses: Mutex<Vec<LlmResponse>>,
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.requests
            .lock()
            .expect("requests mutex")
            .push(req.clone());
        let mut responses = self.responses.lock().expect("responses mutex");
        if responses.is_empty() {
            return Err(ProviderError::Other("script exhausted".into()));
        }
        Ok(responses.remove(0))
    }

    fn name(&self) -> &'static str {
        "scripted"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("m", "scripted")]
    }
}

#[tokio::test]
async fn execute_filters_delivery_tools_for_cron_runs() {
    let notify_calls = Arc::new(AtomicUsize::new(0));
    let send_calls = Arc::new(AtomicUsize::new(0));
    let memory_calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let kernel = Arc::new(
        Kernel::builder()
            .provider(ScriptedProvider {
                requests: Arc::clone(&requests),
                responses: Mutex::new(vec![
                    tool_use(vec![tool_call(
                        "call-1",
                        "notify_user",
                        json!({"body": "duplicate delivery"}),
                    )]),
                    done("done"),
                ]),
            })
            .policy(AllowAllPolicy)
            .add_tool(NamedCountingTool {
                name: "notify_user",
                calls: Arc::clone(&notify_calls),
            })
            .add_tool(NamedCountingTool {
                name: "channel_send",
                calls: Arc::clone(&send_calls),
            })
            .add_tool(NamedCountingTool {
                name: "memory",
                calls: Arc::clone(&memory_calls),
            })
            .build(),
    );
    let exec = KernelCronExecutor::new("m");
    let mut job = fixture(Some("telegram:478376391"), None, "p");
    job.scope = MemoryScope::for_owner(Owner::new("telegram:478376391"))
        .with_channel("telegram")
        .with_conv("telegram:478376391");

    let result = exec
        .execute(CronExecCtx {
            job: &job,
            kernel,
            prompt: &job.prompt,
            cancel: CancellationToken::new(),
        })
        .await
        .expect("cron execution should finish");

    assert_eq!(result.final_text.as_deref(), Some("done"));
    assert_eq!(notify_calls.load(Ordering::SeqCst), 0);
    assert_eq!(send_calls.load(Ordering::SeqCst), 0);
    assert_eq!(memory_calls.load(Ordering::SeqCst), 0);

    let requests = requests.lock().expect("requests mutex");
    let advertised = requests[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(advertised, vec!["memory"]);
}

#[test]
fn exec_result_is_success_only_for_final_text() {
    assert!(
        CronExecResult {
            final_text: Some("ok".into()),
            error: None,
        }
        .is_success()
    );
    assert!(!CronExecResult::default().is_success());
    assert!(
        !CronExecResult {
            final_text: Some("ok".into()),
            error: Some("e".into()),
        }
        .is_success()
    );
}
