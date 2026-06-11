//! Streaming kernel event type.
//!
//! `crabgent-core` emits structured events and never logs directly. Logging is
//! the job of `crabgent-hook-log` plus `crabgent-log`. New variants here surface
//! kernel-internal state to hooks so they can translate it to a transport of
//! their choice (tracing, audit store, channel push) without coupling the
//! kernel to any specific observability sink.

use serde::{Deserialize, Serialize};

use crate::error::ProviderError;
use crate::types::{Citation, Notification, ToolCall, ToolResult};

/// Streaming kernel event surfaced to hooks (and to streaming callers).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    Token(String),
    /// Reasoning fragment from the assistant. Delta-stream peer to
    /// [`Event::Token`]: surfaces "thinking" / "reasoning" content that
    /// providers (Anthropic `thinking_delta`, `OpenAI` Responses
    /// `reasoning_text.delta` + `reasoning_summary_text.delta`) emit
    /// alongside the final answer. Consumer-side sinks such as
    /// `crabgent-thinking::ThinkingHook` map this to a progress step so
    /// channel adapters can surface live reasoning next to tool lifecycle.
    Reasoning(String),
    ToolCallStarted(ToolCall),
    ToolCallCompleted {
        call: ToolCall,
        result: ToolResult,
    },
    Notification(Notification),
    /// A hosted web-search (or other provider server-side tool) returned a
    /// result. The kernel has already appended `Message::ProviderBlock` to
    /// the conversation for multi-turn echo.
    ///
    /// Hooks may observe citations and raw block content here but must not
    /// replace this event with another variant.
    ServerToolResult {
        provider: String,
        name: String,
        citations: Vec<Citation>,
        raw: serde_json::Value,
    },
    /// A provider attempt within the fallback chain failed. Informational:
    /// emitted on every attempt error, regardless of whether the chain falls
    /// back, retries, or terminates. The kernel still resolves the run via
    /// its own retry / fallback logic; this event exists so hooks can record
    /// per-attempt diagnostics without re-classifying the underlying
    /// `ProviderError`.
    ///
    /// `attempt_idx` is 0-based, `total_attempts` is the resolved chain
    /// length, `provider` and `model` describe the attempt that failed.
    /// `error_class` is the kernel-side classification (see
    /// [`AttemptErrorClass`]). `message` is the human-readable error
    /// detail; consumer-side sinks should treat it as untrusted text and
    /// redact before logging. `will_fallback` is `true` when the chain
    /// will try another attempt, `false` when the error terminates the
    /// chain.
    ///
    /// Hooks may observe but must not replace this event with another
    /// variant.
    AttemptFailed {
        attempt_idx: usize,
        total_attempts: usize,
        provider: String,
        model: String,
        error_class: AttemptErrorClass,
        message: String,
        will_fallback: bool,
    },
    /// Terminal event: the run produced a final assistant text. Emitted
    /// last on the streaming channel so consumers can detect completion
    /// without out-of-band signalling.
    ///
    /// `on_event` hooks must not replace this event with another variant.
    /// Non-streaming `Kernel::run()` waits for `Final` to recover the
    /// assistant text and reports an internal error if the stream closes
    /// after a hook replaces this terminal event.
    Final(String),
}

