use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::{LlmRequest, Provider, ProviderError, WebSearchConfig};
use crabgent_provider_openai::wire::WireFormatDyn;
use crabgent_provider_openai::wire::responses::ResponsesWire;
use crabgent_provider_openai::{
    ApiKeyAuth, AuthStrategy, CodexOAuthAuth, OpenAiConfig, OpenAiProvider,
};
use reqwest::header::{AUTHORIZATION, HeaderName, HeaderValue};
use secrecy::SecretString;
use serde_json::json;

const API_KEY_SECRET: &str = "secret-test-key-99999";
const CODEX_TOKEN_SECRET: &str = "secret-test-token-99999";

fn api_key_auth(base_url: &str) -> ApiKeyAuth {
    ApiKeyAuth::new(SecretString::from(API_KEY_SECRET.to_owned()))
        .with_base_url(base_url.to_owned())
}

fn codex_auth(base_url: &str) -> CodexOAuthAuth {
    CodexOAuthAuth::new(
        SecretString::from(CODEX_TOKEN_SECRET.to_owned()),
        Some("account-test-id".to_owned()),
    )
    .with_base_url(base_url.to_owned())
}

fn config(max_retries: u32) -> OpenAiConfig {
    OpenAiConfig::new("secret-config-key")
        .with_max_retries(max_retries)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

fn req(model: &str) -> LlmRequest {
    LlmRequest {
        model: model.into(),
        system_prompt: None,
        messages: vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": "hi"}],
        })],
        tools: Vec::new(),
        max_tokens: Some(64),
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}

#[tokio::test]
async fn auth_failure_no_token_leak() {
    let mut server = mockito::Server::new_async().await;
    let api_auth = api_key_auth(&server.url());
    let codex_auth = codex_auth(&server.url());
    let api_mock = server
        .mock("POST", "/v1/chat/completions")
        .with_status(401)
        .with_body(format!("bad key {API_KEY_SECRET}"))
        .expect(1)
        .create_async()
        .await;
    let codex_mock = server
        .mock("POST", "/backend-api/codex/responses")
        .with_status(401)
        .with_body(format!("bad token {CODEX_TOKEN_SECRET}"))
        .expect(1)
        .create_async()
        .await;

    let api_provider =
        OpenAiProvider::try_new(reqwest::Client::new(), config(0), Box::new(api_auth))
            .expect("valid api provider");
    let api_error = api_provider
        .complete(
            &req("gpt-5.5"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect_err("auth error");

    let codex_provider =
        OpenAiProvider::try_new(reqwest::Client::new(), config(0), Box::new(codex_auth))
            .expect("valid codex provider");
    let codex_error = codex_provider
        .complete(
            &req("gpt-5.3-codex"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect_err("auth error");

    assert_auth_error_redacted(&api_error, API_KEY_SECRET);
    assert_auth_error_redacted(&codex_error, CODEX_TOKEN_SECRET);
    api_mock.assert_async().await;
    codex_mock.assert_async().await;
}

#[tokio::test]
async fn codex_oauth_no_token_refresh() {
    let mut server = mockito::Server::new_async().await;
    let auth = codex_auth(&server.url());
    let request_count = Arc::new(AtomicUsize::new(0));
    let recorded_requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let counter = Arc::clone(&request_count);
    let recorder = Arc::clone(&recorded_requests);
    let mock = server
        .mock("POST", "/backend-api/codex/responses")
        .match_request(move |request| {
            counter.fetch_add(1, Ordering::SeqCst);
            request.utf8_lossy_body().is_ok_and(|body| {
                recorder.lock().is_ok_and(|mut requests| {
                    requests.push(body.into_owned());
                    true
                })
            })
        })
        .with_status(401)
        .with_body("token expired")
        .expect(1)
        .create_async()
        .await;

    let provider = OpenAiProvider::try_new(reqwest::Client::new(), config(3), Box::new(auth))
        .expect("valid provider");
    let error = provider
        .complete(
            &req("gpt-5.3-codex"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect_err("auth error");

    assert!(matches!(error, ProviderError::Auth(_)));
    mock.assert_async().await;
    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    let recorded_len = recorded_requests
        .lock()
        .expect("recorded requests lock")
        .len();
    assert_eq!(recorded_len, 1);
}

#[tokio::test]
async fn auth_failure_refreshes_once_and_retries() {
    let mut server = mockito::Server::new_async().await;
    let auth = RefreshOnceAuth::new(server.url());
    let first = server
        .mock("POST", "/backend-api/codex/responses")
        .match_header("authorization", "Bearer old-token")
        .with_status(401)
        .with_body("expired")
        .expect(1)
        .create_async()
        .await;
    let second = server
        .mock("POST", "/backend-api/codex/responses")
        .match_header("authorization", "Bearer new-token")
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        )
        .expect(1)
        .create_async()
        .await;

    let provider = OpenAiProvider::try_new(reqwest::Client::new(), config(0), Box::new(auth))
        .expect("valid provider");
    let response = provider
        .complete(
            &req("gpt-5.3-codex"),
            &crabgent_core::RunCtx::new(
                crabgent_core::RunId::new(),
                crabgent_core::Subject::new("test"),
            ),
            None,
        )
        .await
        .expect("retry succeeds");

    assert_eq!(response.text, "ok");
    first.assert_async().await;
    second.assert_async().await;
}

fn assert_auth_error_redacted(error: &ProviderError, secret: &str) {
    assert!(matches!(error, ProviderError::Auth(_)));
    let rendered = format!("{error:?}\n{error}");
    assert!(!rendered.contains(secret), "secret leaked: {rendered}");
    assert!(!rendered.contains("401"), "status leaked: {rendered}");
}

struct RefreshOnceAuth {
    base_url: String,
    token: Mutex<String>,
    refresh_count: AtomicUsize,
    wire: ResponsesWire,
}

impl RefreshOnceAuth {
    fn new(base_url: String) -> Self {
        Self {
            base_url,
            token: Mutex::new("old-token".to_owned()),
            refresh_count: AtomicUsize::new(0),
            wire: ResponsesWire,
        }
    }
}

#[async_trait]
impl AuthStrategy for RefreshOnceAuth {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn auth_headers(&self) -> Vec<(HeaderName, HeaderValue)> {
        let token = self.token.lock().expect("token lock").clone();
        vec![(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("valid header"),
        )]
    }

    fn wire(&self) -> &dyn WireFormatDyn {
        &self.wire
    }

    fn supports_model_discovery(&self) -> bool {
        false
    }

    fn stream_only(&self) -> bool {
        true
    }

    async fn refresh_after_auth_error(&self) -> Result<bool, ProviderError> {
        self.refresh_count.fetch_add(1, Ordering::SeqCst);
        "new-token".clone_into(&mut self.token.lock().expect("token lock"));
        Ok(true)
    }
}
