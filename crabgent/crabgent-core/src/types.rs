//! Core types passed across the kernel: LLM requests, responses, tool I/O,
//! usage accounting, and notifications.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::Message;
use crate::model::{ModelId, ReasoningEffort};

/// Configuration for hosted web-search capability forwarded to the provider.
///
/// When `enabled` is false (the default) the provider receives no web-search
/// instruction and the fields are ignored. Providers that do not support hosted
/// web search reject the request via [`ProviderError::WebSearchUnsupported`].
///
/// [`ProviderError::WebSearchUnsupported`]: crate::error::ProviderError::WebSearchUnsupported
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSearchConfig {
    /// Enable hosted web search for this request. Default false.
    #[serde(default)]
    pub enabled: bool,
    /// Optional cap on the number of search results the provider may consume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
    /// Provider should only return results from these domains (empty = no
    /// restriction). Provider support varies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_domains: Vec<String>,
    /// Provider must not return results from these domains (empty = no
    /// restriction). Provider support varies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_domains: Vec<String>,
}

/// A single web-search citation attached to a
/// [`ProviderEvent::ServerToolResult`].
///
/// Provider wire format is opaque; the raw value is preserved for
/// external consumers that need provider-specific fields.
///
/// [`ProviderEvent::ServerToolResult`]: crate::provider::ProviderEvent::ServerToolResult
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_text: Option<String>,
    pub provider: String,
    pub raw: serde_json::Value,
}

/// A request to a Provider's `complete()` or `stream()` method.
///
/// The kernel passes this to the Provider after running the `before_llm`
/// hook chain. Messages are loose JSON values so hooks and providers can
/// freely manipulate them across heterogeneous wire formats.
///
/// Owned-only (no lifetime) so hooks can `Decision::Replace(LlmRequest)`
/// without `'static`-lifetime tricks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmRequest {
    pub model: ModelId,
    pub system_prompt: Option<String>,
    pub messages: Vec<Value>,
    pub tools: Vec<ToolDef>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub stop_sequences: Vec<String>,
    /// Per-request reasoning-effort override. `None` falls back to the
    /// model's registry default during `request_for_attempt`. Providers
    /// that do not understand the field ignore it.
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Hosted web-search configuration forwarded to the provider.
    /// Omitted from wire when not enabled (`#[serde(default)]`).
    #[serde(default)]
    pub web_search: WebSearchConfig,
    /// Per-request tool-call forcing. `None` keeps provider-default behaviour
    /// (equivalent to `Auto`). Providers that cannot express a mode map to the
    /// closest available shape and document the mapping.
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,
}

/// Provider-neutral tool-call forcing mode for a single request.
///
/// The serde form here is the neutral persistence shape (`"auto"`, `"any"`,
/// `"none"`, and `{"tool": "<name>"}` for [`ToolChoice::Tool`]). Provider crates
/// translate each variant to their own wire shape via explicit match arms; they
/// never serialize this enum directly onto the wire.
///
/// Do not glob-import this enum: the `None` variant would shadow `Option::None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// Model decides whether to call a tool. Provider-default behaviour.
    Auto,
    /// Force the model to call one of the advertised tools.
    Any,
    /// Force the model to call this specific tool by name.
    Tool(String),
    /// Forbid tool calls for this request.
    None,
}

/// Per-run access policy for kernel-registered tools.
///
/// `All` preserves the kernel default: every registered tool is advertised and
/// executable. `None` hides all tools. `Only` limits the run to the named tool
/// set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToolAccess {
    #[default]
    All,
    None,
    Only {
        tools: Vec<String>,
    },
}

impl ToolAccess {
    #[must_use]
    pub const fn all() -> Self {
        Self::All
    }

    #[must_use]
    pub const fn none() -> Self {
        Self::None
    }

    #[must_use]
    pub fn only(tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Only {
            tools: tools.into_iter().map(Into::into).collect(),
        }
    }

    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Only { tools } => tools.iter().any(|tool| tool == name),
        }
    }
}

/// Tool definition advertised to the LLM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Result of a `Provider::complete()` call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: StopReason,
    pub usage: Usage,
    pub model: ModelId,
}

/// Why the LLM stopped generating in this turn.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Other,
}

/// A tool invocation requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
    /// Opaque provider reasoning-correlation token that must be replayed
    /// with some provider tool-call history entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

