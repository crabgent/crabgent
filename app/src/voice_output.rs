//! Explicit spoken replies over upstream TTS plus optional forced alignment.
//!
//! The runtime default stays text. `VoiceOutputGateHook` hides speech tools
//! unless the latest user message clearly asks for audio/TTS output.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use crabgent_channel::{AudioStore, AudioStoreError, ChannelSink, ChannelSubjectExt, MessageRef};
use crabgent_core::tool::{gate_tool_action, parse_args_with_context, soft_error_object};
use crabgent_core::{
    Action, AudioPayload, AudioRef, Decision, ForcedAlignmentProvider, ForcedAlignmentRequest,
    ForcedAlignmentResponse, Hook, LlmRequest, RunCtx, Tool, ToolChoice, ToolCtx, ToolError,
    ToolResult, TtsAudioFormat, TtsModelId, TtsProvider, VoiceId,
};
use crabgent_tool_tts::TtsTool;
use serde::Deserialize;
use serde_json::{Map, Value, json};

pub const VOICE_REPLY_TOOL: &str = "voice_reply";

const CHANNEL_UPLOAD_TOOL: &str = "channel_upload";
const SPEAK_TOOL: &str = crabgent_tool_tts::TOOL_NAME;
const DESCRIPTION: &str = "Generate a spoken reply with text-to-speech, store it, \
upload the audio file into the current channel conversation, and return timing \
feedback. Use only when the latest user message explicitly asks for a spoken, \
audio, voice, or TTS reply. Default replies must stay text.";

pub struct VoiceReplyTool {
    synth: TtsTool,
    store: Arc<dyn AudioStore>,
    sink: Arc<dyn ChannelSink>,
    policy: Arc<dyn crabgent_core::PolicyHook>,
    alignment: Option<Arc<dyn ForcedAlignmentProvider>>,
    default_format: TtsAudioFormat,
}

pub struct VoiceReplyToolConfig {
    pub store: Arc<dyn AudioStore>,
    pub sink: Arc<dyn ChannelSink>,
    pub policy: Arc<dyn crabgent_core::PolicyHook>,
    pub provider: Arc<dyn TtsProvider>,
    pub alignment: Option<Arc<dyn ForcedAlignmentProvider>>,
    pub model: TtsModelId,
    pub voice: VoiceId,
    pub default_format: TtsAudioFormat,
}

impl VoiceReplyTool {
    #[must_use]
    pub fn new(config: VoiceReplyToolConfig) -> Self {
        let synth = TtsTool::new(
            Arc::clone(&config.store),
            config.provider,
            config.model,
            config.voice,
        );
        Self {
            synth,
            store: config.store,
            sink: config.sink,
            policy: config.policy,
            alignment: config.alignment,
            default_format: config.default_format,
        }
    }

    async fn run(&self, args: VoiceReplyArgs, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        gate_tool_action(
            self.policy.as_ref(),
            ctx,
            &Action::tool(CHANNEL_UPLOAD_TOOL),
        )
        .await?;

        let text = args.text.trim();
        if text.is_empty() {
            return Ok(soft_error_object("empty input text"));
        }

        let format = args.format.unwrap_or(self.default_format);
        let synth_result = self.synthesize(text, args.voice, format, ctx).await?;
        if synth_result.is_error {
            return Ok(synth_result);
        }
        let Some(audio_ref) = audio_ref_from_tool_output(&synth_result.output) else {
            return Ok(soft_error_object("speech synthesis returned no audio_ref"));
        };
        let (bytes, mime) = match self.store.get(&audio_ref).await {
            Ok(value) => value,
            Err(AudioStoreError::NotFound) => {
                return Ok(soft_error_object("stored speech audio expired"));
            }
            Err(error) => {
                crabgent_log::warn!(error = %error, "voice_reply could not read stored TTS audio");
                return Ok(soft_error_object("stored speech audio unavailable"));
            }
        };

        let conv = crabgent_core::Owner::new(args.conv);
        let thread_parent = if args.top_level {
            None
        } else {
            ctx.subject.inbound_message_ref()
        };
        let filename = filename_for(&audio_ref, &mime, format);
        let message = match self
            .sink
            .upload(
                &ctx.subject,
                &conv,
                &filename,
                bytes.to_vec(),
                args.comment.as_deref(),
                thread_parent.as_ref(),
            )
            .await
        {
            Ok(message) => message,
            Err(error) => {
                crabgent_log::warn!(error = %error, "voice_reply upload failed");
                return Ok(soft_error_object("voice upload failed"));
            }
        };

        let alignment = self
            .align_if_enabled(&bytes, &mime, &audio_ref, text)
            .await
            .unwrap_or_else(|error| {
                crabgent_log::warn!(error = %error, "voice_reply forced alignment failed");
                json!({"ok": false, "error": "forced alignment failed"})
            });

        Ok(ToolResult::success(json!({
            "ok": true,
            "audio_ref": audio_ref.as_str(),
            "mime": mime,
            "message": render_message_ref(&message),
            "forced_alignment": alignment,
        })))
    }

