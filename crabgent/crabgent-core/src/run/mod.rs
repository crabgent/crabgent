//! Public run-loop API.

use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use futures::{Stream, StreamExt};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tokio::time;
use tokio_util::sync::CancellationToken;

use crate::error::KernelError;
use crate::hook::{CancelReason, Event, RunCtx};
use crate::hook_chain::HookChain;
use crate::kernel::{Defaults, Kernel};
use crate::message::Message;
use crate::model::{ModelId, ModelTarget, ReasoningEffort};
use crate::run_id::RunId;
use crate::subject::Subject;
use crate::types::WebSearchConfig;

mod audio_check;
mod fallback;
mod model_resolution;
mod shared;
mod stream;
mod tools_check;
mod vision_check;
mod web_search_check;

const STREAM_DROP_ABORT_GRACE: Duration = Duration::from_millis(250);

/// Request to the kernel for one `Kernel::run()` invocation.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// Caller-supplied unique run identifier.
    pub run_id: RunId,
    /// Subject identity for policy decisions.
    pub subject: Subject,
    /// Configured default model selector. Unqualified ids resolve when exactly
    /// one provider advertises them; provider-qualified targets pin the run to
    /// a concrete provider/model pair.
    ///
    /// [`ModelRegistry`]: crate::model::ModelRegistry
    pub model: ModelTarget,
    /// Explicit per-run model selector. When present, this wins over session
    /// and global overrides.
    pub explicit_model: Option<ModelTarget>,
    /// Session-scoped model override loaded by a caller that has an active
    /// session context.
    pub session_model_override: Option<ModelId>,
    /// Provider/model targets to try after the primary model when the
    /// provider returns a fallback-eligible error.
    pub fallbacks: Vec<ModelTarget>,
    /// Initial conversation messages.
    pub messages: Vec<Message>,
    /// Optional system prompt.
    pub system_prompt: Option<String>,
    /// Override the kernel's default `max_turns`.
    pub max_turns: Option<u32>,
    /// Optional sampling temperature.
    pub temperature: Option<f32>,
    /// Optional max output tokens.
    pub max_tokens: Option<u32>,
    /// Optional cancellation-cause discriminator. Channel adapters or
    /// embedders may pass a shared `Arc<OnceLock<CancelReason>>` so the
    /// kernel installs it on `RunCtx.cancel_reason`; hooks and `on_stop`
    /// observers then distinguish user-typed stop-pattern cancel from
    /// hook-driven graceful-stop cancel. `None` leaves
    /// `RunCtx.cancel_reason` at the fresh empty cell created by the
    /// run loop.
    pub cancel_reason: Option<Arc<OnceLock<CancelReason>>>,
    /// Optional cooperative pause signal. When the token fires, the run
    /// stops at the next safe boundary (turn start or between tool
    /// dispatches) with `Outcome::Paused` instead of being interrupted
    /// mid-flight. The per-run pause token is derived as a child of this
    /// token (when supplied) and additionally observes
    /// [`Kernel::request_pause`], mirroring how per-run cancellation is
    /// derived. `None` leaves the run pausable only through the
    /// kernel-wide pause signal.
    ///
    /// [`Kernel::request_pause`]: crate::Kernel::request_pause
    pub pause: Option<CancellationToken>,
    /// Per-run `reasoning_effort` override. When `Some`, it wins over the
    /// model's default capability. When `None`, the run falls back to
    /// `ModelCapabilities::reasoning_effort` inside `request_for_attempt`.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Hosted web-search configuration forwarded to the provider.
    /// Default is disabled (`WebSearchConfig::default()`).
    pub web_search: WebSearchConfig,
}

impl Kernel {
    /// Run the agent loop until the LLM stops or `max_turns` is reached.
    /// Returns the final assistant text.
    pub async fn run(
        &self,
        req: RunRequest,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, KernelError> {
        let stream = self.run_streaming(req, cancel);
        tokio::pin!(stream);
        while let Some(item) = stream.next().await {
            match item? {
                Event::Final(text) => return Ok(text),
                Event::Token(_)
                | Event::Reasoning(_)
                | Event::ToolCallStarted(_)
                | Event::ToolCallCompleted { .. }
                | Event::Notification(_)
                | Event::ServerToolResult { .. }
                | Event::AttemptFailed { .. } => {}
            }
        }
        Err(KernelError::Internal(
            "run stream ended without final event".into(),
        ))
    }

    /// Run the agent loop and stream events to the caller. The returned
    /// stream yields `Ok(Event)` for every token, reasoning fragment,
    /// tool-call lifecycle, or notification, ending with
    /// `Ok(Event::Final(text))` on success or
    /// `Err(KernelError)` on failure. The driver runs in a background
    /// task; callers may drop the stream to cancel the background driver.
    /// A short watchdog aborts the driver if cooperative cancellation does
    /// not finish.
    pub fn run_streaming(
        &self,
        req: RunRequest,
        cancel: Option<&CancellationToken>,
    ) -> impl Stream<Item = Result<Event, KernelError>> + Send + 'static {
        self.run_streaming_with_tool_filter(req, cancel, |_| true)
    }

