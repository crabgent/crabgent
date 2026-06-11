//! Error type for the channel surface.

use crabgent_core::error::KernelError;
use crabgent_core::subject::InvalidSubjectError;
use thiserror::Error;

/// Errors a `Channel`, `ChannelInbox`, or `ChannelSink` implementation
/// can return.
///
/// `#[non_exhaustive]`: external adapters may surface their own
/// failure modes via `Adapter`. Future variants may be added without
/// breaking consumer code.
///
/// LLM leak prevention: channel tools may forward this enum's `Display`
/// text into LLM-visible `ToolResult` payloads. Adapter, STT, and envelope
/// parser failures therefore render opaque messages while preserving their
/// detail in `Debug` and variant fields for operator-side handling.
///
/// Variants that wrap another domain's error (`Kernel`, `Serde`) keep that
/// inner `Display`; direct callers must only expose those variants when the
/// wrapped error text is safe for the target audience. The built-in channel
/// tools map both variants to opaque execution labels before creating
/// LLM-visible soft errors.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ChannelError {
    /// The named channel adapter is not registered with the router.
    #[error("channel '{0}' is not registered")]
    NotRegistered(String),

    /// The conversation is unknown to the adapter.
    #[error("conversation not found: {0}")]
    ConversationNotFound(String),

    /// The owner string does not match the adapter's expected format.
    #[error("invalid owner format: {0}")]
    InvalidOwnerFormat(String),

    /// Adapter-side error (network, REST, transport, ...).
    ///
    /// `Display` is intentionally opaque so reqwest URLs, homeserver
    /// hostnames, HTTP response bodies, and filesystem paths do not
    /// reach the LLM via `soft_result`. The full underlying message
    /// stays in the `String` field for `Debug` and is logged at
    /// construction by `ChannelError::adapter` via `tracing::warn`.
    #[error("channel adapter error")]
    Adapter(String),

    /// The adapter does not implement the named operation.
    #[error("operation '{0}' not supported on this channel")]
    Unsupported(&'static str),

    /// Speech-to-text processing failed before the event reached the kernel.
    ///
    /// `Display` is intentionally opaque because the wrapped string can carry
    /// upstream provider text.
    #[error("speech-to-text failed")]
    SttFailed(String),

    /// `PolicyHook` denied the channel action.
    ///
    /// `reason` carries the policy implementor's denial text from
    /// `PolicyDecision::Deny(reason)` verbatim. Per `security.md`
    /// PolicyHook-Implementor-Responsibility the implementor owns the
    /// LLM-safety contract of that string; the channel boundary only
    /// forwards it.
    #[error("policy denied [{action}]: {reason}")]
    PolicyDenied {
        /// The action name (e.g. "channel.send").
        action: String,
        /// The denial reason supplied by the `PolicyHook` implementor.
        reason: String,
    },

    /// The envelope or outbound message is malformed.
    ///
    /// `Display` is intentionally opaque because public constructors can carry
    /// external parser text. The detail remains in the variant field and
    /// `Debug` output for operator diagnostics.
    #[error("invalid envelope")]
    InvalidEnvelope(String),

    /// The derived subject identity is malformed.
    #[error("invalid subject: {0}")]
    InvalidSubject(#[from] InvalidSubjectError),

    /// A stop-pattern regular expression failed to compile.
    #[error("invalid stop pattern: {0}")]
    InvalidPattern(#[from] regex::Error),

    /// Webhook signature verification failed.
    #[error("signature verification failed")]
    SignatureMismatch,

    /// The cancellation token fired during the operation.
    #[error("cancelled")]
    Cancelled,

    /// The inbox is shutting down and no longer accepts inbound events.
    #[error("channel inbox is shutting down")]
    ShuttingDown,

    /// Kernel-level error propagated from a kernel run.
    #[error("kernel error: {0}")]
    Kernel(#[from] KernelError),

    /// JSON serialization or deserialization failure.
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),

    /// The inbound body exceeded the channel-layer byte cap.
    ///
    /// Raised by `check_inbound_size` BEFORE `sanitize_for_prompt`.
    /// Caller decides whether to log + drop or surface a refusal.
    #[error("inbound body too large: {observed} bytes exceeds {max} bytes")]
    InboundTooLarge {
        /// Observed byte length of the offending body.
        observed: usize,
        /// Configured maximum (`INBOUND_BODY_MAX_BYTES`).
        max: usize,
    },
}

impl ChannelError {
    /// Build an `Adapter` error from any `Display` value.
    ///
    /// The underlying detail is captured for `Debug` / operator-side
    /// inspection only: `Display` renders the opaque
    /// `"channel adapter error"` so reqwest URLs, homeserver hostnames,
    /// HTTP response bodies, and filesystem paths cannot reach the LLM
    /// via `soft_result`. To preserve the original message for ops, the
    /// helper emits a `warn!` log scoped to `crabgent_channel::error`
    /// before constructing the variant. The macro routes through the
    /// crate-local `extern crate crabgent_log as tracing` alias declared
    /// in `lib.rs`, so logging stays inside `crabgent-log`.
    pub fn adapter(msg: impl std::fmt::Display) -> Self {
        let detail = msg.to_string();
        tracing::warn!(
            target: "crabgent_channel::error",
            error = %detail,
            "channel adapter error suppressed for LLM"
        );
        Self::Adapter(detail)
    }

    /// Build a `PolicyDenied` error from an action name and the
    /// implementor-supplied denial reason.
    pub fn policy_denied(action: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::PolicyDenied {
            action: action.into(),
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_registered_renders_channel_name() {
        let e = ChannelError::NotRegistered("slack".into());
        assert!(e.to_string().contains("slack"));
    }

    #[test]
    fn adapter_helper_constructs() {
        let e = ChannelError::adapter("rest 502");
        assert!(matches!(e, ChannelError::Adapter(_)));
        // Display is opaque: the underlying detail must not appear in the
        // LLM-visible rendering. The string is preserved in Debug for ops.
        assert_eq!(e.to_string(), "channel adapter error");
        assert!(!e.to_string().contains("rest 502"));
        let debug = format!("{e:?}");
        assert!(
            debug.contains("rest 502"),
            "Debug must preserve detail: {debug}"
        );
    }

    #[test]
    fn adapter_display_does_not_leak_token() {
        // Pin the LLM-safety contract: even if a Slack/matrix transport
        // error embeds a Bearer token or homeserver URL into its string,
        // the `Display` rendering forwarded to the LLM via `soft_result`
        // must not echo it.
        let leaky = "GET https://internal.example.com/api?token=Bearer+secret-xyz failed";
        let e = ChannelError::adapter(leaky);
        let rendered = e.to_string();
        assert_eq!(rendered, "channel adapter error");
        assert!(!rendered.contains("secret-xyz"));
        assert!(!rendered.contains("internal.example.com"));
        assert!(!rendered.contains("Bearer"));
    }

    #[test]
    fn unsupported_renders_operation_name() {
        let e = ChannelError::Unsupported("react");
        assert!(e.to_string().contains("react"));
    }

    #[test]
    fn policy_denied_renders_action_and_reason() {
        let e = ChannelError::policy_denied("channel.send", "scope blocked for direct channel");
        assert!(matches!(e, ChannelError::PolicyDenied { .. }));
        let rendered = e.to_string();
        assert!(rendered.contains("channel.send"), "{rendered}");
        assert!(
            rendered.contains("scope blocked for direct channel"),
            "{rendered}"
        );
    }

    #[test]
    fn signature_mismatch_renders() {
        assert_eq!(
            ChannelError::SignatureMismatch.to_string(),
            "signature verification failed"
        );
    }

    #[test]
    fn invalid_envelope_display_is_opaque_but_debug_preserves_reason() {
        let e = ChannelError::InvalidEnvelope("missing body".into());
        assert_eq!(e.to_string(), "invalid envelope");
        assert!(format!("{e:?}").contains("missing body"));
    }

    #[test]
    fn stt_failed_display_is_opaque_but_debug_preserves_reason() {
        let e = ChannelError::SttFailed("provider token expired".into());
        assert_eq!(e.to_string(), "speech-to-text failed");
        assert!(format!("{e:?}").contains("provider token expired"));
    }

    #[test]
    fn invalid_subject_converts() {
        let e: ChannelError = InvalidSubjectError.into();
        assert!(matches!(e, ChannelError::InvalidSubject(_)));
        assert!(e.to_string().contains("subject id"));
    }

    #[test]
    fn conversation_not_found_renders_id() {
        let e = ChannelError::ConversationNotFound("slack:T1/C2".into());
        assert!(e.to_string().contains("slack:T1/C2"));
    }

    #[test]
    fn cancelled_renders_short() {
        assert_eq!(ChannelError::Cancelled.to_string(), "cancelled");
    }

    #[test]
    fn shutting_down_renders_clear_message() {
        assert_eq!(
            ChannelError::ShuttingDown.to_string(),
            "channel inbox is shutting down"
        );
    }

    #[test]
    fn kernel_error_converts_via_from() {
        let ke = KernelError::Internal("boom".into());
        let ce: ChannelError = ke.into();
        assert!(matches!(ce, ChannelError::Kernel(_)));
    }

    #[test]
    fn inbound_too_large_renders_observed_and_max() {
        let e = ChannelError::InboundTooLarge {
            observed: 9000,
            max: 8192,
        };
        let rendered = e.to_string();
        assert!(rendered.contains("9000"), "{rendered}");
        assert!(rendered.contains("8192"), "{rendered}");
    }

    #[test]
    fn serde_error_converts_via_from() {
        let invalid: Result<serde_json::Value, _> = serde_json::from_str("not json");
        let serde_err = invalid.expect_err("expected error");
        let ce: ChannelError = serde_err.into();
        assert!(matches!(ce, ChannelError::Serde(_)));
    }
}
