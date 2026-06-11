//! Integration tests for [`TtsTool`]: the tool synthesizes speech via an
//! injected provider, stores the audio, and degrades every failure mode to a
//! clean, leak-free soft result without aborting the run.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use crabgent_channel::{AudioStore, AudioStoreError};
use crabgent_core::error::ToolError;
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::{
    AudioRef, Subject, TtsAudioFormat, TtsError, TtsModelId, TtsProvider, TtsProviderCapabilities,
    TtsRequest, TtsResponse, VoiceId,
};
use crabgent_tool_tts::TtsTool;
use serde_json::{Value, json};

const MODEL: &str = "tts-1";
const DEFAULT_VOICE: &str = "alloy";

enum StubOutcome {
    Ready(AudioRef),
    StoreError,
}

struct StubAudioStore {
    outcome: StubOutcome,
    puts: Arc<Mutex<Vec<(Bytes, String)>>>,
}

impl StubAudioStore {
    fn ready(ref_value: &str) -> Self {
        Self {
            outcome: StubOutcome::Ready(AudioRef::new(ref_value)),
            puts: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn store_error() -> Self {
        Self {
            outcome: StubOutcome::StoreError,
            puts: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl AudioStore for StubAudioStore {
    async fn put(&self, bytes: Bytes, mime: &str) -> Result<AudioRef, AudioStoreError> {
        match &self.outcome {
            StubOutcome::Ready(audio_ref) => {
                if let Ok(mut slot) = self.puts.lock() {
                    slot.push((bytes, mime.to_owned()));
                }
                Ok(audio_ref.clone())
            }
            StubOutcome::StoreError => Err(AudioStoreError::MimeUnsupported),
        }
    }

    async fn get(&self, _audio_ref: &AudioRef) -> Result<(Bytes, String), AudioStoreError> {
        Err(AudioStoreError::NotFound)
    }
}

struct RecordingTtsProvider {
    last: Arc<Mutex<Option<TtsRequest>>>,
    calls: Arc<Mutex<usize>>,
    audio: Vec<u8>,
    mime: String,
    fail: bool,
}

impl RecordingTtsProvider {
    fn answering(bytes: Vec<u8>, mime: &str) -> Self {
        Self {
            last: Arc::new(Mutex::new(None)),
            calls: Arc::new(Mutex::new(0)),
            audio: bytes,
            mime: mime.to_owned(),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            last: Arc::new(Mutex::new(None)),
            calls: Arc::new(Mutex::new(0)),
            audio: Vec::new(),
            mime: String::new(),
            fail: true,
        }
    }
}

#[async_trait]
impl TtsProvider for RecordingTtsProvider {
    async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse, TtsError> {
        if let Ok(mut count) = self.calls.lock() {
            *count += 1;
        }
        if let Ok(mut slot) = self.last.lock() {
            *slot = Some(req.clone());
        }
        if self.fail {
            return Err(TtsError::Backend(
                "provider exploded with secret detail".to_owned(),
            ));
        }
        Ok(TtsResponse {
            audio: Arc::from(self.audio.as_slice()),
            mime: self.mime.clone(),
            model: req.model,
        })
    }

    fn capabilities(&self) -> TtsProviderCapabilities {
        TtsProviderCapabilities::default()
    }
}

struct Doubles {
    tool: TtsTool,
    puts: Arc<Mutex<Vec<(Bytes, String)>>>,
    last: Arc<Mutex<Option<TtsRequest>>>,
    calls: Arc<Mutex<usize>>,
}

fn make_tool(store: StubAudioStore, provider: RecordingTtsProvider) -> Doubles {
    let puts = store.puts.clone();
    let last = provider.last.clone();
    let calls = provider.calls.clone();
    let store_dyn: Arc<dyn AudioStore> = Arc::new(store);
    let provider_dyn: Arc<dyn TtsProvider> = Arc::new(provider);
    let tool = TtsTool::new(
        store_dyn,
        provider_dyn,
        TtsModelId::new(MODEL),
        VoiceId::new(DEFAULT_VOICE),
    );
    Doubles {
        tool,
        puts,
        last,
        calls,
    }
}

fn ctx() -> ToolCtx {
    ToolCtx::new(Subject::new("u"))
}

#[tokio::test]
async fn speak_success_returns_audio_ref_mime_model() {
    let doubles = make_tool(
        StubAudioStore::ready("aud-out-1"),
        RecordingTtsProvider::answering(b"ID3synth-audio".to_vec(), "audio/mpeg"),
    );

    let result = doubles
        .tool
        .execute_result(json!({ "text": "hallo welt" }), &ctx())
        .await
        .expect("tool runs");

    assert!(!result.is_error, "expected success: {result:?}");
    assert_eq!(result.output["audio_ref"], "aud-out-1");
    assert_eq!(result.output["mime"], "audio/mpeg");
    assert_eq!(result.output["model"], MODEL);

    let puts = doubles.puts.lock().expect("puts lock");
    assert_eq!(puts.len(), 1, "store received exactly one put");
    let (bytes, mime) = puts.first().expect("one put recorded");
    assert_eq!(
        bytes.as_ref(),
        b"ID3synth-audio",
        "store got the provider bytes"
    );
    assert_eq!(mime, "audio/mpeg", "store got the provider mime");
}

#[tokio::test]
async fn provider_failure_is_soft_error_no_leak() {
    let doubles = make_tool(
        StubAudioStore::ready("aud-out-1"),
        RecordingTtsProvider::failing(),
    );

    let result = doubles
        .tool
        .execute_result(json!({ "text": "hallo welt" }), &ctx())
        .await
        .expect("provider failure does not hard-abort the run");

    assert!(result.is_error, "provider failure surfaces as a soft error");
    let output = result.output.to_string();
    assert!(
        output.contains("speech synthesis failed"),
        "opaque reason expected: {output}"
    );
    assert!(
        !output.contains("provider exploded with secret detail"),
        "provider error detail must not leak: {output}"
    );

    let puts = doubles.puts.lock().expect("puts lock");
    assert!(puts.is_empty(), "store is not touched when synthesis fails");
}

#[tokio::test]
async fn store_failure_is_soft_error() {
    let doubles = make_tool(
        StubAudioStore::store_error(),
        RecordingTtsProvider::answering(b"ID3audio".to_vec(), "audio/mpeg"),
    );

    let result = doubles
        .tool
        .execute_result(json!({ "text": "hallo welt" }), &ctx())
        .await
        .expect("store failure does not hard-abort the run");

    assert!(result.is_error, "store failure surfaces as a soft error");
    let output = result.output.to_string();
    assert!(
        output.contains("audio store unavailable"),
        "opaque reason expected: {output}"
    );
    assert!(
        !output.contains("unsupported audio MIME type"),
        "store error detail must not leak: {output}"
    );
}

#[tokio::test]
async fn empty_text_is_soft_error_without_provider_call() {
    let doubles = make_tool(
        StubAudioStore::ready("aud-out-1"),
        RecordingTtsProvider::answering(b"never reached".to_vec(), "audio/mpeg"),
    );

    let result = doubles
        .tool
        .execute_result(json!({ "text": "   " }), &ctx())
        .await
        .expect("empty text stays soft");

    assert!(result.is_error, "empty text is a soft client error");
    assert!(
        result.output.to_string().contains("empty input text"),
        "opaque reason expected: {}",
        result.output
    );
    let calls = *doubles.calls.lock().expect("calls lock");
    assert_eq!(calls, 0, "empty text never reaches the provider");
}

#[tokio::test]
async fn over_long_text_is_soft_error_without_provider_call() {
    let doubles = make_tool(
        StubAudioStore::ready("aud-out-1"),
        RecordingTtsProvider::answering(b"never reached".to_vec(), "audio/mpeg"),
    );

    // The tool caps input at 4096 chars; 4097 trips the soft limit before any
    // synthesis call.
    let text = "a".repeat(4097);
    let result = doubles
        .tool
        .execute_result(json!({ "text": text }), &ctx())
        .await
        .expect("over-long text stays soft");

    assert!(result.is_error, "over-long text is a soft client error");
    assert!(
        result.output.to_string().contains("input text too long"),
        "opaque reason expected: {}",
        result.output
    );
    let calls = *doubles.calls.lock().expect("calls lock");
    assert_eq!(calls, 0, "over-long text never reaches the provider");
}

#[tokio::test]
async fn voice_and_format_override_honored() {
    let doubles = make_tool(
        StubAudioStore::ready("aud-out-1"),
        RecordingTtsProvider::answering(b"wavbytes".to_vec(), "audio/wav"),
    );

    let args: Value = json!({ "text": "speak this", "voice": "rachel", "format": "wav" });
    doubles
        .tool
        .execute_result(args, &ctx())
        .await
        .expect("override path runs");

    let req = doubles
        .last
        .lock()
        .expect("last lock")
        .clone()
        .expect("provider was called");
    assert_eq!(req.voice.as_str(), "rachel", "voice override honored");
    assert_eq!(req.format, TtsAudioFormat::Wav, "format override honored");

    // Default path: no overrides falls back to the configured voice and mp3.
    let defaults = make_tool(
        StubAudioStore::ready("aud-out-2"),
        RecordingTtsProvider::answering(b"mp3bytes".to_vec(), "audio/mpeg"),
    );
    defaults
        .tool
        .execute_result(json!({ "text": "default please" }), &ctx())
        .await
        .expect("default path runs");

    let def_req = defaults
        .last
        .lock()
        .expect("last lock")
        .clone()
        .expect("provider was called");
    assert_eq!(def_req.voice.as_str(), DEFAULT_VOICE, "default voice used");
    assert_eq!(def_req.format, TtsAudioFormat::Mp3, "default format is mp3");
}

#[tokio::test]
async fn advertises_name_description_and_required_text() {
    let doubles = make_tool(
        StubAudioStore::ready("aud-out-1"),
        RecordingTtsProvider::answering(b"x".to_vec(), "audio/mpeg"),
    );

    assert_eq!(doubles.tool.name(), "speak");
    assert!(
        doubles.tool.description().contains("speech"),
        "description mentions speech synthesis"
    );

    let schema = doubles.tool.parameters_schema();
    let required = schema["required"].as_array().expect("required is an array");
    assert!(required.iter().any(|v| v == "text"), "text is required");
}

#[tokio::test]
async fn simple_execute_yields_the_output_value() {
    let doubles = make_tool(
        StubAudioStore::ready("aud-out-1"),
        RecordingTtsProvider::answering(b"ID3audio".to_vec(), "audio/mpeg"),
    );

    let output = doubles
        .tool
        .execute(json!({ "text": "hallo" }), &ctx())
        .await
        .expect("simple path runs");

    assert_eq!(output["audio_ref"], "aud-out-1");
    assert_eq!(output["model"], MODEL);
}

#[tokio::test]
async fn invalid_args_is_hard_invalid_args() {
    let doubles = make_tool(
        StubAudioStore::ready("aud-out-1"),
        RecordingTtsProvider::answering(b"x".to_vec(), "audio/mpeg"),
    );

    let err = doubles
        .tool
        .execute_result(json!({ "voice": "rachel" }), &ctx())
        .await
        .expect_err("missing text is invalid");

    assert!(matches!(err, ToolError::InvalidArgs(_)), "got {err:?}");
}
