//! `HookChain`: FIFO list of hooks, applied as a pipeline per event.

use std::future::Future;
use std::sync::Arc;

use crate::error::KernelError;
use tokio::task::JoinError;
use tokio_util::sync::CancellationToken;

use crate::hook::{Decision, Event, Hook, Outcome, RunCtx};
use crate::message::Message;
use crate::types::{LlmRequest, LlmResponse, Notification, ToolCall, ToolResult};

macro_rules! apply_state_chain {
    ($hooks:expr, $initial:expr, [$($capture:ident),* $(,)?], |$hook:ident, $state:ident| $body:expr) => {{
        $(let $capture = $capture.clone();)*
        apply_chain($hooks, $initial, |$hook, $state| {
            $(let $capture = $capture.clone();)*
            async move {
                let decision = $body.await;
                ($state, decision)
            }
        })
        .await
    }};
}

macro_rules! apply_unit_chain_with {
    ($hooks:expr, [$($capture:ident),* $(,)?], |$hook:ident| $body:expr) => {{
        $(let $capture = $capture.clone();)*
        apply_unit_chain($hooks, |$hook| {
            $(let $capture = $capture.clone();)*
            async move { $body.await }
        })
        .await
    }};
}

/// FIFO chain of registered hooks. Applied as a pipeline per event.
#[derive(Default, Clone)]
pub struct HookChain {
    hooks: Vec<Arc<dyn Hook>>,
}

impl HookChain {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push<H: Hook + 'static>(&mut self, hook: H) {
        self.hooks.push(Arc::new(hook));
    }

    pub fn push_arc(&mut self, hook: Arc<dyn Hook>) {
        self.hooks.push(hook);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }

    pub async fn apply_on_session_start(&self, ctx: &RunCtx) -> Result<(), KernelError> {
        let ctx = ctx.clone();
        apply_unit_chain_with!(&self.hooks, [ctx], |hook| hook.on_session_start(&ctx))
    }

    pub async fn apply_on_user_prompt_submit(
        &self,
        msgs: &[Message],
        ctx: &RunCtx,
    ) -> Result<Vec<Message>, KernelError> {
        let ctx = ctx.clone();
        apply_state_chain!(&self.hooks, msgs.to_vec(), [ctx], |hook, state| {
            hook.on_user_prompt_submit(&state, &ctx)
        })
    }

    pub async fn apply_before_llm(
        &self,
        req: &LlmRequest,
        ctx: &RunCtx,
    ) -> Result<LlmRequest, KernelError> {
        let ctx = ctx.clone();
        apply_state_chain!(&self.hooks, req.clone(), [ctx], |hook, state| {
            hook.before_llm(&state, &ctx)
        })
    }

    pub async fn apply_after_llm(
        &self,
        req: &LlmRequest,
        resp: &LlmResponse,
        ctx: &RunCtx,
    ) -> Result<LlmResponse, KernelError> {
        let req = Arc::new(req.clone());
        let ctx = ctx.clone();
        apply_state_chain!(&self.hooks, resp.clone(), [req, ctx], |hook, state| {
            hook.after_llm(&req, &state, &ctx)
        })
    }

    pub async fn apply_before_tool(
        &self,
        call: &ToolCall,
        ctx: &RunCtx,
    ) -> Result<ToolCall, KernelError> {
        let ctx = ctx.clone();
        apply_state_chain!(&self.hooks, call.clone(), [ctx], |hook, state| {
            hook.before_tool(&state, &ctx)
        })
    }

    pub async fn apply_after_tool(
        &self,
        call: &ToolCall,
        result: &ToolResult,
        ctx: &RunCtx,
    ) -> Result<ToolResult, KernelError> {
        let call = Arc::new(call.clone());
        let ctx = ctx.clone();
        apply_state_chain!(&self.hooks, result.clone(), [call, ctx], |hook, state| {
            hook.after_tool(&call, &state, &ctx)
        })
    }

    pub async fn apply_on_message(
        &self,
        msgs: &[Message],
        ctx: &RunCtx,
    ) -> Result<Vec<Message>, KernelError> {
        let ctx = ctx.clone();
        apply_state_chain!(&self.hooks, msgs.to_vec(), [ctx], |hook, state| {
            hook.on_message(&state, &ctx)
        })
    }

    pub async fn apply_on_event(&self, ev: &Event, ctx: &RunCtx) -> Result<Event, KernelError> {
        let ctx = ctx.clone();
        apply_state_chain!(&self.hooks, ev.clone(), [ctx], |hook, state| {
            hook.on_event(&state, &ctx)
        })
    }

    pub async fn apply_pre_compact(
        &self,
        msgs: &[Message],
        ctx: &RunCtx,
    ) -> Result<Option<Vec<Message>>, KernelError> {
        let ctx = ctx.clone();
        let mut state = msgs.to_vec();
        let mut replaced = false;
        for hook in &self.hooks {
            match hook.pre_compact(&state, &ctx).await {
                Decision::Continue => {}
                Decision::Replace(next) => {
                    state = next;
                    replaced = true;
                }
                Decision::Deny(reason) => return Err(KernelError::HookDenied { reason }),
            }
        }
        Ok(replaced.then_some(state))
    }

    pub async fn apply_on_stop(&self, ctx: &RunCtx, outcome: &Outcome) {
        apply_terminal(&self.hooks, |hook| async move {
            hook.on_stop(ctx, outcome).await;
        })
        .await;
    }

    /// Apply notification-specific hooks.
    ///
    /// The run-loop first applies `on_event` to every event. If the resulting
    /// event is a notification, it also applies this notification surface.
    pub async fn apply_on_notification(
        &self,
        note: &Notification,
        ctx: &RunCtx,
    ) -> Result<(), KernelError> {
        let note = note.clone();
        let ctx = ctx.clone();
        apply_unit_chain_with!(&self.hooks, [note, ctx], |hook| hook
            .on_notification(&note, &ctx))
    }

    pub async fn apply_on_error(&self, ctx: &RunCtx, err: &KernelError) {
        apply_terminal(&self.hooks, |hook| async move {
            hook.on_error(ctx, err).await;
        })
        .await;
    }

    pub async fn apply_on_kernel_shutdown(&self, token: &CancellationToken) {
        apply_terminal(&self.hooks, |hook| async move {
            hook.on_kernel_shutdown(token).await;
        })
        .await;
    }

    pub async fn apply_on_kernel_shutdown_task_panic(&self, err: &JoinError) {
        apply_terminal(&self.hooks, |hook| async move {
            hook.on_kernel_shutdown_task_panic(err).await;
        })
        .await;
    }
}