    /// Run the agent loop with a per-run tool filter. Tools rejected by
    /// `include_tool` are neither advertised to the provider nor executable if
    /// a provider still returns such a tool call.
    pub fn run_streaming_with_tool_filter<F>(
        &self,
        req: RunRequest,
        cancel: Option<&CancellationToken>,
        include_tool: F,
    ) -> impl Stream<Item = Result<Event, KernelError>> + Send + 'static
    where
        F: Fn(&str) -> bool,
    {
        let max = req.max_turns.unwrap_or_else(|| self.defaults().max_turns);
        let RunRequest {
            run_id,
            subject,
            model,
            explicit_model,
            session_model_override,
            fallbacks,
            messages,
            system_prompt,
            max_turns: _,
            temperature,
            max_tokens,
            cancel_reason,
            pause,
            reasoning_effort,
            web_search,
        } = req;
        let child_cancel = derive_per_run_cancel(cancel, &self.shutdown_token);
        let child_pause = derive_per_run_pause(pause.as_ref(), &self.pause_token, &child_cancel);
        let reason_cell = cancel_reason.unwrap_or_else(|| Arc::new(OnceLock::new()));
        let run_ctx = RunCtx::new(run_id, subject)
            .with_cancel(child_cancel.clone())
            .with_cancel_reason(reason_cell);
        let tools = self
            .tools()
            .iter()
            .filter(|tool| include_tool(tool.name()))
            .cloned()
            .collect();
        let cfg = stream::StreamCfg {
            providers: self.providers.clone(),
            policy: Arc::clone(self.policy()),
            tools,
            hooks: self.hooks().clone(),
            run_ctx,
            max_turns: max,
            model,
            explicit_model,
            session_model_override,
            fallbacks,
            system_prompt,
            temperature,
            max_tokens,
            reasoning_effort,
            web_search,
            models: Arc::clone(&self.models),
            global_override_store: Arc::clone(self.global_model_override_store()),
            global_reasoning_effort_override_store: Arc::clone(
                self.global_reasoning_effort_override_store(),
            ),
            cancel: child_cancel.clone(),
            pause: child_pause,
            shutdown: self.shutdown_token.clone(),
        };
        let (tx, rx) = mpsc::channel::<Result<Event, KernelError>>(Defaults::STREAM_BUFFER_SIZE);
        let abort = spawn_driver(
            &self.running,
            &self.shutdown_token,
            self.hooks(),
            cfg,
            messages,
            tx,
        );
        RunStream {
            rx,
            abort,
            cancel: child_cancel,
            _running: Arc::clone(&self.running),
        }
    }
}

/// Derive the per-run cancel token. When the caller supplies a token,
/// the per-run token is its direct child so caller cancellation is
/// synchronous (no scheduling race), and a watcher task forwards kernel
/// shutdown into the per-run token. When the caller passes `None`, the
/// per-run token is a direct child of `shutdown_token`. A caller token
/// that is already cancelled at entry yields a per-run token that is
/// also already cancelled; the driver task observes that on its first
/// poll and short-circuits via the existing run-loop cancel path,
/// keeping `on_stop:cancelled` semantics intact.
fn derive_per_run_cancel(
    cancel: Option<&CancellationToken>,
    shutdown_token: &CancellationToken,
) -> CancellationToken {
    match cancel {
        Some(caller) => {
            let per_run = caller.child_token();
            if !caller.is_cancelled() {
                let shutdown = shutdown_token.clone();
                let pr = per_run.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        () = shutdown.cancelled() => pr.cancel(),
                        () = pr.cancelled() => {}
                    }
                });
            }
            per_run
        }
        None => shutdown_token.child_token(),
    }
}

