//! Authentication strategies for OpenAI-compatible endpoint families.

use std::fmt;

use async_trait::async_trait;
use crabgent_core::RunCtx;
use reqwest::header::{AUTHORIZATION, HeaderName, HeaderValue, USER_AGENT};
use secrecy::{ExposeSecret, SecretString};

use crabgent_core::ProviderError;

use crate::wire::WireFormatDyn;
use crate::wire::chat_completions::ChatCompletionsWire;
use crate::wire::responses::ResponsesWire;

const OPENAI_BASE_URL: &str = "https://api.openai.com";
const CHATGPT_BASE_URL: &str = "https://chatgpt.com";
const OPENAI_BETA_HEADER: HeaderName = HeaderName::from_static("openai-beta");
const ORIGINATOR_HEADER: HeaderName = HeaderName::from_static("originator");
const CHATGPT_ACCOUNT_ID_HEADER: HeaderName = HeaderName::from_static("chatgpt-account-id");
const SESSION_ID_HEADER_UNDERSCORE: HeaderName = HeaderName::from_static("session_id");
const SESSION_ID_HEADER_DASH: HeaderName = HeaderName::from_static("session-id");
const THREAD_ID_HEADER_UNDERSCORE: HeaderName = HeaderName::from_static("thread_id");
const THREAD_ID_HEADER_DASH: HeaderName = HeaderName::from_static("thread-id");
const CLIENT_REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-client-request-id");
const CODEX_WINDOW_ID_HEADER: HeaderName = HeaderName::from_static("x-codex-window-id");
const CODEX_ORIGINATOR: &str = "codex_cli_rs";
const CODEX_USER_AGENT: &str = "codex_cli_rs/0.59.0";

/// Fixed installation UUID emitted in the `x-codex-window-id` header and the
/// `client_metadata.x-codex-installation-id` body field for the Codex Responses
/// backend. The value matches the constant used by `codex_cli_rs` upstream and
/// is verified against an unauthenticated probe of the endpoint.
pub const CODEX_INSTALLATION_ID: &str = "9d3e7a2c-5f4b-4a1d-9c8e-2b1a4f6d3e5c";

/// Derive a stable per-conversation identifier from the run context. Prefers
/// the session id when a `SessionPersistHook` has populated it; otherwise
/// falls back to the run id so each kernel call still gets a unique cache
/// scope key. The Codex Responses backend uses this string to scope prompt
/// caching: identical headers across two calls bind both to the same prefix
/// cache.
#[must_use]
pub fn cache_scope_id_from(ctx: &RunCtx) -> String {
    ctx.session_id()
        .map_or_else(|| ctx.run_id.to_string(), str::to_owned)
}

/// Authentication strategy paired with one concrete wire format.
#[async_trait]
pub trait AuthStrategy: Send + Sync {
    fn base_url(&self) -> &str;
    fn auth_headers(&self) -> Vec<(HeaderName, HeaderValue)>;
    /// Per-call headers derived from the run context. Default returns no
    /// headers; `CodexOAuth` overrides with session/thread/window identity
    /// so the Codex Responses backend can scope prompt caching by
    /// conversation. ApiKey-based strategies on the public `OpenAI` API
    /// need no such headers and inherit the empty default.
    fn request_headers(&self, _ctx: &RunCtx) -> Vec<(HeaderName, HeaderValue)> {
        Vec::new()
    }
    fn wire(&self) -> &dyn WireFormatDyn;
    fn supports_model_discovery(&self) -> bool {
        true
    }
    /// Absolute URL for batch speech-to-text submissions. The default
    /// targets the public Responses-compatible REST endpoint
    /// `<base_url>/v1/audio/transcriptions` and ignores any provided
    /// model id. The Codex OAuth backend overrides this to hit
    /// `chatgpt.com/backend-api/transcribe`, the only STT path that
    /// honours the `ChatGPT` subscription bearer.
    fn stt_endpoint_url(&self) -> String {
        format!(
            "{}/v1/audio/transcriptions",
            self.base_url().trim_end_matches('/')
        )
    }
    /// Whether the backend expects a `model` form field on
    /// transcription requests. `chatgpt.com/backend-api/transcribe`
    /// pins the model server-side and rejects the field; the public
    /// API requires it. Default mirrors the public API.
    fn stt_accepts_model_field(&self) -> bool {
        true
    }
    /// Whether `Provider::complete` must internally consume a streaming
    /// response. The Codex backend
    /// (`chatgpt.com/backend-api/codex/responses`) rejects any request
    /// with `stream=false` ("Stream must be set to true"); the public
    /// API accepts both. Default is non-streaming.
    fn stream_only(&self) -> bool {
        false
    }
    fn supports_hosted_web_search(&self) -> bool {
        false
    }
    fn supports_hosted_image_generation(&self) -> bool {
        false
    }
    /// Called once after a 401/403 before surfacing the auth failure.
    ///
    /// Implementations that can refresh or reload credentials return true
    /// when a retry should be attempted with updated auth headers.
    async fn refresh_after_auth_error(&self) -> Result<bool, ProviderError> {
        Ok(false)
    }
}

