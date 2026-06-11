//! Slack `TypingIndicator` stub.
//!
//! Slack does not expose a typing-indicator API for bot tokens (xoxb).
//! The `typing.start` Web API method exists but rejects bot tokens with
//! `not_allowed_token_type`; only user tokens (xoxp) may call it. The
//! Socket-Mode event stream is inbound-only and carries no
//! "bot is typing" envelope.
//!
//! As a result, the Slack indicator is a no-op. It implements
//! [`crabgent_thinking::TypingIndicator`] so callers can wire it through
//! [`crabgent_thinking::TypingHook`] with the same code path as other
//! adapters; the no-op simply discards every call.
//!
//! A reaction-based workaround (e.g. an hourglass emoji on
//! the inbound message that triggered the run) needs `MessageRef` in
//! the hook context, which the current `RunCtx` does not carry. That
//! workaround can be added as a separate feature.

use async_trait::async_trait;
use crabgent_core::RunCtx;
use crabgent_thinking::{TypingIndicator, TypingResult};

/// Slack typing-indicator no-op.
///
/// Bot tokens cannot drive `typing.start`. See module-level docs for
/// follow-up options.
#[derive(Debug, Default, Clone, Copy)]
pub struct SlackTypingIndicator;

#[async_trait]
impl TypingIndicator for SlackTypingIndicator {
    async fn start(&self, _ctx: &RunCtx) -> TypingResult<()> {
        Ok(())
    }

    async fn stop(&self, _ctx: &RunCtx) -> TypingResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::{RunId, Subject};

    fn ctx() -> RunCtx {
        RunCtx::new(RunId::new(), Subject::new("agent"))
    }

    #[tokio::test]
    async fn start_is_noop() {
        SlackTypingIndicator.start(&ctx()).await.expect("start ok");
    }

    #[tokio::test]
    async fn stop_is_noop() {
        SlackTypingIndicator.stop(&ctx()).await.expect("stop ok");
    }

    #[tokio::test]
    async fn indicator_is_object_safe() {
        let ind: std::sync::Arc<dyn TypingIndicator> = std::sync::Arc::new(SlackTypingIndicator);
        ind.start(&ctx()).await.expect("trait object start");
        ind.stop(&ctx()).await.expect("trait object stop");
    }
}
