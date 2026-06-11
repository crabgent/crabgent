//! Bullet 4.1: a Telegram Direct inbound surfaces the DM partner identity
//! via the `sender` attribute of the `<inbound>` tag, and never a `name`
//! attribute. Telegram is Direct-only and leaves `Channel::conv_display`
//! at the trait default (`None`), so the readable channel/workspace labels
//! stay absent; the partner is carried by `event.from.display_name`, which
//! the kernel inbox stamps as `sender`.
//!
//! The proof is end-to-end: a mocked Telegram API drives the real poller,
//! the resulting `InboundEvent` flows into a `KernelChannelInbox` wired
//! with the `TelegramChannel` as its `conv_display` source, and a capturing
//! provider records the projected user-message text that reaches the LLM.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel::{Channel, ChannelInbox, KernelChannelInbox};
use crabgent_channel_telegram::poller::build_update_json;
use crabgent_channel_telegram::{TelegramChannel, TelegramPoller};
use crabgent_core::error::ProviderError;
use crabgent_core::owner::Owner;
use crabgent_core::policy::AllowAllPolicy;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, Usage};
use crabgent_core::{Kernel, ModelInfo, RunCtx};
use httpmock::Method::POST;
use httpmock::MockServer;
use serde_json::{Value, json};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

/// Provider that records the first user-message text of every request it
/// sees, so the test can inspect the rendered `<inbound>` tag.
struct CapturingProvider {
    user_texts: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Provider for CapturingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        if let Some(text) = first_user_text(req) {
            self.user_texts.lock().expect("user_texts mutex").push(text);
        }
        Ok(LlmResponse {
            text: "ok".into(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "capture"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("claude-haiku-4-5", "capture")]
    }
}

/// Extract the first user-message text block from the projected request.
/// `LlmRequest.messages` carries the pre-wire JSON (`{"role":"user",
/// "content":[{"type":"text","text":...}]}`), so the test reads the same
/// bytes a provider would.
fn first_user_text(req: &LlmRequest) -> Option<String> {
    req.messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .find_map(|message| {
            message
                .get("content")
                .and_then(Value::as_array)
                .and_then(|blocks| {
                    blocks.iter().find_map(|block| {
                        block.get("text").and_then(Value::as_str).map(str::to_owned)
                    })
                })
        })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn telegram_direct_inbound_carries_sender_and_no_channel_name() {
    let server = MockServer::start();
    // One Direct message from user 7 (username "u7") in chat 42.
    server.mock(|when, then| {
        when.method(POST).path("/bottk/getUpdates");
        then.status(200).json_body(json!({
            "ok": true,
            "result": [build_update_json(1, 42, 7, "hi", "private")],
        }));
    });

    let channel = Arc::new(TelegramChannel::new("tk", "B-1", "crabgent_bot"));
    // Telegram is Direct-only and does not override conv_display: the
    // trait default returns None, so no readable channel/workspace label
    // can ever be stamped for a Telegram conversation.
    assert!(
        channel
            .conv_display(&Owner::new("telegram:42"))
            .await
            .is_none(),
        "Telegram conv_display must stay at the None default"
    );

    let user_texts = Arc::new(Mutex::new(Vec::new()));
    let kernel = Arc::new(
        Kernel::builder()
            .provider(CapturingProvider {
                user_texts: Arc::clone(&user_texts),
            })
            .policy(AllowAllPolicy)
            .build(),
    );
    let inbox: Arc<dyn ChannelInbox> = Arc::new(
        KernelChannelInbox::new(kernel, "claude-haiku-4-5", Arc::new(AllowAllPolicy))
            .with_conv_display_channel(Arc::clone(&channel) as Arc<dyn Channel>),
    );

    let poller_channel = Arc::new(
        TelegramChannel::new("tk", "B-1", "crabgent_bot").with_api_base(server.base_url()),
    );
    let poller = TelegramPoller::new(poller_channel, inbox)
        .with_poll_timeout(Duration::from_millis(50))
        .with_error_backoff(Duration::from_millis(50));

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let handle = tokio::spawn(async move { poller.run(cancel_clone).await });
    // Poll the captured user text instead of a fixed sleep: the poller tick,
    // dispatch, and kernel run all complete asynchronously, so wall-clock
    // waits flake on loaded CI. Bounded at 5s; retries every 20ms.
    let waited = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !user_texts.lock().expect("user_texts mutex").is_empty() {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    cancel.cancel();
    handle.await.expect("join").expect("run ok");
    waited.expect("user message reached the LLM within 5s");

    let texts = user_texts.lock().expect("user_texts mutex");
    assert_eq!(texts.len(), 1, "exactly one user message reached the LLM");
    let tag = &texts[0];
    assert!(
        tag.starts_with("<inbound ") && tag.ends_with("</inbound>"),
        "user text is the wrapped inbound tag: {tag}"
    );
    assert!(
        tag.contains("source=\"direct\""),
        "Telegram inbound is Direct: {tag}"
    );
    assert!(
        tag.contains("channel=\"telegram\""),
        "adapter slug present: {tag}"
    );
    assert!(
        tag.contains("sender=\"u7\""),
        "DM partner flows via the sender attribute: {tag}"
    );
    assert!(
        !tag.contains(" name=\""),
        "Direct-only Telegram never carries a readable channel name: {tag}"
    );
    assert!(
        !tag.contains("workspace=\""),
        "Telegram has no workspace label: {tag}"
    );
}
