//! Integration tests for `KernelChannelInbox::with_formatting_hint`.
//!
//! These tests drive `receive` end-to-end through a stub provider that
//! records the composed `system_prompt` so we can assert the order and
//! presence of the formatting hint without touching crate-private
//! helpers.

use crabgent_core::RunCtx;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    ChannelInbox, InboundEvent, KernelChannelInbox, MessageRef, Participant, ParticipantRole,
};
use crabgent_core::Kernel;
use crabgent_core::ModelInfo;
use crabgent_core::error::ProviderError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, Usage};
use tokio_util::sync::CancellationToken;

const HINT: &str = "<output_format>\nuse FOO\n</output_format>";

#[derive(Clone, Default)]
struct RecordingProvider {
    prompts: Arc<Mutex<Vec<Option<String>>>>,
}

#[async_trait]
impl Provider for RecordingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.prompts
            .lock()
            .expect("recording mutex poisoned")
            .push(req.system_prompt.clone());
        Ok(LlmResponse {
            text: "ok".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "recording"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "recording")]
    }
}

fn build_kernel(provider: RecordingProvider) -> Arc<Kernel> {
    Arc::new(
        Kernel::builder()
            .provider(provider)
            .policy(AllowAllPolicy)
            .build(),
    )
}

fn build_event() -> InboundEvent {
    InboundEvent {
        channel: "slack".to_owned(),
        conv: Owner::new("slack:T1/D1"),
        kind: None,
        from: Participant::new("U1", ParticipantRole::Human),
        message: MessageRef::top_level("slack", Owner::new("slack:T1/D1"), "ts:1"),
        body: "hi".to_owned(),
        attachments: vec![],
        timestamp: Utc::now(),
    }
}

async fn captured_prompt(
    inbox: KernelChannelInbox,
    provider: &RecordingProvider,
) -> Option<String> {
    inbox.receive(build_event()).await.expect("receive ok");
    tokio::time::sleep(Duration::from_millis(150)).await;
    let recorded = provider
        .prompts
        .lock()
        .expect("recording mutex poisoned")
        .clone();
    assert_eq!(recorded.len(), 1, "expected exactly one provider call");
    recorded.into_iter().next().expect("one prompt captured")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_appends_formatting_hint_after_conversation_hint() {
    let provider = RecordingProvider::default();
    let kernel = build_kernel(provider.clone());
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
        .with_system_prompt("base prompt")
        .with_formatting_hint(HINT);

    let prompt = captured_prompt(inbox, &provider)
        .await
        .expect("system_prompt present");

    let conv_idx = prompt
        .find("Conversation context")
        .expect("conversation hint present");
    let fmt_idx = prompt.find(HINT).expect("formatting hint present");
    assert!(
        conv_idx < fmt_idx,
        "formatting hint must follow conversation hint: {prompt:?}"
    );
    let base_idx = prompt.find("base prompt").expect("base prompt present");
    assert!(
        base_idx < conv_idx,
        "conversation hint must follow base prompt: {prompt:?}"
    );
    assert!(
        prompt.contains("\n\n"),
        "parts joined by blank line: {prompt:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_without_conversation_hint_still_appends_formatting_hint() {
    let provider = RecordingProvider::default();
    let kernel = build_kernel(provider.clone());
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
        .with_system_prompt("base")
        .without_conversation_hint()
        .with_formatting_hint(HINT);

    let prompt = captured_prompt(inbox, &provider)
        .await
        .expect("system_prompt present");

    assert!(
        prompt.ends_with(&format!("base\n\n{HINT}")),
        "base and formatting hint order changed: {prompt:?}"
    );
    assert!(
        !prompt.contains("Conversation context"),
        "conversation hint must be suppressed: {prompt:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_without_formatting_hint_is_backwards_compatible() {
    let provider = RecordingProvider::default();
    let kernel = build_kernel(provider.clone());
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
        .with_system_prompt("base prompt");

    let prompt = captured_prompt(inbox, &provider)
        .await
        .expect("system_prompt present");

    let base_idx = prompt.find("base prompt").expect("base prompt present");
    let conv_idx = prompt
        .find("Conversation context")
        .expect("conversation hint present");
    assert!(
        base_idx < conv_idx,
        "conversation hint must follow base prompt: {prompt:?}"
    );
    assert!(
        !prompt.contains(HINT),
        "formatting hint must be absent: {prompt:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_with_only_formatting_hint() {
    let provider = RecordingProvider::default();
    let kernel = build_kernel(provider.clone());
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
        .without_conversation_hint()
        .with_formatting_hint(HINT);

    let prompt = captured_prompt(inbox, &provider)
        .await
        .expect("system_prompt present");

    assert!(
        prompt.ends_with(HINT),
        "formatting hint must remain the final prompt segment: {prompt:?}"
    );
    assert!(
        !prompt.contains("Conversation context"),
        "conversation hint must be suppressed: {prompt:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_formatting_hint_chains_after_with_system_prompt() {
    let provider = RecordingProvider::default();
    let kernel = build_kernel(provider.clone());
    let inbox = KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
        .with_system_prompt("base")
        .with_formatting_hint(HINT);

    let prompt = captured_prompt(inbox, &provider)
        .await
        .expect("system_prompt present");

    let base_idx = prompt.find("base").expect("base prompt present");
    let fmt_idx = prompt.find(HINT).expect("formatting hint present");
    assert!(base_idx < fmt_idx, "{prompt:?}");
}
