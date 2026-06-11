//! Integration tests for [`HearAgainTool`]: the tool resolves a retained
//! audio handle, routes it to an injected audio model, and degrades every
//! failure mode to a clean, leak-free result without aborting the run.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use crabgent_channel::{AudioStore, AudioStoreError};
use crabgent_core::error::ToolError;
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::{
    AudioRef, LlmRequest, LlmResponse, ModelId, Provider, ProviderCapabilities, ProviderError,
    RunCtx, StopReason, Subject, Usage,
};
use crabgent_tool_audio::{AudioCircuit, AudioCircuitConfig, HearAgainTool};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const AUDIO_MODEL: &str = "gpt-4o-audio-preview";

enum StubOutcome {
    Ready(Bytes, String),
    Missing,
    StoreError,
}

struct StubAudioStore {
    outcome: StubOutcome,
}

impl StubAudioStore {
    fn ready(mime: &str) -> Self {
        Self {
            outcome: StubOutcome::Ready(
                Bytes::from_static(b"RIFFstub-audio-bytes"),
                mime.to_owned(),
            ),
        }
    }

    const fn missing() -> Self {
        Self {
            outcome: StubOutcome::Missing,
        }
    }

    const fn store_error() -> Self {
        Self {
            outcome: StubOutcome::StoreError,
        }
    }
}

#[async_trait]
impl AudioStore for StubAudioStore {
    async fn put(&self, _bytes: Bytes, _mime: &str) -> Result<AudioRef, AudioStoreError> {
        Ok(AudioRef::new("stub-put"))
    }

    async fn get(&self, _audio_ref: &AudioRef) -> Result<(Bytes, String), AudioStoreError> {
        match &self.outcome {
            StubOutcome::Ready(bytes, mime) => Ok((bytes.clone(), mime.clone())),
            StubOutcome::Missing => Err(AudioStoreError::NotFound),
            StubOutcome::StoreError => Err(AudioStoreError::MimeUnsupported),
        }
    }
}

struct RecordingProvider {
    last: Arc<Mutex<Option<LlmRequest>>>,
    answer: String,
    fail: bool,
}

impl RecordingProvider {
    fn answering(answer: &str) -> Self {
        Self {
            last: Arc::new(Mutex::new(None)),
            answer: answer.to_owned(),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            last: Arc::new(Mutex::new(None)),
            answer: String::new(),
            fail: true,
        }
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        if let Ok(mut slot) = self.last.lock() {
            *slot = Some(req.clone());
        }
        if self.fail {
            return Err(ProviderError::Transport("audio backend down".into()));
        }
        Ok(LlmResponse {
            text: self.answer.clone(),
            tool_calls: Vec::new(),
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
            },
            model: ModelId::from(AUDIO_MODEL),
        })
    }

    fn name(&self) -> &'static str {
        "recording"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            audio: true,
            ..Default::default()
        }
    }
}

fn make_tool(
    store: StubAudioStore,
    provider: RecordingProvider,
) -> (HearAgainTool, Arc<Mutex<Option<LlmRequest>>>) {
    let captured = provider.last.clone();
    let store_dyn: Arc<dyn AudioStore> = Arc::new(store);
    let provider_dyn: Arc<dyn Provider> = Arc::new(provider);
    let circuit = Arc::new(AudioCircuit::new(AudioCircuitConfig::default()));
    (
        HearAgainTool::new(store_dyn, provider_dyn, ModelId::from(AUDIO_MODEL), circuit),
        captured,
    )
}

fn args(audio_ref: &str, question: &str) -> Value {
    json!({ "audio_ref": audio_ref, "question": question })
}

