//! [`CronExecutor`]: trait that runs the cron job's effective prompt and
//! produces the final assistant text (or an error description).
//!
//! [`KernelCronExecutor`] is the default implementation. It builds a
//! [`RunRequest`] from the job (default model plus optional explicit job
//! override, subject resolver, optional system prompt, single user-message)
//! and calls [`Kernel::run_streaming`]. Kernel-level errors are captured as the
//! [`CronExecResult::error`] string so the scheduler still advances the
//! schedule rather than retrying forever; executor-internal failures
//! (e.g. no default model configured) are returned as [`CronError`].

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::ActivityEventSummary;
use crabgent_core::error::KernelError;
use crabgent_core::hook::Event;
use crabgent_core::message::Message;
use crabgent_core::run::RunRequest;
use crabgent_core::{
    ContentBlock, InvalidSubjectError, Kernel, MemoryScope, ModelTarget, Owner, RunId, Subject,
};
use crabgent_store::records::CronJob;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::error::CronError;
use crate::observer::{
    CronActivityEvent, CronActivityKind, CronJobActivityMeta, CronObserver, notify_observers,
};

/// Context passed to a [`CronExecutor`] for one cron-job run.
#[derive(Clone)]
pub struct CronExecCtx<'a> {
    /// The claimed cron job.
    pub job: &'a CronJob,
    /// Kernel handle. Cloned `Arc` so the executor can keep it alive past
    /// the borrow of the rest of the context.
    pub kernel: Arc<Kernel>,
    /// Effective prompt (after pre-processor augmentation).
    pub prompt: &'a str,
    /// Cooperative-shutdown token. The executor must propagate this into
    /// `Kernel::run` so long-running providers/tools can abort.
    pub cancel: CancellationToken,
}

/// Outcome of one cron-job run. Exactly one of `final_text` and `error`
/// is `Some` for a non-default value.
#[derive(Debug, Clone, Default)]
pub struct CronExecResult {
    /// Final assistant text on success.
    pub final_text: Option<String>,
    /// Error description on failure (kernel-level or executor-captured).
    pub error: Option<String>,
}

impl CronExecResult {
    /// `true` when the run produced final text and no captured error.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.final_text.is_some() && self.error.is_none()
    }
}

/// Runs the cron job and returns its result.
#[async_trait]
pub trait CronExecutor: Send + Sync {
    /// Execute one cron-job run.
    async fn execute(&self, ctx: CronExecCtx<'_>) -> Result<CronExecResult, CronError>;
}

type SubjectResolver = Arc<dyn Fn(&CronJob) -> Result<Subject, InvalidSubjectError> + Send + Sync>;

const ATTR_CHANNEL: &str = "channel";
const ATTR_CONV: &str = "conv";
const ATTR_AGENT: &str = "agent";
const ATTR_CHANNEL_KIND: &str = "channel_kind";
const CRON_BLOCKED_DELIVERY_TOOLS: &[&str] = &["channel_send", "notify_user"];

/// Default executor that drives `kernel.run()` for each cron job.
pub struct KernelCronExecutor {
    default_model: ModelTarget,
    default_system_prompt: Option<String>,
    max_turns: Option<u32>,
    resolve_subject: SubjectResolver,
    observers: Vec<Arc<dyn CronObserver>>,
}

impl KernelCronExecutor {
    /// Construct a new executor with `default_model` used as the config
    /// default. A job `model_override`, when present, is forwarded as the
    /// explicit model layer.
    pub fn new(default_model: impl Into<ModelTarget>) -> Self {
        Self {
            default_model: default_model.into(),
            default_system_prompt: None,
            max_turns: None,
            resolve_subject: default_subject_resolver(),
            observers: Vec::new(),
        }
    }

    /// Set a default system prompt applied to every cron run that does not
    /// carry its own (cron jobs in v1 do not).
    #[must_use]
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.default_system_prompt = Some(prompt.into());
        self
    }

    /// Cap iterations per cron run. `None` means use the kernel default.
    #[must_use]
    pub const fn with_max_turns(mut self, n: u32) -> Self {
        self.max_turns = Some(n);
        self
    }

    /// Register a compact progress observer for kernel streaming events.
    #[must_use]
    pub fn with_observer(mut self, observer: Arc<dyn CronObserver>) -> Self {
        self.observers.push(observer);
        self
    }

