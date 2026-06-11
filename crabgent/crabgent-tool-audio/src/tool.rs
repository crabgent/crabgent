//! [`HearAgainTool`]: route a retained user voice clip to an independent
//! audio-native model and answer a question about how it was said.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::AudioStore;
use crabgent_core::error::ToolError;
use crabgent_core::tool::{Tool, ToolCtx, parse_args_with_context, soft_error_object};
use crabgent_core::{AudioRef, ModelId, Provider, ToolResult};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::call::{AskAudioError, AudioCall, ask_audio};
use crate::circuit::AudioCircuit;

const TOOL_NAME: &str = "hear_again";

const DESCRIPTION: &str = "Re-listen to a retained user voice message with an \
    audio-native model and answer a question about HOW it was said (tone, \
    emotion, pauses, emphasis, laughter, mumbled or misheard words) that the \
    text transcript may have lost. Pass the `audio_ref` from the audio note on \
    a recent voice message. This runs an independent one-shot call to a \
    separate audio model, so the current chat model need not support audio. \
    The answer is generated with at most 1024 tokens and truncated to 8 KB.";

/// Pull tool that fetches retained audio by [`AudioRef`] and sends a one-shot
/// `[audio + question]` request to an injected audio-native provider.
///
/// Holds the retained-audio store plus the `[audio]` route (`provider` +
/// `model`), resolved by the consumer independently of the chat model. The
/// tool carries no `PolicyHook`: like the core builtins it is gated by the
/// kernel's `Action::ToolCall("hear_again")` consult before dispatch, and it
/// has no sub-name structured policy target.
pub struct HearAgainTool {
    store: Arc<dyn AudioStore>,
    provider: Arc<dyn Provider>,
    model: ModelId,
    circuit: Arc<AudioCircuit>,
}

impl HearAgainTool {
    /// Wire the tool with a retained-audio store, the `[audio]` route
    /// (`provider` + `model`), and the shared [`AudioCircuit`].
    ///
    /// The circuit is shared (`Arc`) with the divergence push hook so the
    /// breaker and per-call budget bound every audio call, not just this tool's.
    #[must_use]
    pub fn new(
        store: Arc<dyn AudioStore>,
        provider: Arc<dyn Provider>,
        model: ModelId,
        circuit: Arc<AudioCircuit>,
    ) -> Self {
        Self {
            store,
            provider,
            model,
            circuit,
        }
    }

    /// Full execution: resolve the handle, fetch audio, run the one-shot
    /// audio call. Missing/expired handles return a soft `NotFound`; store,
    /// payload, and provider failures degrade to a soft error so the chat run
    /// continues. Store and provider error detail is logged, never returned.
    async fn rehear(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        let parsed: Args = parse_args_with_context(args, "hear_again args")?;
        if parsed.question.trim().is_empty() {
            return Err(ToolError::InvalidArgs(
                "hear_again args: question must not be empty".to_owned(),
            ));
        }

        // The shared audio call is logging-free and runs under the shared
        // circuit (per-call timeout + breaker + send-byte cap). Map its failures
        // onto the tool contract here: NotFound stays a soft `NotFound`; every
        // other failure (including a tripped breaker or a timeout) degrades to a
        // soft error so the chat run continues, with the detail logged but never
        // returned.
        let result = self
            .circuit
            .run(ask_audio(AudioCall {
                store: self.store.as_ref(),
                provider: self.provider.as_ref(),
                model: self.model.clone(),
                audio_ref: &parsed.audio_ref,
                question: parsed.question,
                subject: ctx.subject.clone(),
                cancel: ctx.cancel.as_ref(),
                max_send_bytes: self.circuit.max_send_bytes(),
            }))
            .await;
        match result {
            Ok(answer) => Ok(ToolResult::success(json!({
                "answer": answer.answer,
                "model": answer.model,
            }))),
            Err(AskAudioError::NotFound) => Err(not_found(&parsed.audio_ref)),
            Err(err) => Ok(log_soft(soft_reason(&err), &err)),
        }
    }
}

/// LLM-safe one-line reason for each soft failure. No source detail, no
/// credentials: the underlying error is logged via `log_soft`, never returned.
const fn soft_reason(err: &AskAudioError) -> &'static str {
    match err {
        AskAudioError::NotFound => "retained audio not found",
        AskAudioError::Store(_) => "audio store unavailable",
        AskAudioError::Payload(_) => "retained audio could not be prepared for the audio model",
        AskAudioError::Provider(_) => "audio model unavailable",
        AskAudioError::Transcode(_) => "audio could not be processed",
        AskAudioError::TooLarge { .. } => "retained audio too large to re-hear",
        AskAudioError::Timeout => "audio model timed out",
        AskAudioError::CircuitOpen => "audio perception temporarily paused",
    }
}

#[derive(Deserialize)]
struct Args {
    audio_ref: AudioRef,
    question: String,
}

/// Log a recoverable failure once and return the opaque soft result the LLM
/// sees. Centralising the single `warn!` keeps `rehear` under the cognitive
/// complexity cap (the macro expands to several branches under workspace
/// feature unification, so it lives in one place, not three).
fn log_soft(reason: &str, error: &dyn std::fmt::Display) -> ToolResult {
    crabgent_log::warn!(
        tool = TOOL_NAME,
        reason = reason,
        error = %error,
        "hear_again degraded to a soft error"
    );
    soft_error_object(reason)
}

/// The soft `NotFound` returned when a handle is missing or has been swept.
fn not_found(audio_ref: &AudioRef) -> ToolError {
    ToolError::NotFound(format!(
        "audio reference not available (missing or expired): {}",
        audio_ref.as_str()
    ))
}

#[async_trait]
impl Tool for HearAgainTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["audio_ref", "question"],
            "properties": {
                "audio_ref": {
                    "type": "string",
                    "description": "Handle of the retained audio, taken from the audio note on a recent voice message."
                },
                "question": {
                    "type": "string",
                    "description": "What to determine about how the message was said (tone, pauses, mumbled words, ...)."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        // hear_again models recoverable store/payload/provider failures as soft
        // tool results (see `execute_result`); the simple path yields the output.
        self.rehear(args, ctx).await.map(|result| result.output)
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        self.rehear(args, ctx).await
    }
}