/// Kernel-side classification of a failed provider attempt, mirroring the
/// cases distinguished by the fallback classifier. Carries enough structure
/// for hooks to reason about an `Event::AttemptFailed` without re-inspecting
/// the underlying `ProviderError`.
///
/// `#[non_exhaustive]` so future provider error shapes can extend the
/// classification without breaking external consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AttemptErrorClass {
    /// Provider rate-limited the request. `retry_after_secs` is forwarded
    /// from the source so a hook can decide between short-wait and
    /// long-wait reporting.
    RateLimited { retry_after_secs: Option<u64> },
    /// HTTP client error (4xx, excluding 408 and 429). Status forwarded
    /// verbatim.
    ApiClient { status: u16 },
    /// HTTP server error (5xx). Status forwarded verbatim.
    ApiServer { status: u16 },
    /// Transport-layer failure (connection refused, DNS, TLS).
    Transport,
    /// Provider call exceeded its deadline. Also covers HTTP 408.
    Timeout,
    /// Authentication failure.
    Auth,
    /// Provider response could not be parsed.
    MalformedResponse,
    /// Provider or model does not support tool definitions.
    ToolsUnsupported,
    /// Provider or model does not support image content.
    VisionUnsupported,
    /// Provider or model does not support audio content.
    AudioUnsupported,
    /// Provider or model does not support hosted web search.
    WebSearchUnsupported,
    /// Provider or model does not support the requested reasoning effort.
    ReasoningEffortUnsupported,
    /// Model discovery failed before a provider call.
    ModelDiscovery,
    /// The cancellation token fired during the call.
    Cancelled,
    /// Stream opened cleanly then failed mid-pump in a retryable way.
    RetryableStream,
    /// Catch-all provider error not classified into a more specific bucket.
    Other,
}

