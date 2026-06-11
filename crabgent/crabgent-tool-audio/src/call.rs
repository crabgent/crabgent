//! Shared one-shot audio-call path.
//!
//! [`ask_audio`] fetches a retained clip by [`AudioRef`], sends a single
//! `[audio + question]` request to an audio-native provider, and returns the
//! truncated text answer. Both the pull tool ([`crate::HearAgainTool`]) and the
//! divergence push hook route through this one function so the audio
//! call has a single mechanism, not two parallel ones.
//!
//! The function performs no logging and never panics. It maps each failure to
//! an [`AskAudioError`] variant; the caller owns the policy for that failure
//! (the pull tool degrades to a soft tool result, the push hook fails open).

use std::sync::Arc;

use crabgent_channel::{AudioStore, AudioStoreError};
use crabgent_core::text::truncate_with_ellipsis;
use crabgent_core::{
    AudioPayload, AudioRef, ContentBlock, LlmRequest, Message, ModelId, PayloadError, Provider,
    ProviderError, RawMessages, RunCtx, RunId, Subject, WebSearchConfig,
};
use tokio_util::sync::CancellationToken;

/// Cap on tokens the audio model may generate for one answer.
const MAX_TOKENS: u32 = 1024;

/// Byte cap applied to the returned answer before it enters a tool output or
/// an annotation tag.
pub const MAX_ANSWER_BYTES: usize = 8 * 1024;

/// Suffix appended when the answer is truncated to [`MAX_ANSWER_BYTES`].
const ANSWER_TRUNCATED_SUFFIX: &str = "... [truncated]";

/// System prompt for the one-shot audio call.
///
/// Chat-audio models (`gpt-audio`) otherwise intermittently refuse with "I
/// cannot hear audio / please upload the file" when the caller's question is
/// phrased as an imperative to listen ("hoer dir das an", "listen to this"),
/// even though the clip IS attached as an `input_audio` part. Verified live:
/// imperative phrasings refuse without this prompt and answer with it. This
/// tells the model the audio is present and must be analyzed, never refused.
const AUDIO_SYSTEM_PROMPT: &str = "You are an audio-analysis model. The user's \
    audio clip is already attached to this message as an input_audio part. \
    Analyze that attached audio directly and answer. Never say you cannot hear \
    or access audio, never ask for the audio to be uploaded, played, or \
    provided. If the wording suggests playing or listening, treat it as a \
    request to analyze the attached clip.";

/// Framing prefixed to the caller's question so an imperative phrasing does
/// not trip the model's refusal reflex. Belt-and-braces with
/// [`AUDIO_SYSTEM_PROMPT`]; the answer stays in the question's own language.
const QUESTION_FRAME_PREFIX: &str = "Analyze the audio clip attached to this \
    message (it is already provided as audio input) and answer the following \
    directly, in the same language as the question. Do not ask for the audio. \
    Question: ";

/// The text answer plus the model that produced it.
pub struct AudioAnswer {
    /// Truncated answer text from the audio model.
    pub answer: String,
    /// Model the provider reported for this completion.
    pub model: ModelId,
}

