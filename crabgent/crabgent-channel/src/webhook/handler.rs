//! Webhook request / response types and the [`WebhookHandler`] trait.

use std::collections::HashMap;

use async_trait::async_trait;

use crate::ChannelError;

/// One inbound HTTP webhook request, decoupled from the server runtime.
///
/// Channel-adapter crates own the HTTP server (axum, actix, ...);
/// they construct a `WebhookRequest` from their server's request
/// type and pass it to [`WebhookHandler::handle`].
#[derive(Debug, Clone)]
pub struct WebhookRequest {
    /// Lower-cased header map. Adapters lowercase header keys
    /// before insertion so handlers can rely on stable casing.
    pub headers: HashMap<String, String>,
    /// Raw request body (bytes). Handlers parse adapter-specific
    /// payload formats from this.
    pub body: Vec<u8>,
}

impl WebhookRequest {
    /// Build a request from headers + body.
    #[must_use]
    pub const fn new(headers: HashMap<String, String>, body: Vec<u8>) -> Self {
        Self { headers, body }
    }

    /// Build a request from raw header pairs + body.
    ///
    /// Header names are normalized to lowercase on insertion so
    /// lookup remains case-insensitive for adapter code.
    #[must_use]
    pub fn from_raw<I, K, V>(headers: I, body: Vec<u8>) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: Into<String>,
    {
        let headers = headers
            .into_iter()
            .map(|(k, v)| (k.as_ref().to_lowercase(), v.into()))
            .collect();
        Self { headers, body }
    }

    /// Look up a header by lower-cased name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// Borrow the request body as a byte slice.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// One outbound HTTP response a `WebhookHandler` can return.
#[derive(Debug, Clone)]
pub struct WebhookResponse {
    /// HTTP status code (`200` for ack, `401` for bad signature, ...).
    pub status: u16,
    /// Response body bytes (often empty).
    pub body: Vec<u8>,
}

impl WebhookResponse {
    /// Build a response from status + body.
    #[must_use]
    pub const fn new(status: u16, body: Vec<u8>) -> Self {
        Self { status, body }
    }

    /// Empty `200 OK` ack response.
    #[must_use]
    pub const fn ok() -> Self {
        Self::new(200, Vec::new())
    }

    /// `401 Unauthorized` empty body.
    #[must_use]
    pub const fn unauthorized() -> Self {
        Self::new(401, Vec::new())
    }
}

impl Default for WebhookResponse {
    fn default() -> Self {
        Self::ok()
    }
}

/// Adapter-side webhook handler.
///
/// Channel crates implement one or more handlers under
/// adapter-specific paths. The HTTP server in the host application
/// routes inbound requests by path, calls `handle`, and returns the
/// handler's `WebhookResponse` to the client.
#[async_trait]
pub trait WebhookHandler: Send + Sync {
    /// Path this handler responds to (e.g. `/slack/events`).
    fn path(&self) -> &'static str;

    /// Process one webhook request. Implementations are expected
    /// to verify signatures (typically via a [`super::SignatureVerify`])
    /// before acting on the body, and to return a fast response: a
    /// slow handler can violate adapter-side timeouts (Slack 3 s).
    async fn handle(&self, req: WebhookRequest) -> Result<WebhookResponse, ChannelError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    struct EchoHandler;

    #[async_trait]
    impl WebhookHandler for EchoHandler {
        fn path(&self) -> &'static str {
            "/echo"
        }
        async fn handle(&self, req: WebhookRequest) -> Result<WebhookResponse, ChannelError> {
            Ok(WebhookResponse::new(200, req.body))
        }
    }

    #[test]
    fn webhook_request_header_lookup_works() {
        let req = WebhookRequest::new(
            build_headers(&[("x-slack-signature", "v0=abc"), ("content-type", "json")]),
            b"{}".to_vec(),
        );
        assert_eq!(req.header("x-slack-signature"), Some("v0=abc"));
        assert_eq!(req.header("content-type"), Some("json"));
        assert_eq!(req.header("missing"), None);
        assert_eq!(req.body(), b"{}");
    }

    #[test]
    fn webhook_request_clone_is_independent() {
        let req1 = WebhookRequest::new(build_headers(&[("a", "1")]), b"body".to_vec());
        let req2 = req1.clone();
        assert_eq!(req1.body, req2.body);
        assert_eq!(req1.headers, req2.headers);
    }

    #[test]
    fn webhook_request_from_raw_normalizes_headers() {
        let req = WebhookRequest::from_raw(
            [("X-Slack-Signature", "v0=abc"), ("Content-Type", "json")],
            b"{}".to_vec(),
        );
        assert_eq!(req.header("x-slack-signature"), Some("v0=abc"));
        assert_eq!(req.header("content-type"), Some("json"));
        assert_eq!(req.header("missing"), None);
    }

    #[test]
    fn webhook_response_ok_is_200_empty() {
        let r = WebhookResponse::ok();
        assert_eq!(r.status, 200);
        assert!(r.body.is_empty());
    }

    #[test]
    fn webhook_response_unauthorized_is_401() {
        let r = WebhookResponse::unauthorized();
        assert_eq!(r.status, 401);
        assert!(r.body.is_empty());
    }

    #[test]
    fn webhook_response_default_is_ok() {
        let r = WebhookResponse::default();
        assert_eq!(r.status, 200);
        assert!(r.body.is_empty());
    }

    #[test]
    fn webhook_response_new_carries_status_and_body() {
        let r = WebhookResponse::new(202, b"queued".to_vec());
        assert_eq!(r.status, 202);
        assert_eq!(r.body, b"queued");
    }

    #[tokio::test]
    async fn handler_dispatches() {
        let h = EchoHandler;
        assert_eq!(h.path(), "/echo");
        let req = WebhookRequest::new(HashMap::new(), b"hello".to_vec());
        let resp = h.handle(req).await.expect("ok");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"hello");
    }
}