// Intentionally NO wildcard `_ => ...` arm in the From impl below: any new
// `ProviderError` variant must compile-break this mapping so a contributor
// consciously picks the right `AttemptErrorClass`.
impl From<&ProviderError> for AttemptErrorClass {
    fn from(err: &ProviderError) -> Self {
        match err {
            ProviderError::RateLimited { retry_after_secs }
            | ProviderError::Api {
                status: 429,
                retry_after_secs,
                ..
            } => Self::RateLimited {
                retry_after_secs: *retry_after_secs,
            },
            ProviderError::Api { status: 408, .. } | ProviderError::Timeout => Self::Timeout,
            ProviderError::Api { status, .. } if (500..=599).contains(status) => {
                Self::ApiServer { status: *status }
            }
            ProviderError::Api { status, .. } => Self::ApiClient { status: *status },
            ProviderError::Transport(_) => Self::Transport,
            ProviderError::Auth(_) => Self::Auth,
            ProviderError::MalformedResponse(_) => Self::MalformedResponse,
            ProviderError::ToolsUnsupported { .. } => Self::ToolsUnsupported,
            ProviderError::VisionUnsupported { .. } => Self::VisionUnsupported,
            ProviderError::AudioUnsupported { .. } => Self::AudioUnsupported,
            ProviderError::WebSearchUnsupported { .. } => Self::WebSearchUnsupported,
            ProviderError::ReasoningEffortUnsupported { .. } => Self::ReasoningEffortUnsupported,
            ProviderError::ModelDiscovery { .. } => Self::ModelDiscovery,
            ProviderError::Cancelled => Self::Cancelled,
            ProviderError::RetryableStream { .. } => Self::RetryableStream,
            ProviderError::Other(_) => Self::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trips_via_json() {
        let ev = Event::Token("hi".into());
        let s = serde_json::to_string(&ev).expect("ser");
        assert!(s.contains("\"kind\":\"token\""));
        let back: Event = serde_json::from_str(&s).expect("de");
        match back {
            Event::Token(t) => assert_eq!(t, "hi"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn event_final_uses_adjacent_tag() {
        let ev = Event::Final("done text".into());
        let v = serde_json::to_value(&ev).expect("to_value");
        assert_eq!(v["kind"], "final");
        assert_eq!(v["data"], "done text");
    }

    #[test]
    fn reasoning_event_round_trips_via_json() {
        let ev = Event::Reasoning("draft thought".into());
        let v = serde_json::to_value(&ev).expect("to_value");
        assert_eq!(v["kind"], "reasoning");
        assert_eq!(v["data"], "draft thought");
        let back: Event = serde_json::from_value(v).expect("de");
        match back {
            Event::Reasoning(text) => assert_eq!(text, "draft thought"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn attempt_error_class_maps_rate_limited() {
        let err = ProviderError::RateLimited {
            retry_after_secs: Some(5),
        };
        assert_eq!(
            AttemptErrorClass::from(&err),
            AttemptErrorClass::RateLimited {
                retry_after_secs: Some(5),
            },
        );
    }

    #[test]
    fn attempt_error_class_maps_api_429_to_rate_limited() {
        let err = ProviderError::Api {
            status: 429,
            message: "too many requests".into(),
            retry_after_secs: Some(7),
        };
        assert_eq!(
            AttemptErrorClass::from(&err),
            AttemptErrorClass::RateLimited {
                retry_after_secs: Some(7),
            },
        );
    }

    #[test]
    fn attempt_error_class_maps_api_408_to_timeout() {
        let err = ProviderError::Api {
            status: 408,
            message: "request timeout".into(),
            retry_after_secs: None,
        };
        assert_eq!(AttemptErrorClass::from(&err), AttemptErrorClass::Timeout);
    }

    #[test]
    fn attempt_error_class_maps_api_5xx_to_api_server() {
        let err = ProviderError::Api {
            status: 503,
            message: "busy".into(),
            retry_after_secs: None,
        };
        assert_eq!(
            AttemptErrorClass::from(&err),
            AttemptErrorClass::ApiServer { status: 503 },
        );
    }

    #[test]
    fn attempt_error_class_maps_api_4xx_to_api_client() {
        let err = ProviderError::Api {
            status: 422,
            message: "unprocessable".into(),
            retry_after_secs: None,
        };
        assert_eq!(
            AttemptErrorClass::from(&err),
            AttemptErrorClass::ApiClient { status: 422 },
        );
    }

    #[test]
    fn attempt_error_class_maps_terminal_variants() {
        assert_eq!(
            AttemptErrorClass::from(&ProviderError::Auth("bad".into())),
            AttemptErrorClass::Auth,
        );
        assert_eq!(
            AttemptErrorClass::from(&ProviderError::MalformedResponse("bad json".into())),
            AttemptErrorClass::MalformedResponse,
        );
        assert_eq!(
            AttemptErrorClass::from(&ProviderError::ModelDiscovery {
                reason: "no endpoint".into()
            }),
            AttemptErrorClass::ModelDiscovery,
        );
        assert_eq!(
            AttemptErrorClass::from(&ProviderError::ReasoningEffortUnsupported {
                provider: "anthropic".into(),
                model: "claude-haiku-4-5".into(),
            }),
            AttemptErrorClass::ReasoningEffortUnsupported,
        );
        assert_eq!(
            AttemptErrorClass::from(&ProviderError::Cancelled),
            AttemptErrorClass::Cancelled,
        );
    }

    #[test]
    fn attempt_error_class_maps_eligible_variants() {
        assert_eq!(
            AttemptErrorClass::from(&ProviderError::Transport("dns".into())),
            AttemptErrorClass::Transport,
        );
        assert_eq!(
            AttemptErrorClass::from(&ProviderError::Timeout),
            AttemptErrorClass::Timeout,
        );
        assert_eq!(
            AttemptErrorClass::from(&ProviderError::RetryableStream {
                message: "overloaded".into()
            }),
            AttemptErrorClass::RetryableStream,
        );
        assert_eq!(
            AttemptErrorClass::from(&ProviderError::Other("misc".into())),
            AttemptErrorClass::Other,
        );
    }

    #[test]
    fn attempt_failed_event_serializes_with_snake_case_class() {
        let ev = Event::AttemptFailed {
            attempt_idx: 0,
            total_attempts: 2,
            provider: "openai".into(),
            model: "gpt-5.5".into(),
            error_class: AttemptErrorClass::ApiServer { status: 503 },
            message: "busy".into(),
            will_fallback: true,
        };
        let v = serde_json::to_value(&ev).expect("to_value");
        assert_eq!(v["kind"], "attempt_failed");
        assert_eq!(v["data"]["attempt_idx"], 0);
        assert_eq!(v["data"]["total_attempts"], 2);
        assert_eq!(v["data"]["error_class"]["kind"], "api_server");
        assert_eq!(v["data"]["error_class"]["data"]["status"], 503);
        assert_eq!(v["data"]["will_fallback"], true);
    }
}