async fn apply_chain<T, F, Fut>(
    hooks: &[Arc<dyn Hook>],
    initial: T,
    mut apply: F,
) -> Result<T, KernelError>
where
    F: FnMut(Arc<dyn Hook>, T) -> Fut,
    Fut: Future<Output = (T, Decision<T>)>,
{
    let mut state = initial;
    for hook in hooks {
        let (previous, decision) = apply(Arc::clone(hook), state).await;
        match decision {
            Decision::Continue => state = previous,
            Decision::Replace(next) => state = next,
            Decision::Deny(reason) => return Err(KernelError::HookDenied { reason }),
        }
    }
    Ok(state)
}

async fn apply_unit_chain<F, Fut>(hooks: &[Arc<dyn Hook>], mut apply: F) -> Result<(), KernelError>
where
    F: FnMut(Arc<dyn Hook>) -> Fut,
    Fut: Future<Output = Decision<()>>,
{
    for hook in hooks {
        if let Decision::Deny(reason) = apply(Arc::clone(hook)).await {
            return Err(KernelError::HookDenied { reason });
        }
    }
    Ok(())
}

/// Run a fire-and-forget terminal callback against every hook in FIFO order.
///
/// Terminal callbacks (`on_stop`, `on_error`, `on_kernel_shutdown`,
/// `on_kernel_shutdown_task_panic`) have no `Decision`, never short-circuit,
/// and thread no state. They differ only in the hook method invoked, so they
/// share this applicator.
async fn apply_terminal<F, Fut>(hooks: &[Arc<dyn Hook>], mut apply: F)
where
    F: FnMut(Arc<dyn Hook>) -> Fut,
    Fut: Future<Output = ()>,
{
    for hook in hooks {
        apply(Arc::clone(hook)).await;
    }
}