#[tokio::test]
async fn routes_audio_to_audio_model_and_returns_answer() {
    let (tool, captured) = make_tool(
        StubAudioStore::ready("audio/wav"),
        RecordingProvider::answering("hesitant, long pause before the yes"),
    );

    // Pass a live cancellation token so the run forwards it to the audio call.
    let ctx = ToolCtx::new(Subject::new("u")).with_cancel(CancellationToken::new());
    let result = tool
        .execute_result(args("aud-1", "did they sound sure?"), &ctx)
        .await
        .expect("tool runs");

    assert!(!result.is_error, "expected success: {result:?}");
    assert_eq!(
        result.output["answer"],
        "hesitant, long pause before the yes"
    );
    assert_eq!(result.output["model"], AUDIO_MODEL);

    let req = captured
        .lock()
        .expect("capture lock")
        .clone()
        .expect("provider called with a request");
    assert_eq!(
        req.model,
        ModelId::from(AUDIO_MODEL),
        "routed to the audio model, not the chat model"
    );
    let content = req
        .messages
        .first()
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .expect("user content array");
    assert_eq!(
        content.len(),
        2,
        "exactly one audio block plus the question"
    );
    assert_eq!(
        content[0]["type"], "audio",
        "first block is the retained audio"
    );
    assert_eq!(content[1]["type"], "text", "second block is the question");
    // The question is framed (anti-refusal prefix) but must still carry the
    // caller's verbatim question, and a system prompt is attached so the audio
    // model treats the attached clip as analyzable rather than refusing.
    let text = content[1]["text"].as_str().expect("question text");
    assert!(
        text.ends_with("did they sound sure?"),
        "framed question keeps the caller's question verbatim: {text:?}"
    );
    assert!(
        req.system_prompt
            .as_deref()
            .is_some_and(|p| p.contains("audio-analysis model")),
        "audio call carries the anti-refusal system prompt"
    );
}

