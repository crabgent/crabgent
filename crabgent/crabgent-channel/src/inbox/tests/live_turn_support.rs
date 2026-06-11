//! Shared stubs and helpers for the `live_turn` delivery test siblings.
//!
//! Split out of `live_turn.rs` to keep each test file under the LOC cap.
//! Both `live_turn.rs` and `live_turn_progress.rs` glob-import this module.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::error::{ProviderError, ToolError};
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{EventStream, Provider, ProviderCapabilities, ProviderEvent};
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, ToolCall, ToolResult, Usage};
use crabgent_core::{Kernel, ModelInfo, RunCtx};
use futures::stream;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::channel::Channel;
use crate::inbox::KernelChannelInbox;
use crate::sink::{ChannelRouter, ChannelSink};
use crate::test_support::RecordingChannel;

#[derive(Clone)]
pub(super) struct ScriptedProvider {
    responses: Arc<Mutex<VecDeque<LlmResponse>>>,
    seen_prompts: Arc<Mutex<Vec<Option<String>>>>,
}

impl ScriptedProvider {
    pub(super) fn new(responses: impl IntoIterator<Item = LlmResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            seen_prompts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(super) fn seen_prompts(&self) -> Vec<Option<String>> {
        self.seen_prompts
            .lock()
            .expect("mutex should not be poisoned")
            .clone()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.seen_prompts
            .lock()
            .expect("mutex should not be poisoned")
            .push(req.system_prompt.clone());
        self.responses
            .lock()
            .expect("mutex should not be poisoned")
            .pop_front()
            .ok_or_else(|| ProviderError::Other("script exhausted".to_owned()))
    }

    fn name(&self) -> &'static str {
        "script"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            streaming: true,
            thinking: true,
            system_prompt: true,
            ..Default::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "script")]
    }
}

pub(super) struct ReasoningProvider {
    pub(super) raw_reasoning: &'static str,
}

#[async_trait]
impl Provider for ReasoningProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        Ok(text_response("final answer"))
    }

    async fn stream(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<EventStream, ProviderError> {
        Ok(Box::pin(stream::iter([
            Ok(ProviderEvent::ReasoningDelta(self.raw_reasoning.to_owned())),
            Ok(ProviderEvent::TextDelta("final answer".to_owned())),
            Ok(ProviderEvent::Stop(StopReason::EndTurn)),
        ])))
    }

    fn name(&self) -> &'static str {
        "reasoning"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            streaming: true,
            thinking: true,
            ..Default::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "reasoning")]
    }
}

#[derive(Clone)]
pub(super) struct BlockingProvider {
    started: Arc<Notify>,
    cancelled: Arc<Notify>,
    started_count: Arc<AtomicUsize>,
    cancelled_count: Arc<AtomicUsize>,
}

impl BlockingProvider {
    pub(super) fn new() -> Self {
        Self {
            started: Arc::new(Notify::new()),
            cancelled: Arc::new(Notify::new()),
            started_count: Arc::new(AtomicUsize::new(0)),
            cancelled_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(super) async fn wait_started(&self) {
        if self.started_count.load(Ordering::SeqCst) > 0 {
            return;
        }
        self.started.notified().await;
    }
}

#[async_trait]
impl Provider for BlockingProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.started_count.fetch_add(1, Ordering::SeqCst);
        self.started.notify_waiters();
        if let Some(cancel) = cancel {
            cancel.cancelled().await;
        }
        self.cancelled_count.fetch_add(1, Ordering::SeqCst);
        self.cancelled.notify_waiters();
        Err(ProviderError::Cancelled)
    }

    fn name(&self) -> &'static str {
        "blocking"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "blocking")]
    }
}

#[derive(Clone)]
pub(super) struct StaticTool {
    pub(super) name: &'static str,
    pub(super) result: ToolResult,
}

#[async_trait]
impl Tool for StaticTool {
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
        Ok(self.result.output.clone())
    }

    async fn execute_result(&self, _args: Value, _ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        Ok(self.result.clone())
    }
}

pub(super) fn text_response(text: &str) -> LlmResponse {
    LlmResponse {
        text: text.to_owned(),
        tool_calls: Vec::new(),
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        model: "claude-haiku-4-5".into(),
    }
}

pub(super) fn tool_response(calls: Vec<ToolCall>) -> LlmResponse {
    LlmResponse {
        text: String::new(),
        tool_calls: calls,
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        model: "claude-haiku-4-5".into(),
    }
}

pub(super) fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: format!("call-{name}"),
        name: name.to_owned(),
        args,
        thought_signature: None,
    }
}

pub(super) fn sink_for(channel: &Arc<RecordingChannel>) -> Arc<dyn ChannelSink> {
    let trait_obj: Arc<dyn Channel> = Arc::clone(channel) as _;
    Arc::new(ChannelRouter::new().with_channel(trait_obj))
}

pub(super) fn kernel_with_provider(provider: impl Provider + 'static) -> Arc<Kernel> {
    Arc::new(
        Kernel::builder()
            .provider(provider)
            .policy(AllowAllPolicy)
            .build(),
    )
}

pub(super) fn inbox_with_sink(
    kernel: Arc<Kernel>,
    sink: Arc<dyn ChannelSink>,
) -> KernelChannelInbox {
    KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
        .with_live_turn_delivery(sink)
}

pub(super) async fn wait_for_sent(channel: &RecordingChannel, count: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if channel.sent_count() == count {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("send count reached expected value");
}

pub(super) async fn wait_for_edit(channel: &RecordingChannel, count: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if channel.edit_count() >= count {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("edit count reached expected value");
}
