//! `SignatureVerify` trait and explicit development-only no-op impl.

use async_trait::async_trait;

use crate::ChannelError;

use super::handler::WebhookRequest;

/// Verify the signature of an incoming webhook request.
///
/// Implementations check adapter-specific signature headers (Slack
/// `X-Slack-Signature` HMAC-SHA256, Telegram `X-Telegram-Bot-Api-Secret-Token`,
/// Stripe-style `Stripe-Signature`, ...) against the request body
/// and a configured secret.
///
/// Returning `Err(ChannelError::SignatureMismatch)` is the
/// canonical "this request is not from the expected source"
/// response: the handler should turn that into a `401`.
#[async_trait]
pub trait SignatureVerify: Send + Sync {
    /// Verify `req`. `Ok(())` if the signature is valid.
    async fn verify(&self, req: &WebhookRequest) -> Result<(), ChannelError>;
}

/// No-op `SignatureVerify` for development setups.
///
/// Always returns `Ok(())`. DO NOT USE IN PRODUCTION: it skips
/// authenticity checking and lets any caller drive the webhook.
#[derive(Debug, Clone, Copy)]
pub struct NoopSignatureVerify {
    _private: (),
}

impl NoopSignatureVerify {
    /// Construct the no-op verifier for local development and tests only.
    ///
    /// DO NOT USE IN PRODUCTION. Webhook signatures are not verified.
    ///
    /// ```compile_fail
    /// use crabgent_channel::NoopSignatureVerify;
    ///
    /// let _ = NoopSignatureVerify::default();
    /// ```
    #[must_use]
    pub const fn unsafe_dev_only() -> Self {
        Self { _private: () }
    }
}

#[async_trait]
impl SignatureVerify for NoopSignatureVerify {
    async fn verify(&self, _req: &WebhookRequest) -> Result<(), ChannelError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_request() -> WebhookRequest {
        WebhookRequest::new(HashMap::new(), Vec::new())
    }

    struct AlwaysReject;

    #[async_trait]
    impl SignatureVerify for AlwaysReject {
        async fn verify(&self, _req: &WebhookRequest) -> Result<(), ChannelError> {
            Err(ChannelError::SignatureMismatch)
        }
    }

    #[tokio::test]
    async fn noop_signature_always_passes() {
        let v = NoopSignatureVerify::unsafe_dev_only();
        v.verify(&empty_request()).await.expect("noop ok");
    }

    #[tokio::test]
    async fn noop_unsafe_dev_only_works_via_trait() {
        let v = NoopSignatureVerify::unsafe_dev_only();
        v.verify(&empty_request()).await.expect("noop ok");
    }

    #[tokio::test]
    async fn always_reject_returns_signature_mismatch() {
        let v = AlwaysReject;
        let err = v.verify(&empty_request()).await.expect_err("must fail");
        assert!(matches!(err, ChannelError::SignatureMismatch));
    }

    #[test]
    fn noop_signature_is_clone_and_copy() {
        const fn assert_clone<T: Clone>(_: &T) {}
        const fn assert_copy<T: Copy>(_: &T) {}
        let v = NoopSignatureVerify::unsafe_dev_only();
        assert_clone(&v);
        assert_copy(&v);
    }
}
