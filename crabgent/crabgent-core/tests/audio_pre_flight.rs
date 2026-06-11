use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_core::{
    AllowAllPolicy, AudioPayload, ContentBlock, Kernel, KernelError, LlmRequest, LlmResponse,
    Message, ModelCapabilities, ModelId, ModelInfo, ModelTarget, Provider, ProviderCapabilities,
    ProviderError, RunCtx, RunId, RunRequest, StopReason, Subject, Usage,
};
use tokio_util::sync::CancellationToken;

struct AudioProvider {
    provider_audio: bool,
    model_audio: bool,
    model_aliases: Vec<ModelId>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for AudioProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            text: "ok".into(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        "audio-test"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            audio: self.provider_audio,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        let mut info = ModelInfo::minimal("m", self.name().to_owned());
        info.aliases.clone_from(&self.model_aliases);
        info.caps = ModelCapabilities {
            supports_audio: self.model_audio,
            ..info.caps
        };
        vec![info]
    }
}

fn kernel(provider_audio: bool, model_audio: bool) -> (Kernel, Arc<AtomicUsize>) {
    kernel_with_aliases(provider_audio, model_audio, Vec::new())
}

fn kernel_with_aliases(
    provider_audio: bool,
    model_audio: bool,
    model_aliases: Vec<ModelId>,
) -> (Kernel, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = AudioProvider {
        provider_audio,
        model_audio,
        model_aliases,
        calls: Arc::clone(&calls),
    };
    let kernel = Kernel::builder()
        .provider(provider)
        .policy(AllowAllPolicy)
        .build();
    (kernel, calls)
}

fn request(content: Vec<ContentBlock>) -> RunRequest {
    request_with_model("m", content)
}

fn request_with_model(model: impl Into<ModelTarget>, content: Vec<ContentBlock>) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("audio-user"),
        model: model.into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content,
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(1),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

fn audio_block() -> ContentBlock {
    ContentBlock::Audio(
        AudioPayload::new(vec![82, 73, 70, 70], "audio/wav", Some("clip.wav".into()))
            .expect("valid audio payload"),
    )
}

fn audio_request() -> RunRequest {
    request(vec![audio_block()])
}

fn text_request() -> RunRequest {
    request(vec![ContentBlock::Text { text: "hi".into() }])
}

#[tokio::test]
async fn audio_request_no_audio_provider_rejects() {
    let (kernel, calls) = kernel(false, false);

    let err = kernel
        .run(audio_request(), None)
        .await
        .expect_err("provider without audio rejects audio request");

    match err {
        KernelError::Provider(ProviderError::AudioUnsupported { provider, model }) => {
            assert_eq!(provider, "audio-test");
            assert_eq!(model, "m");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn audio_request_no_audio_model_rejects() {
    let (kernel, calls) = kernel(true, false);

    let err = kernel
        .run(audio_request(), None)
        .await
        .expect_err("model without audio rejects audio request");

    match err {
        KernelError::Provider(ProviderError::AudioUnsupported { provider, model }) => {
            assert_eq!(provider, "audio-test");
            assert_eq!(model, "m");
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn audio_request_with_caps_passes() {
    let (kernel, calls) = kernel(true, true);

    let text = kernel.run(audio_request(), None).await.expect("audio ok");

    assert_eq!(text, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn audio_check_resolves_via_model_alias() {
    let (kernel, calls) = kernel_with_aliases(true, true, vec![ModelId::new("m-alias")]);

    let text = kernel
        .run(request_with_model("m-alias", vec![audio_block()]), None)
        .await
        .expect("audio alias ok");

    assert_eq!(text, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn text_only_request_unaffected() {
    let (kernel, calls) = kernel(false, false);

    let text = kernel.run(text_request(), None).await.expect("text ok");

    assert_eq!(text, "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