/// Result of a tool execution, sent back to the LLM in the next turn.
///
/// `is_error=true` signals a recoverable tool failure that the LLM can repair
/// with corrected args. Hard execution failures stay `Err(ToolError)` and stop
/// the run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResult {
    /// Provider-assigned tool call id. The kernel stamps this after dispatch.
    pub call_id: String,
    /// Tool output payload sent back to the LLM.
    pub output: Value,
    /// Recoverable tool failure flag for provider tool-result messages.
    pub is_error: bool,
    /// Extra run-log messages emitted by the tool owner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub run_messages: Vec<Message>,
}

impl ToolResult {
    /// Build a successful result. The kernel fills `call_id` after execution.
    #[must_use]
    pub const fn success(output: Value) -> Self {
        Self {
            call_id: String::new(),
            output,
            is_error: false,
            run_messages: Vec::new(),
        }
    }

    /// Build a recoverable tool error. The kernel fills `call_id` after execution.
    #[must_use]
    pub const fn soft_error(output: Value) -> Self {
        Self {
            call_id: String::new(),
            output,
            is_error: true,
            run_messages: Vec::new(),
        }
    }

    /// Stamp the provider tool-call id after dispatch.
    #[must_use]
    pub fn with_call_id(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = call_id.into();
        self
    }

    /// Attach a run-log message owned by the tool implementation.
    #[must_use]
    pub fn with_run_message(mut self, message: Message) -> Self {
        self.run_messages.push(message);
        self
    }
}

/// Token usage stats for a single LLM call.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cache_read_tokens: u32,
}

/// A user-facing notification emitted by tools or the kernel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Notification {
    pub kind: String,
    pub message: String,
    pub level: NotificationLevel,
}