/// Derive the per-run pause token, mirroring [`derive_per_run_cancel`]:
/// a caller-supplied pause token (e.g. an executor-owned pause root)
/// stays synchronous via a direct child, with a watcher forwarding the
/// kernel-wide pause signal; without a caller token the per-run token is
/// a direct child of the kernel pause root. Unlike cancellation, the
/// resulting token is only polled at safe run-loop boundaries.
///
/// The watcher additionally exits when the per-run cancel token fires.
/// That token is cancelled by `RunStream::drop` (every stream is
/// eventually dropped), so a run that never pauses does not leak a
/// parked watcher task per run.
fn derive_per_run_pause(
    pause: Option<&CancellationToken>,
    pause_token: &CancellationToken,
    run_cancel: &CancellationToken,
) -> CancellationToken {
    match pause {
        Some(caller) => {
            let per_run = caller.child_token();
            if !caller.is_cancelled() {
                let root = pause_token.clone();
                let pr = per_run.clone();
                let run_done = run_cancel.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        () = root.cancelled() => pr.cancel(),
                        () = pr.cancelled() => {}
                        () = run_done.cancelled() => {}
                    }
                });
            }
            per_run
        }
        None => pause_token.child_token(),
    }
}

fn spawn_driver(
    running: &Arc<std::sync::Mutex<JoinSet<()>>>,
    shutdown_token: &CancellationToken,
    hooks: &HookChain,
    cfg: stream::StreamCfg,
    messages: Vec<Message>,
    tx: mpsc::Sender<Result<Event, KernelError>>,
) -> Option<AbortHandle> {
    let mut guard = running
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Reap any tracked-but-finished drivers before adding a new one so
    // the JoinSet does not grow unboundedly in long-lived adapters.
    reap_finished_drivers(&mut guard, hooks);
    if shutdown_token.is_cancelled() {
        // Receiver is still owned by the caller's RunStream; the send
        // succeeds unless the caller has already dropped it.
        drop(tx.try_send(Err(KernelError::ShuttingDown)));
        return None;
    }
    Some(guard.spawn(stream::drive_stream(cfg, messages, tx)))
}

pub(crate) fn reap_finished_drivers(running: &mut JoinSet<()>, hooks: &HookChain) {
    while let Some(joined) = running.try_join_next() {
        if let Err(err) = joined
            && err.is_panic()
        {
            let hooks = hooks.clone();
            tokio::spawn(async move {
                hooks.apply_on_kernel_shutdown_task_panic(&err).await;
            });
        }
    }
}

struct RunStream {
    rx: mpsc::Receiver<Result<Event, KernelError>>,
    abort: Option<AbortHandle>,
    cancel: CancellationToken,
    /// Keeps the kernel-owned `JoinSet` alive past `Kernel::drop`. Without
    /// this clone the `JoinSet` would drop with the kernel and
    /// `JoinSet::drop` would abort every active driver, silently ending
    /// any still-pinned `RunStream`. Holding the `Arc` here keeps the set
    /// (and therefore each running driver) alive until the consumer
    /// drops the stream.
    _running: Arc<std::sync::Mutex<JoinSet<()>>>,
}

impl Stream for RunStream {
    type Item = Result<Event, KernelError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(item @ (Ok(Event::Final(_)) | Err(_)))) => {
                self.abort = None;
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                self.abort = None;
                Poll::Ready(None)
            }
            other => other,
        }
    }
}

impl Drop for RunStream {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(abort) = self.abort.take() {
            abort_after_grace(abort);
        }
    }
}

fn abort_after_grace(abort: AbortHandle) {
    let Ok(handle) = Handle::try_current() else {
        abort.abort();
        return;
    };
    drop(handle.spawn(async move {
        time::sleep(STREAM_DROP_ABORT_GRACE).await;
        abort.abort();
    }));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use tokio::task::{JoinError, JoinSet};
    use tokio::time::sleep;

    use super::reap_finished_drivers;
    use crate::hook::Hook;
    use crate::hook_chain::HookChain;

    struct PanicHook {
        observed: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Hook for PanicHook {
        async fn on_kernel_shutdown_task_panic(&self, err: &JoinError) {
            assert!(err.is_panic());
            self.observed.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn reap_finished_drivers_reports_panics_to_hooks() {
        let observed = Arc::new(AtomicUsize::new(0));
        let mut hooks = HookChain::new();
        hooks.push(PanicHook {
            observed: Arc::clone(&observed),
        });
        let mut running = JoinSet::new();
        running.spawn(async { panic!("driver panic") });

        for _ in 0..10 {
            reap_finished_drivers(&mut running, &hooks);
            if observed.load(Ordering::SeqCst) == 1 {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(observed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reap_finished_drivers_ignores_aborted_tasks() {
        let observed = Arc::new(AtomicUsize::new(0));
        let mut hooks = HookChain::new();
        hooks.push(PanicHook {
            observed: Arc::clone(&observed),
        });
        let mut running = JoinSet::new();
        let handle = running.spawn(async {
            sleep(Duration::from_mins(1)).await;
        });
        handle.abort();

        for _ in 0..10 {
            reap_finished_drivers(&mut running, &hooks);
            sleep(Duration::from_millis(10)).await;
        }

        assert_eq!(observed.load(Ordering::SeqCst), 0);
    }
}