/// API-key auth for the public `OpenAI` API.
pub struct ApiKeyAuth {
    api_key: SecretString,
    base_url: String,
    wire: ChatCompletionsWire,
}

impl ApiKeyAuth {
    pub fn new(api_key: SecretString) -> Self {
        Self {
            api_key,
            base_url: OPENAI_BASE_URL.to_owned(),
            wire: ChatCompletionsWire,
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl AuthStrategy for ApiKeyAuth {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(&self) -> Vec<(HeaderName, HeaderValue)> {
        let mut headers = Vec::with_capacity(1);
        push_bearer_header(&mut headers, self.api_key.expose_secret());
        headers
    }

    fn wire(&self) -> &dyn WireFormatDyn {
        &self.wire
    }
}

impl fmt::Debug for ApiKeyAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyAuth")
            .field("api_key", &"****<masked>")
            .field("base_url", &self.base_url)
            .field("wire", &self.wire)
            .finish()
    }
}

/// Codex OAuth bearer auth for the `ChatGPT` Codex backend.
pub struct CodexOAuthAuth {
    access_token: SecretString,
    // Initial: bare String because account ids do not have a domain newtype yet.
    account_id: Option<String>,
    base_url: String,
    wire: ResponsesWire,
}

impl CodexOAuthAuth {
    pub fn new(access_token: SecretString, account_id: Option<String>) -> Self {
        Self {
            access_token,
            account_id,
            base_url: CHATGPT_BASE_URL.to_owned(),
            wire: ResponsesWire,
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

#[async_trait]
impl AuthStrategy for CodexOAuthAuth {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(&self) -> Vec<(HeaderName, HeaderValue)> {
        let mut headers = Vec::with_capacity(5);
        push_bearer_header(&mut headers, self.access_token.expose_secret());
        push_static_header(&mut headers, OPENAI_BETA_HEADER, "responses=experimental");
        push_static_header(&mut headers, ORIGINATOR_HEADER, CODEX_ORIGINATOR);
        push_static_header(&mut headers, USER_AGENT, CODEX_USER_AGENT);
        if let Some(account_id) = &self.account_id {
            push_static_header(&mut headers, CHATGPT_ACCOUNT_ID_HEADER, account_id);
        }
        headers
    }

    fn request_headers(&self, ctx: &RunCtx) -> Vec<(HeaderName, HeaderValue)> {
        let scope = cache_scope_id_from(ctx);
        let mut headers = Vec::with_capacity(6);
        push_static_header(&mut headers, SESSION_ID_HEADER_UNDERSCORE, &scope);
        push_static_header(&mut headers, SESSION_ID_HEADER_DASH, &scope);
        push_static_header(&mut headers, THREAD_ID_HEADER_UNDERSCORE, &scope);
        push_static_header(&mut headers, THREAD_ID_HEADER_DASH, &scope);
        push_static_header(&mut headers, CLIENT_REQUEST_ID_HEADER, &scope);
        push_static_header(&mut headers, CODEX_WINDOW_ID_HEADER, CODEX_INSTALLATION_ID);
        headers
    }

    fn wire(&self) -> &dyn WireFormatDyn {
        &self.wire
    }

    fn supports_model_discovery(&self) -> bool {
        false
    }

    fn stt_endpoint_url(&self) -> String {
        // The Codex Responses backend exposes a ChatGPT-subscription
        // STT path at `chatgpt.com/backend-api/transcribe` that
        // accepts the OAuth bearer. The public
        // `/v1/audio/transcriptions` endpoint refuses Plus/Pro tokens
        // (HTTP 429 "quota exceeded") because subscription accounts
        // have no API quota. Hit the subscription path instead.
        format!(
            "{}/backend-api/transcribe",
            self.base_url.trim_end_matches('/')
        )
    }

    fn stt_accepts_model_field(&self) -> bool {
        false
    }

    fn stream_only(&self) -> bool {
        true
    }

    fn supports_hosted_web_search(&self) -> bool {
        true
    }

    fn supports_hosted_image_generation(&self) -> bool {
        true
    }
}

impl fmt::Debug for CodexOAuthAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CodexOAuthAuth")
            .field("access_token", &"****<masked>")
            .field("account_id", &self.account_id)
            .field("base_url", &self.base_url)
            .field("wire", &self.wire)
            .finish()
    }
}

fn push_bearer_header(headers: &mut Vec<(HeaderName, HeaderValue)>, token: &str) {
    push_static_header(headers, AUTHORIZATION, &format!("Bearer {token}"));
}

fn push_static_header(headers: &mut Vec<(HeaderName, HeaderValue)>, name: HeaderName, value: &str) {
    match HeaderValue::from_str(value) {
        Ok(value) => headers.push((name, value)),
        Err(err) => {
            // Field value contained a control byte or CR/LF. Surface the
            // header name so an unexpected backend 403 on a dropped header
            // is diagnosable; the value itself stays redacted to avoid
            // logging tokens or account ids.
            crabgent_log::warn!(
                header = %name,
                reason = %err,
                "openai header value rejected by HeaderValue::from_str, dropping"
            );
        }
    }
}
