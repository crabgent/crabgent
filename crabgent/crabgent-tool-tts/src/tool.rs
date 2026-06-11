//! [`TtsTool`]: synthesize speech from text and return a stored audio handle.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::AudioStore;
use crabgent_core::error::ToolError;
use crabgent_core::tool::{Tool, ToolCtx, parse_args_with_context, soft_error_object};
use crabgent_core::types::ToolResult;
use crabgent_core::{TtsAudioFormat, TtsModelId, TtsProvider, TtsRequest, VoiceId};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::TtsToolError;

/// LLM-facing name of the tool.
pub const TOOL_NAME: &str = "speak";

const DESCRIPTION: &str = "Synthesize speech audio from text and return an `audio_ref` \
handle to the stored audio. Pass the text to speak; the audio is generated and stored, \
and the handle can be sent back over a channel. Input is capped (long text is rejected). \
`voice` and `format` are optional overrides; omit them to use the configured defaults.";

/// Synthesize speech from text and store the resulting audio.
///
/// Provider-neutral: any [`TtsProvider`] (built-in or external) drives
/// synthesis, and any [`AudioStore`] retains the bytes. Like the
/// `hear_again` tool, this carries no [`PolicyHook`]; it is gated by the
/// kernel's `Action::ToolCall("speak")` consult before dispatch.
///
/// [`TtsProvider`]: crabgent_core::TtsProvider
/// [`PolicyHook`]: crabgent_core::policy::PolicyHook
pub struct TtsTool {
    store: Arc<dyn AudioStore>,
    provider: Arc<dyn TtsProvider>,
    model: TtsModelId,
    default_voice: VoiceId,
    default_format: TtsAudioFormat,
    max_input_chars: usize,
}

impl TtsTool {
    /// Build a tool over an injected store, provider, model, and default voice.
    ///
    /// The default output format is MP3 and the input cap is 4096 characters
    /// (conservative: `ElevenLabs` accepts ~5000 chars, `OpenAI` 4096 tokens;
    /// providers also enforce their own server-side limit).
    #[must_use]
    pub fn new(
        store: Arc<dyn AudioStore>,
        provider: Arc<dyn TtsProvider>,
        model: TtsModelId,
        default_voice: VoiceId,
    ) -> Self {
        Self {
            store,
            provider,
            model,
            default_voice,
            default_format: TtsAudioFormat::Mp3,
            max_input_chars: 4096,
        }
    }

    /// Synthesize the audio, store it, and build the success value.
    ///
    /// Flat with guard clauses: client errors (empty/too-long text) return
    /// before any provider call; provider and store faults map to their
    /// typed variants. The caller turns any error into a soft tool result.
    async fn synth(&self, args: Args) -> Result<Value, TtsToolError> {
        let text = args.text.trim();
        if text.is_empty() {
            return Err(TtsToolError::InputEmpty);
        }
        let len = text.chars().count();
        if len > self.max_input_chars {
            return Err(TtsToolError::InputTooLong {
                len,
                max: self.max_input_chars,
            });
        }

        let voice = args.voice.unwrap_or_else(|| self.default_voice.clone());
        let format = args.format.unwrap_or(self.default_format);
        let req = TtsRequest {
            text: text.to_owned(),
            model: self.model.clone(),
            voice,
            format,
        };

        let resp = self
            .provider
            .synthesize(req)
            .await
            .map_err(TtsToolError::Provider)?;
        let audio_ref = self
            .store
            .put(bytes::Bytes::copy_from_slice(&resp.audio), &resp.mime)
            .await
            .map_err(TtsToolError::Store)?;

        Ok(json!({
            "audio_ref": audio_ref.as_str(),
            "mime": resp.mime,
            "model": resp.model.as_str(),
        }))
    }
}

/// LLM-safe one-line reason for each failure. No source detail, no
/// credentials: system faults are logged via `log_soft`, never returned.
const fn soft_reason(err: &TtsToolError) -> &'static str {
    match err {
        TtsToolError::Provider(_) => "speech synthesis failed",
        TtsToolError::Store(_) => "audio store unavailable",
        TtsToolError::InputEmpty => "empty input text",
        TtsToolError::InputTooLong { .. } => "input text too long",
    }
}

/// Log a system fault once and return the opaque soft result the LLM sees.
/// Centralising the single `warn!` keeps the caller under the cognitive
/// complexity cap (the macro expands to several branches under workspace
/// feature unification, so it lives in one place).
fn log_soft(reason: &str, error: &dyn std::fmt::Display) -> ToolResult {
    crabgent_log::warn!(
        tool = TOOL_NAME,
        reason = reason,
        error = %error,
        "speak degraded to a soft error"
    );
    soft_error_object(reason)
}

/// Map a tool error to the soft result the LLM sees.
///
/// Provider and store failures are system faults: log them once, then return
/// the opaque reason. Empty/too-long text are client errors carrying no
/// system fault, so they return the opaque reason without a warn.
fn into_soft(err: &TtsToolError) -> ToolResult {
    let reason = soft_reason(err);
    match err {
        TtsToolError::Provider(_) | TtsToolError::Store(_) => log_soft(reason, err),
        TtsToolError::InputEmpty | TtsToolError::InputTooLong { .. } => soft_error_object(reason),
    }
}

#[derive(Deserialize)]
struct Args {
    text: String,
    #[serde(default)]
    voice: Option<VoiceId>,
    #[serde(default)]
    format: Option<TtsAudioFormat>,
}

#[async_trait]
impl Tool for TtsTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["text"],
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to synthesize into speech."
                },
                "voice": {
                    "type": "string",
                    "description": "Optional voice id override. Omit to use the configured default voice."
                },
                "format": {
                    "type": "string",
                    "enum": ["mp3", "opus", "aac", "flac", "wav", "pcm"],
                    "description": "Optional output audio format. Omit to use the default (mp3)."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        // The simple path yields the output value; the soft-error path lives in
        // `execute_result`. A soft result surfaces here as the error object.
        self.execute_result(args, ctx)
            .await
            .map(|result| result.output)
    }

    async fn execute_result(&self, args: Value, _ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        let parsed: Args = parse_args_with_context(args, "speak args")?;
        match self.synth(parsed).await {
            Ok(value) => Ok(ToolResult::success(value)),
            Err(err) => Ok(into_soft(&err)),
        }
    }
}