    async fn synthesize(
        &self,
        text: &str,
        voice: Option<VoiceId>,
        format: TtsAudioFormat,
        ctx: &ToolCtx,
    ) -> Result<ToolResult, ToolError> {
        let mut args = Map::new();
        args.insert("text".to_owned(), Value::String(text.to_owned()));
        args.insert(
            "format".to_owned(),
            Value::String(format.as_neutral_str().to_owned()),
        );
        if let Some(voice) = voice {
            args.insert("voice".to_owned(), Value::String(voice.as_str().to_owned()));
        }
        self.synth.execute_result(Value::Object(args), ctx).await
    }

    async fn align_if_enabled(
        &self,
        bytes: &Bytes,
        mime: &str,
        audio_ref: &AudioRef,
        text: &str,
    ) -> Result<Value, String> {
        let Some(provider) = &self.alignment else {
            return Ok(Value::Null);
        };
        let payload = AudioPayload::new(bytes.to_vec(), mime, Some(audio_ref.as_str().to_owned()))
            .map_err(|error| error.to_string())?;
        let response = provider
            .align(ForcedAlignmentRequest {
                payload,
                text: text.to_owned(),
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(alignment_summary(&response))
    }
}

#[derive(Debug, Deserialize)]
struct VoiceReplyArgs {
    conv: String,
    text: String,
    #[serde(default)]
    voice: Option<VoiceId>,
    #[serde(default)]
    format: Option<TtsAudioFormat>,
    #[serde(default)]
    comment: Option<String>,
    #[serde(default)]
    top_level: bool,
}

#[async_trait]
impl Tool for VoiceReplyTool {
    fn name(&self) -> &'static str {
        VOICE_REPLY_TOOL
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["conv", "text"],
            "properties": {
                "conv": {
                    "type": "string",
                    "description": "Conversation owner string from the current channel context."
                },
                "text": {
                    "type": "string",
                    "description": "Short text to synthesize into speech."
                },
                "voice": {
                    "type": "string",
                    "description": "Optional provider voice id override."
                },
                "format": {
                    "type": "string",
                    "enum": ["mp3", "opus", "aac", "flac", "wav", "pcm"],
                    "description": "Optional output audio format."
                },
                "comment": {
                    "type": "string",
                    "description": "Optional upload caption. Omit for voice-only replies."
                },
                "top_level": {
                    "type": "boolean",
                    "default": false,
                    "description": "Upload at conversation root instead of replying to the inbound message."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        Ok(self.execute_result(args, ctx).await?.output)
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        let parsed: VoiceReplyArgs = parse_args_with_context(args, "voice_reply args")?;
        self.run(parsed, ctx).await
    }
}

/// Removes voice output tools from provider requests unless the latest user
/// message explicitly asks for spoken output.
pub struct VoiceOutputGateHook;

impl VoiceOutputGateHook {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Hook for VoiceOutputGateHook {
    async fn before_llm(&self, req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        if latest_user_requests_tts(&req.messages) {
            return Decision::Continue;
        }
        if !req.tools.iter().any(|tool| is_voice_tool(&tool.name)) {
            return Decision::Continue;
        }
        let mut next = req.clone();
        next.tools.retain(|tool| !is_voice_tool(&tool.name));
        if matches!(
            next.tool_choice.as_ref(),
            Some(ToolChoice::Tool(name)) if is_voice_tool(name)
        ) {
            next.tool_choice = None;
        }
        Decision::Replace(next)
    }
}

fn is_voice_tool(name: &str) -> bool {
    matches!(name, VOICE_REPLY_TOOL | SPEAK_TOOL)
}

fn audio_ref_from_tool_output(output: &Value) -> Option<AudioRef> {
    output
        .get("audio_ref")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(AudioRef::new)
}

fn filename_for(audio_ref: &AudioRef, mime: &str, format: TtsAudioFormat) -> String {
    let ext = match mime {
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/aac" => "aac",
        "audio/flac" => "flac",
        "audio/wav" => "wav",
        "audio/L16" => "pcm",
        _ => format.as_neutral_str(),
    };
    format!("voice-reply-{}.{}", stem(audio_ref.as_str()), ext)
}

fn stem(audio_ref: &str) -> &str {
    audio_ref
        .rsplit_once('.')
        .map_or(audio_ref, |(head, _ext)| head)
}

fn render_message_ref(r: &MessageRef) -> Value {
    json!({
        "channel": r.channel,
        "conv": r.conv.as_str(),
        "id": r.id,
        "thread_root": r.thread_root,
        "broadcast": r.broadcast,
    })
}

pub fn alignment_summary(response: &ForcedAlignmentResponse) -> Value {
    let duration = response
        .words
        .iter()
        .map(|word| word.end)
        .chain(response.characters.iter().map(|ch| ch.end))
        .fold(0.0_f32, f32::max);
    let word_count = response.words.len();
    let duration_secs = f64::from(duration);
    let speech_rate_wpm = (duration > 0.0 && word_count > 0).then(|| {
        let words = u32::try_from(word_count).map_or_else(|_| f64::from(u32::MAX), f64::from);
        (words / (duration_secs / 60.0)).round()
    });
    let gaps: Vec<f32> = response
        .words
        .windows(2)
        .filter_map(|pair| {
            let gap = pair[1].start - pair[0].end;
            (gap > 0.05).then_some(gap)
        })
        .collect();
    let max_gap_ms = gaps
        .iter()
        .copied()
        .fold(0.0_f32, f32::max)
        .mul_add(1000.0, 0.0);
    let pause_count = gaps.iter().filter(|gap| **gap >= 0.4).count();

    json!({
        "ok": true,
        "duration_ms": (duration_secs * 1000.0).round(),
        "word_count": word_count,
        "speech_rate_wpm": speech_rate_wpm,
        "pause_count": pause_count,
        "max_gap_ms": f64::from(max_gap_ms).round(),
        "loss": response.loss,
    })
}

fn latest_user_requests_tts(messages: &[Value]) -> bool {
    latest_user_text(messages).is_some_and(|text| contains_explicit_tts_request(&text))
}

fn latest_user_text(messages: &[Value]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .map(collect_text)
        .filter(|text| !text.trim().is_empty())
}

fn collect_text(value: &Value) -> String {
    let Some(content) = value.get("content") else {
        return String::new();
    };
    match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                item.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| item.as_str())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn contains_explicit_tts_request(text: &str) -> bool {
    let lower = text.to_lowercase();
    if contains_any(
        &lower,
        &[
            "kein tts",
            "keine tts",
            "ohne tts",
            "nicht mit tts",
            "nur text",
            "text reicht",
            "keine sprachnachricht",
            "keine sprachantwort",
            "kein audio",
            "nicht sprechen",
            "nicht vorlesen",
            "was ist tts",
            "default ist text",
            "standardausgabe ist text",
            "nur auf explizite anweisung",
        ],
    ) {
        return false;
    }

    contains_any(
        &lower,
        &[
            "tts bitte",
            "mit tts",
            "per tts",
            "als tts",
            "text-to-speech",
            "text to speech",
            "sprachnachricht",
            "sprachantwort",
            "sprachausgabe",
            "voice reply",
            "voice message",
            "voice note",
            "audioantwort",
            "audio antwort",
            "als audio",
            "per audio",
            "mit stimme",
            "per sprache",
            "gesprochen antwort",
            "mündlich antwort",
            "lies mir",
            "lies das vor",
            "lies es vor",
            "vorlesen",
            "sprich",
            "sag es laut",
            "sag das laut",
        ],
    )
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use crabgent_core::{ForcedAlignedWord, ToolDef};

    use super::*;

    fn req_with_user(text: &str) -> LlmRequest {
        LlmRequest {
            model: crabgent_core::ModelId::new("m"),
            system_prompt: None,
            messages: vec![json!({
                "role": "user",
                "content": [{"type": "text", "text": text}],
            })],
            tools: vec![
                ToolDef {
                    name: VOICE_REPLY_TOOL.to_owned(),
                    description: String::new(),
                    input_schema: json!({}),
                },
                ToolDef {
                    name: SPEAK_TOOL.to_owned(),
                    description: String::new(),
                    input_schema: json!({}),
                },
                ToolDef {
                    name: "channel_send".to_owned(),
                    description: String::new(),
                    input_schema: json!({}),
                },
            ],
            max_tokens: None,
            temperature: None,
            stop_sequences: Vec::new(),
            reasoning_effort: None,
            web_search: crabgent_core::WebSearchConfig::default(),
            tool_choice: None,
        }
    }

    #[tokio::test]
    async fn gate_hides_voice_tool_without_explicit_request() {
        let hook = VoiceOutputGateHook::new();
        let req = req_with_user("Antworte kurz als Text.");
        let ctx = RunCtx::new(
            crabgent_core::RunId::new(),
            crabgent_core::Subject::new("u"),
        );

        let Decision::Replace(next) = hook.before_llm(&req, &ctx).await else {
            panic!("expected replacement");
        };

        assert!(!next.tools.iter().any(|tool| tool.name == VOICE_REPLY_TOOL));
        assert!(!next.tools.iter().any(|tool| tool.name == SPEAK_TOOL));
        assert!(next.tools.iter().any(|tool| tool.name == "channel_send"));
    }

    #[tokio::test]
    async fn gate_keeps_voice_tool_for_explicit_spoken_request() {
        let hook = VoiceOutputGateHook::new();
        let req = req_with_user("Antworte bitte als Sprachnachricht.");
        let ctx = RunCtx::new(
            crabgent_core::RunId::new(),
            crabgent_core::Subject::new("u"),
        );

        assert!(matches!(
            hook.before_llm(&req, &ctx).await,
            Decision::Continue
        ));
    }

    #[test]
    fn explicit_detector_rejects_negative_tts_mentions() {
        assert!(!contains_explicit_tts_request(
            "Default ist Text, nur auf explizite Anweisung mit TTS."
        ));
        assert!(!contains_explicit_tts_request("Was ist TTS?"));
        assert!(contains_explicit_tts_request("TTS bitte."));
    }

    #[test]
    fn alignment_summary_reports_pacing() {
        let response = ForcedAlignmentResponse {
            characters: Vec::new(),
            words: vec![
                ForcedAlignedWord {
                    text: "hi".to_owned(),
                    start: 0.0,
                    end: 0.2,
                    loss: None,
                },
                ForcedAlignedWord {
                    text: "there".to_owned(),
                    start: 0.8,
                    end: 1.0,
                    loss: None,
                },
            ],
            loss: Some(0.1),
        };

        let summary = alignment_summary(&response);

        assert_eq!(summary["duration_ms"].as_f64(), Some(1000.0));
        assert_eq!(summary["speech_rate_wpm"].as_f64(), Some(120.0));
        assert_eq!(summary["pause_count"], 1);
        assert_eq!(summary["max_gap_ms"].as_f64(), Some(600.0));
    }
}