/// Failure modes of [`ask_audio`], mapped by each caller onto its own surface.
///
/// No variant carries credentials or provider error bodies destined for the
/// LLM: callers log the source via the operator log, never forward it.
#[derive(Debug, thiserror::Error)]
pub enum AskAudioError {
    /// The handle is missing or has been swept from the store.
    #[error("retained audio not found or expired")]
    NotFound,
    /// The store could not return the bytes (infrastructure failure).
    #[error("audio store unavailable: {0}")]
    Store(#[source] AudioStoreError),
    /// The retained bytes could not be prepared as an audio payload.
    #[error("retained audio could not be prepared for the audio model: {0}")]
    Payload(#[source] PayloadError),
    /// The audio model call failed.
    #[error("audio model unavailable: {0}")]
    Provider(#[source] ProviderError),
    /// The retained clip could not be transcoded into a format the Chat-audio
    /// model accepts (`ffmpeg` missing, failed, or timed out). A local prep
    /// failure, not provider degradation, so it does not trip the breaker.
    #[error("audio could not be prepared for the audio model: {0}")]
    Transcode(#[source] crate::transcode::TranscodeError),
    /// The retained clip exceeds the circuit's send-byte ceiling, so it is not
    /// sent to the provider (cost guard, fails open).
    #[error("retained audio too large to send: {size} bytes exceeds {max}")]
    TooLarge {
        /// Size of the retained clip in bytes.
        size: usize,
        /// Configured send ceiling in bytes.
        max: usize,
    },
    /// The call exceeded the circuit's per-call timeout.
    #[error("audio model call timed out")]
    Timeout,
    /// The circuit breaker is open; the call was not attempted.
    #[error("audio perception temporarily paused (circuit open)")]
    CircuitOpen,
}

impl AskAudioError {
    /// Whether this failure counts toward tripping the circuit breaker.
    ///
    /// Only audio-provider degradation and per-call timeouts count. Missing
    /// audio, a too-large clip, a local payload error, a store error, or an
    /// already-open breaker are not provider degradation and leave the
    /// consecutive-failure counter untouched.
    #[must_use]
    pub const fn is_transport_failure(&self) -> bool {
        matches!(self, Self::Timeout | Self::Provider(_))
    }
}

/// One audio-perception call: the `[audio]` route plus the clip, question, and
/// per-call budget. Bundled into a struct so [`ask_audio`] stays a single
/// argument and both callers (pull tool, divergence push) construct it the same
/// way.
pub struct AudioCall<'a> {
    /// Retained-audio store the clip is fetched from.
    pub store: &'a dyn AudioStore,
    /// Audio-native provider the one-shot request is sent to.
    pub provider: &'a dyn Provider,
    /// Audio-capable model on `provider`.
    pub model: ModelId,
    /// Handle of the retained clip.
    pub audio_ref: &'a AudioRef,
    /// What to ask the audio model about the clip.
    pub question: String,
    /// Subject the synthesized run is attributed to.
    pub subject: Subject,
    /// Cancellation token threaded into the run and the provider call.
    pub cancel: Option<&'a CancellationToken>,
    /// Largest clip (bytes) sent to the provider; larger fails open.
    pub max_send_bytes: usize,
}

/// Fetch the retained clip and ask an audio-native model one question about it.
///
/// `call.model` selects the audio-capable model on `call.provider`.
/// `call.cancel` is threaded into both the synthesized [`RunCtx`] and the
/// provider call so a cancelled turn aborts the secondary call. A clip over
/// `call.max_send_bytes` returns [`AskAudioError::TooLarge`] before the provider
/// is called.
pub async fn ask_audio(call: AudioCall<'_>) -> Result<AudioAnswer, AskAudioError> {
    let (audio_bytes, mime) = match call.store.get(call.audio_ref).await {
        Ok(pair) => pair,
        Err(AudioStoreError::NotFound) => return Err(AskAudioError::NotFound),
        Err(err) => return Err(AskAudioError::Store(err)),
    };

    if audio_bytes.len() > call.max_send_bytes {
        return Err(AskAudioError::TooLarge {
            size: audio_bytes.len(),
            max: call.max_send_bytes,
        });
    }

    // One copy: Bytes -> Arc<[u8]> directly, not Bytes -> Vec -> Arc<[u8]>.
    let bytes: Arc<[u8]> = Arc::from(audio_bytes.as_ref());
    // The Chat-audio model accepts only wav/mp3; inbound voice is Ogg/Opus.
    // Transcode here so both callers share one preparation path.
    let (send_bytes, send_mime) = crate::transcode::ensure_chat_audio(bytes, mime)
        .await
        .map_err(AskAudioError::Transcode)?;
    let payload = AudioPayload::new(send_bytes, send_mime, None).map_err(AskAudioError::Payload)?;

    let request = build_request(call.model, payload, &call.question);
    let mut run_ctx = RunCtx::new(RunId::new(), call.subject);
    if let Some(token) = call.cancel {
        run_ctx = run_ctx.with_cancel(token.clone());
    }

    let response = call
        .provider
        .complete(&request, &run_ctx, call.cancel)
        .await
        .map_err(AskAudioError::Provider)?;

    let answer = truncate_with_ellipsis(&response.text, MAX_ANSWER_BYTES, ANSWER_TRUNCATED_SUFFIX)
        .into_owned();
    Ok(AudioAnswer {
        answer,
        model: response.model,
    })
}

/// Assemble the one-shot `[audio + question]` request for the audio model.
///
/// The question is framed and a system prompt attached so the model treats it
/// as a request to analyze the already-attached clip instead of refusing (see
/// [`AUDIO_SYSTEM_PROMPT`]).
fn build_request(model: ModelId, payload: AudioPayload, question: &str) -> LlmRequest {
    let framed = format!("{QUESTION_FRAME_PREFIX}{question}");
    let messages = RawMessages::from(vec![Message::User {
        content: vec![
            ContentBlock::Audio(payload),
            ContentBlock::Text { text: framed },
        ],
        timestamp: None,
    }])
    .into_inner();

    LlmRequest {
        model,
        system_prompt: Some(AUDIO_SYSTEM_PROMPT.to_owned()),
        messages,
        tools: Vec::new(),
        max_tokens: Some(MAX_TOKENS),
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}