#[tokio::test]
async fn missing_or_expired_ref_is_soft_not_found() {
    let (tool, _) = make_tool(StubAudioStore::missing(), RecordingProvider::answering("x"));

    let err = tool
        .execute_result(args("gone", "tone?"), &ToolCtx::new(Subject::new("u")))
        .await
        .expect_err("missing ref is an error");

    assert!(
        matches!(&err, ToolError::NotFound(msg) if msg.contains("missing or expired")),
        "expected soft NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn missing_question_is_invalid_args() {
    let (tool, _) = make_tool(
        StubAudioStore::ready("audio/wav"),
        RecordingProvider::answering("x"),
    );

    let err = tool
        .execute_result(
            json!({ "audio_ref": "a" }),
            &ToolCtx::new(Subject::new("u")),
        )
        .await
        .expect_err("missing question is invalid");

    assert!(matches!(err, ToolError::InvalidArgs(_)), "got {err:?}");
}

#[tokio::test]
async fn unsupported_audio_mime_is_soft_error_without_provider_call() {
    let (tool, captured) = make_tool(
        StubAudioStore::ready("text/plain"),
        RecordingProvider::answering("never reached"),
    );

    let result = tool
        .execute_result(args("a", "tone?"), &ToolCtx::new(Subject::new("u")))
        .await
        .expect("runs without panic");

    assert!(result.is_error, "rejected payload yields a soft error");
    assert!(
        captured.lock().expect("capture lock").is_none(),
        "provider must not be called when the payload is rejected"
    );
}

#[tokio::test]
async fn provider_failure_is_soft_error_and_opaque() {
    let (tool, _) = make_tool(
        StubAudioStore::ready("audio/wav"),
        RecordingProvider::failing(),
    );

    let result = tool
        .execute_result(args("a", "tone?"), &ToolCtx::new(Subject::new("u")))
        .await
        .expect("provider failure does not hard-abort the run");

    assert!(result.is_error, "provider failure surfaces as a soft error");
    let output = result.output.to_string();
    assert!(
        !output.contains("audio backend down"),
        "provider error detail must not leak: {output}"
    );
    assert!(
        output.contains("audio model unavailable"),
        "opaque message: {output}"
    );
}

#[tokio::test]
async fn store_failure_is_soft_error_without_provider_call() {
    let (tool, captured) = make_tool(
        StubAudioStore::store_error(),
        RecordingProvider::answering("never reached"),
    );

    let result = tool
        .execute_result(args("a", "tone?"), &ToolCtx::new(Subject::new("u")))
        .await
        .expect("store failure does not hard-abort the run");

    assert!(result.is_error, "store failure surfaces as a soft error");
    assert!(
        !result.output.to_string().contains("MimeUnsupported"),
        "store error detail must not leak: {}",
        result.output
    );
    assert!(
        captured.lock().expect("capture lock").is_none(),
        "provider must not be called when the store fails"
    );
}

#[tokio::test]
async fn simple_execute_yields_the_answer_value() {
    let (tool, _) = make_tool(
        StubAudioStore::ready("audio/wav"),
        RecordingProvider::answering("calm and clear"),
    );

    let output = tool
        .execute(args("a", "tone?"), &ToolCtx::new(Subject::new("u")))
        .await
        .expect("simple path runs");

    assert_eq!(output["answer"], "calm and clear");
    assert_eq!(output["model"], AUDIO_MODEL);
}

#[test]
fn advertises_name_description_and_required_args() {
    let (tool, _) = make_tool(
        StubAudioStore::ready("audio/wav"),
        RecordingProvider::answering("x"),
    );

    assert_eq!(tool.name(), "hear_again");
    assert!(
        tool.description().contains("audio"),
        "description mentions audio re-listening"
    );

    let schema = tool.parameters_schema();
    let required = schema["required"].as_array().expect("required array");
    assert!(
        required.iter().any(|v| v == "audio_ref"),
        "audio_ref required"
    );
    assert!(
        required.iter().any(|v| v == "question"),
        "question required"
    );
}

#[tokio::test]
async fn empty_question_is_invalid_args_without_provider_call() {
    let (tool, captured) = make_tool(
        StubAudioStore::ready("audio/wav"),
        RecordingProvider::answering("never reached"),
    );

    let err = tool
        .execute_result(args("a", "   "), &ToolCtx::new(Subject::new("u")))
        .await
        .expect_err("blank question is rejected");

    assert!(matches!(err, ToolError::InvalidArgs(_)), "got {err:?}");
    assert!(
        captured.lock().expect("capture lock").is_none(),
        "neither store nor provider is reached for a blank question"
    );
}

#[tokio::test(start_paused = true)]
async fn breaker_trips_after_failures_and_turn_completes_on_soft_error() {
    let circuit = Arc::new(AudioCircuit::new(AudioCircuitConfig {
        max_consecutive_failures: 2,
        per_call_timeout: Duration::from_secs(5),
        cooldown: Duration::from_secs(30),
        max_send_bytes: 10 * 1024 * 1024,
    }));
    let store: Arc<dyn AudioStore> = Arc::new(StubAudioStore::ready("audio/wav"));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider::failing());
    let tool = HearAgainTool::new(store, provider, ModelId::from(AUDIO_MODEL), circuit.clone());
    let ctx = ToolCtx::new(Subject::new("u"));

    // Two provider failures trip the breaker; each turn still completes soft.
    for _ in 0..2 {
        let result = tool
            .execute_result(args("a", "tone?"), &ctx)
            .await
            .expect("provider failure stays soft");
        assert!(result.is_error);
    }
    assert!(
        circuit.is_open(),
        "breaker trips after the failure threshold"
    );

    // The open breaker short-circuits: the turn completes on a soft error
    // (fail-open) without another provider call.
    let result = tool
        .execute_result(args("a", "tone?"), &ctx)
        .await
        .expect("open breaker stays soft");
    assert!(result.is_error);
    assert!(
        result
            .output
            .to_string()
            .contains("audio perception temporarily paused"),
        "open breaker surfaces the paused reason: {}",
        result.output
    );
}

#[tokio::test]
async fn oversized_clip_fails_open_without_provider_call_or_trip() {
    let circuit = Arc::new(AudioCircuit::new(AudioCircuitConfig {
        max_consecutive_failures: 1,
        per_call_timeout: Duration::from_secs(5),
        cooldown: Duration::from_secs(30),
        max_send_bytes: 4,
    }));
    let store: Arc<dyn AudioStore> = Arc::new(StubAudioStore::ready("audio/wav"));
    let provider = RecordingProvider::answering("never reached");
    let captured = provider.last.clone();
    let provider_dyn: Arc<dyn Provider> = Arc::new(provider);
    let tool = HearAgainTool::new(
        store,
        provider_dyn,
        ModelId::from(AUDIO_MODEL),
        circuit.clone(),
    );
    let ctx = ToolCtx::new(Subject::new("u"));

    let result = tool
        .execute_result(args("a", "tone?"), &ctx)
        .await
        .expect("oversized clip stays soft");
    assert!(result.is_error, "oversized clip fails open");
    assert!(
        captured.lock().expect("capture lock").is_none(),
        "an oversized clip is not sent to the provider"
    );
    assert!(
        !circuit.is_open(),
        "a too-large clip is a local guard, not provider degradation, so it must not trip"
    );
}