    /// Override the subject resolution. Receives the cron job and must
    /// produce a [`Subject`] for the policy hook.
    #[must_use]
    pub fn with_subject_resolver<F>(mut self, f: F) -> Self
    where
        F: Fn(&CronJob) -> Subject + Send + Sync + 'static,
    {
        let resolver = Arc::new(f);
        self.resolve_subject = Arc::new(move |job| Ok(resolver(job)));
        self
    }

    /// Override subject resolution with a fallible resolver for user-supplied ids.
    #[must_use]
    pub fn with_fallible_subject_resolver<F>(mut self, f: F) -> Self
    where
        F: Fn(&CronJob) -> Result<Subject, InvalidSubjectError> + Send + Sync + 'static,
    {
        self.resolve_subject = Arc::new(f);
        self
    }

    fn build_request(&self, job: &CronJob, prompt: &str) -> Result<RunRequest, CronError> {
        let subject = subject_with_scope_attrs((self.resolve_subject)(job)?, &job.scope);
        Ok(RunRequest {
            pause: None,
            run_id: RunId::new(),
            subject,
            model: self.default_model.clone(),
            explicit_model: job.model_override.clone().map(Into::into),
            session_model_override: None,
            fallbacks: Vec::new(),
            messages: vec![Message::User {
                content: vec![ContentBlock::Text {
                    text: prompt.to_owned(),
                }],
                timestamp: None,
            }],
            system_prompt: self.default_system_prompt.clone(),
            max_turns: self.max_turns,
            temperature: None,
            max_tokens: None,
            cancel_reason: None,
            reasoning_effort: job.reasoning_effort_override,
            web_search: ::crabgent_core::types::WebSearchConfig::default(),
        })
    }
}

#[async_trait]
impl CronExecutor for KernelCronExecutor {
    async fn execute(&self, ctx: CronExecCtx<'_>) -> Result<CronExecResult, CronError> {
        let req = self.build_request(ctx.job, ctx.prompt)?;
        let meta = CronJobActivityMeta::from_job(ctx.job, ctx.prompt, Some(req.run_id.clone()));
        let stream =
            ctx.kernel
                .run_streaming_with_tool_filter(req, Some(&ctx.cancel), cron_allows_tool);
        tokio::pin!(stream);
        while let Some(item) = stream.next().await {
            match item {
                Ok(Event::Final(text)) => {
                    self.observe_kernel(meta.clone(), &Event::Final(text.clone()))
                        .await;
                    return Ok(CronExecResult {
                        final_text: Some(text),
                        error: None,
                    });
                }
                Ok(event) => self.observe_kernel(meta.clone(), &event).await,
                Err(error) => {
                    return Ok(CronExecResult {
                        final_text: None,
                        error: Some(error.to_string()),
                    });
                }
            }
        }
        Ok(CronExecResult {
            final_text: None,
            error: Some(missing_final_error()),
        })
    }
}

impl KernelCronExecutor {
    async fn observe_kernel(&self, meta: CronJobActivityMeta, event: &Event) {
        notify_observers(
            &self.observers,
            CronActivityEvent::new(
                Some(meta),
                CronActivityKind::Kernel(ActivityEventSummary::from_event(event)),
            ),
        )
        .await;
    }
}

fn missing_final_error() -> String {
    KernelError::Internal("run stream ended without final event".into()).to_string()
}

fn cron_allows_tool(name: &str) -> bool {
    !CRON_BLOCKED_DELIVERY_TOOLS.contains(&name)
}

/// Resolve the cron subject identity from job scope.
fn default_subject_resolver() -> SubjectResolver {
    Arc::new(|job: &CronJob| {
        let id = job.scope.owner.as_ref().map_or("cron", Owner::as_str);
        Subject::try_new(id)
    })
}

/// Stamp persisted scope onto the cron run subject.
///
/// The `agent` attr is required for fan-out session disambiguation when
/// multiple cron agents share the same owner/thread/recipient conversation.
/// `channel` and `conv` keep cron runs tied to the stored delivery target.
fn subject_with_scope_attrs(mut subject: Subject, scope: &MemoryScope) -> Subject {
    if let Some(channel) = scope.channel.as_deref() {
        subject = subject.with_attr(ATTR_CHANNEL, channel);
    }
    if let Some(conv) = scope.conv.as_deref() {
        subject = subject.with_attr(ATTR_CONV, conv);
    }
    if let Some(agent) = scope.agent.as_deref() {
        subject = subject.with_attr(ATTR_AGENT, agent);
    }
    if let Some(kind) = scope.kind.as_deref() {
        subject = subject.with_attr(ATTR_CHANNEL_KIND, kind);
    }
    subject
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod executor_tests;