/// Severity classification for a `Notification`.
///
/// Future releases may add levels. External matches should keep a
/// wildcard arm.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    Info,
    Warn,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn llm_request_serializes_basic_fields() {
        let req = LlmRequest {
            model: ModelId::new("claude"),
            system_prompt: Some("you are helpful".into()),
            messages: vec![json!({"role": "user", "content": "hi"})],
            tools: vec![],
            max_tokens: Some(4096),
            temperature: Some(0.5),
            stop_sequences: vec![],
            reasoning_effort: None,
            web_search: WebSearchConfig::default(),
            tool_choice: None,
        };
        let s = serde_json::to_string(&req).expect("serialize");
        assert!(s.contains("claude"));
        assert!(s.contains("you are helpful"));
        assert!(s.contains("4096"));
    }

    #[test]
    fn llm_request_round_trips_model_id_as_string() {
        let req = LlmRequest {
            model: ModelId::new("haiku"),
            system_prompt: None,
            messages: vec![],
            tools: vec![],
            max_tokens: None,
            temperature: None,
            stop_sequences: vec![],
            reasoning_effort: None,
            web_search: WebSearchConfig::default(),
            tool_choice: None,
        };
        let s = serde_json::to_string(&req).expect("ser");
        // ModelId serialises transparently as its string body.
        assert!(s.contains("\"model\":\"haiku\""));
        let back: LlmRequest = serde_json::from_str(&s).expect("de");
        assert_eq!(back.model, ModelId::new("haiku"));
    }

    #[test]
    fn stop_reason_round_trips_snake_case() {
        let reasons = [
            StopReason::EndTurn,
            StopReason::ToolUse,
            StopReason::MaxTokens,
            StopReason::StopSequence,
            StopReason::Other,
        ];
        for r in reasons {
            let s = serde_json::to_string(&r).expect("ser");
            let back: StopReason = serde_json::from_str(&s).expect("de");
            assert_eq!(r, back);
        }
    }

    #[test]
    fn stop_reason_serializes_snake_case() {
        let s = serde_json::to_string(&StopReason::EndTurn).expect("ser");
        assert_eq!(s, "\"end_turn\"");
        let s = serde_json::to_string(&StopReason::ToolUse).expect("ser");
        assert_eq!(s, "\"tool_use\"");
    }

    #[test]
    fn tool_choice_round_trips_snake_case() {
        let choices = [
            ToolChoice::Auto,
            ToolChoice::Any,
            ToolChoice::None,
            ToolChoice::Tool("memory_search".to_string()),
        ];
        for c in &choices {
            let s = serde_json::to_string(c).expect("ser");
            let back: ToolChoice = serde_json::from_str(&s).expect("de");
            assert_eq!(*c, back);
        }
    }

    #[test]
    fn tool_choice_serializes_expected_shapes() {
        assert_eq!(
            serde_json::to_string(&ToolChoice::Auto).expect("ser"),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::Any).expect("ser"),
            "\"any\""
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::None).expect("ser"),
            "\"none\""
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::Tool("cron_schedule".to_string())).expect("ser"),
            "{\"tool\":\"cron_schedule\"}"
        );
    }

    #[test]
    fn tool_access_serializes_expected_shapes() {
        assert_eq!(
            serde_json::to_value(ToolAccess::all()).expect("ser"),
            json!({"mode": "all"})
        );
        assert_eq!(
            serde_json::to_value(ToolAccess::none()).expect("ser"),
            json!({"mode": "none"})
        );
        assert_eq!(
            serde_json::to_value(ToolAccess::only(["task", "memory"])).expect("ser"),
            json!({"mode": "only", "tools": ["task", "memory"]})
        );
    }

    #[test]
    fn tool_access_allows_matches_mode() {
        assert!(ToolAccess::all().allows("bash"));
        assert!(!ToolAccess::none().allows("bash"));
        assert!(ToolAccess::only(["task"]).allows("task"));
        assert!(!ToolAccess::only(["task"]).allows("bash"));
    }

    #[test]
    fn llm_request_tool_choice_defaults_to_none_on_deserialize() {
        // Backward-compat: persisted requests written before `tool_choice`
        // existed deserialize with the field defaulted to `None`.
        let json = r#"{"model":"haiku","system_prompt":null,"messages":[],"tools":[],"max_tokens":null,"temperature":null,"stop_sequences":[]}"#;
        let req: LlmRequest = serde_json::from_str(json).expect("de");
        assert_eq!(req.tool_choice, None);
    }

    #[test]
    fn usage_default_is_zero() {
        let u = Usage::default();
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 0);
        assert_eq!(u.cache_creation_tokens, 0);
        assert_eq!(u.cache_read_tokens, 0);
    }

    #[test]
    fn tool_call_round_trips() {
        let call = ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            args: json!({"command": "ls"}),
            thought_signature: None,
        };
        let s = serde_json::to_string(&call).expect("ser");
        let back: ToolCall = serde_json::from_str(&s).expect("de");
        assert_eq!(call, back);
    }

    #[test]
    fn tool_call_preserves_optional_thought_signature() {
        let call = ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            args: json!({}),
            thought_signature: Some("opaque".into()),
        };
        let s = serde_json::to_string(&call).expect("ser");
        assert!(s.contains("thought_signature"));
        let back: ToolCall = serde_json::from_str(&s).expect("de");
        assert_eq!(back.thought_signature.as_deref(), Some("opaque"));
    }

    #[test]
    fn tool_result_carries_error_flag() {
        let r = ToolResult {
            call_id: "c1".into(),
            output: json!({"err": "permission denied"}),
            is_error: true,
            run_messages: Vec::new(),
        };
        let s = serde_json::to_string(&r).expect("ser");
        assert!(s.contains("permission denied"));
        assert!(s.contains("true"));
    }

    #[test]
    fn tool_result_success_defaults_to_non_error_without_call_id() {
        let r = ToolResult::success(json!({"ok": true}));
        assert_eq!(r.call_id, "");
        assert_eq!(r.output, json!({"ok": true}));
        assert!(!r.is_error);
    }

    #[test]
    fn tool_result_soft_error_sets_error_flag() {
        let r = ToolResult::soft_error(json!("validation failed"));
        assert_eq!(r.call_id, "");
        assert_eq!(r.output, json!("validation failed"));
        assert!(r.is_error);
    }

    #[test]
    fn tool_result_with_call_id_stamps_provider_call_id() {
        let r = ToolResult::soft_error(json!("validation failed")).with_call_id("c1");
        assert_eq!(r.call_id, "c1");
        assert!(r.is_error);
    }

    #[test]
    fn tool_result_can_carry_run_log_messages() {
        let message = Message::ChannelOutbound {
            conv: crate::owner::Owner::new("slack:T1/C1"),
            body: "sent".into(),
            channel: "slack".into(),
            message_id: "1".into(),
            thread_root: None,
            broadcast: false,
        };
        let r = ToolResult::success(json!({"ok": true})).with_run_message(message.clone());
        assert_eq!(r.run_messages, vec![message]);
    }

    #[test]
    fn notification_level_round_trip() {
        let levels = [
            NotificationLevel::Info,
            NotificationLevel::Warn,
            NotificationLevel::Error,
        ];
        for level in levels {
            let s = serde_json::to_string(&level).expect("ser");
            let back: NotificationLevel = serde_json::from_str(&s).expect("de");
            assert_eq!(level, back);
        }
    }

    #[test]
    fn tool_def_carries_schema() {
        let td = ToolDef {
            name: "read_file".into(),
            description: "read a file".into(),
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            }),
        };
        let s = serde_json::to_string(&td).expect("ser");
        assert!(s.contains("read_file"));
        assert!(s.contains("path"));
    }
}
