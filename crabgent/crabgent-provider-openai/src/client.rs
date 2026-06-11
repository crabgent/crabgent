//! HTTP lifecycle for OpenAI-compatible endpoint families.

use std::any::Any;
use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use bytes::Bytes;
use crabgent_core::{
    EventStream, LlmRequest, LlmResponse, ProviderError, ProviderEvent, RunCtx, StopReason, Usage,
};
use crabgent_log::warn;
use crabgent_provider_transport::{
    ErrorBodyMode, HttpStatusError, RetryLifecycleConfig, RetryLifecycleOutcome, TransportError,
    is_auth_status, join_url_path, read_body as read_capped_body, send_with_retry_lifecycle,
};
use futures::stream::{self, Stream, StreamExt};
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::auth::AuthStrategy;
use crate::retry::{is_retryable_status, sleep_delay};
use crate::types::{OpenAiConfig, OpenAiError};
use crate::wire::WireFormatDyn;

const MAX_RESPONSE_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

/// Fire a non-streaming request and parse the provider response.
///
/// Auth strategies that report `stream_only()` (e.g. Codex OAuth, where the
/// backend rejects `stream=false`) are routed through the streaming path
/// and accumulated back into an `LlmResponse`.
pub(crate) async fn call_complete(
    http: &reqwest::Client,
    config: &OpenAiConfig,
    auth: &dyn AuthStrategy,
    req: &LlmRequest,
    ctx: &RunCtx,
    cancel: Option<&CancellationToken>,
) -> Result<LlmResponse, ProviderError> {
    if auth.stream_only() {
        return call_complete_via_stream(http, config, auth, req, ctx, cancel).await;
    }
    let wire = auth.wire();
    let body = wire
        .build_body(req, ctx, false)
        .map_err(ProviderError::from)?;
    let response = send_with_retry(http, config, auth, &body, ctx, cancel).await?;
    let bytes = read_body(
        response,
        cancel,
        config.request_timeout,
        MAX_RESPONSE_BODY_BYTES,
    )
    .await?;
    let parsed: Value = serde_json::from_slice(&bytes)
        .map_err(|error| OpenAiError::MalformedResponse(error.to_string()))?;
    wire.parse_response(parsed).map_err(ProviderError::from)
}

async fn call_complete_via_stream(
    http: &reqwest::Client,
    config: &OpenAiConfig,
    auth: &dyn AuthStrategy,
    req: &LlmRequest,
    ctx: &RunCtx,
    cancel: Option<&CancellationToken>,
) -> Result<LlmResponse, ProviderError> {
    let stream_response = start_stream(http, config, auth, req, ctx, cancel).await?;
    let mut events = into_event_stream(
        stream_response.response.bytes_stream(),
        stream_response.decoder,
    );
    accumulate_events(&mut events, req).await
}

async fn accumulate_events(
    events: &mut EventStream,
    req: &LlmRequest,
) -> Result<LlmResponse, ProviderError> {
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut usage = Usage::default();
    let mut stop_reason = StopReason::EndTurn;
    while let Some(event) = events.next().await {
        match event? {
            ProviderEvent::TextDelta(delta) => text.push_str(&delta),
            ProviderEvent::ToolUse(call) => tool_calls.push(call),
            ProviderEvent::Usage(u) => usage = u,
            ProviderEvent::Stop(reason) => stop_reason = reason,
            // `ProviderEvent` is `#[non_exhaustive]`; ignore unknown future
            // variants rather than failing the accumulation.
            _ => {}
        }
    }
    Ok(LlmResponse {
        text,
        tool_calls,
        stop_reason,
        usage,
        model: req.model.clone(),
    })
}

/// Open a streaming request and return the HTTP response with a decoder.
pub(crate) async fn start_stream(
    http: &reqwest::Client,
    config: &OpenAiConfig,
    auth: &dyn AuthStrategy,
    req: &LlmRequest,
    ctx: &RunCtx,
    cancel: Option<&CancellationToken>,
) -> Result<StreamResponse, ProviderError> {
    let wire = auth.wire();
    let decoder = StreamDecoder::from_wire(wire);
    let body = wire
        .build_body(req, ctx, true)
        .map_err(ProviderError::from)?;
    let response = send_with_retry(http, config, auth, &body, ctx, cancel).await?;
    Ok(StreamResponse { response, decoder })
}

pub(crate) struct StreamResponse {
    pub(crate) response: reqwest::Response,
    pub(crate) decoder: StreamDecoder,
}

/// Convert an open response body into provider events.
pub(crate) fn into_event_stream(
    bytes_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    decoder: StreamDecoder,
) -> EventStream {
    let state = StreamPumpState {
        bytes: Box::pin(bytes_stream),
        decoder,
        finished: false,
    };
    Box::pin(stream::unfold(state, advance_stream_state))
}

async fn send_with_retry(
    http: &reqwest::Client,
    config: &OpenAiConfig,
    auth: &dyn AuthStrategy,
    body: &Value,
    ctx: &RunCtx,
    cancel: Option<&CancellationToken>,
) -> Result<reqwest::Response, ProviderError> {
    let noop = CancellationToken::new();
    let token = cancel.unwrap_or(&noop);
    let outcome = send_with_retry_lifecycle(
        || build_http_request(http, auth, body, ctx),
        token,
        RetryLifecycleConfig {
            max_retries: config.max_retries,
            request_timeout: config.request_timeout,
            error_body_max_bytes: MAX_ERROR_BODY_BYTES,
            error_body_mode: ErrorBodyMode::Text,
        },
        is_retryable_status,
        |attempt, retry_after| sleep_delay(attempt, config.retry_base_delay, retry_after),
    )
    .await
    .map_err(map_transport_error)?;
    match outcome {
        RetryLifecycleOutcome::HttpError(error) if is_auth_status(error.status) => {
            retry_after_auth_refresh(http, config, auth, body, ctx, cancel, error.status).await
        }
        other => map_retry_outcome(other),
    }
}

async fn retry_after_auth_refresh(
    http: &reqwest::Client,
    config: &OpenAiConfig,
    auth: &dyn AuthStrategy,
    body: &Value,
    ctx: &RunCtx,
    cancel: Option<&CancellationToken>,
    status: u16,
) -> Result<reqwest::Response, ProviderError> {
    if !auth.refresh_after_auth_error().await? {
        return Err(map_auth_error(status));
    }
    warn!(
        status = status,
        "openai authentication refreshed after auth failure; retrying request"
    );
    let noop = CancellationToken::new();
    let token = cancel.unwrap_or(&noop);
    let outcome = send_with_retry_lifecycle(
        || build_http_request(http, auth, body, ctx),
        token,
        RetryLifecycleConfig {
            max_retries: config.max_retries,
            request_timeout: config.request_timeout,
            error_body_max_bytes: MAX_ERROR_BODY_BYTES,
            error_body_mode: ErrorBodyMode::Text,
        },
        is_retryable_status,
        |attempt, retry_after| sleep_delay(attempt, config.retry_base_delay, retry_after),
    )
    .await
    .map_err(map_transport_error)?;
    map_retry_outcome(outcome)
}

fn map_retry_outcome(outcome: RetryLifecycleOutcome) -> Result<reqwest::Response, ProviderError> {
    match outcome {
        RetryLifecycleOutcome::Ok(response) => Ok(response),
        RetryLifecycleOutcome::HttpError(error) => Err(map_http_error(error)),
        RetryLifecycleOutcome::Network(error) => Err(map_network_error(&error)),
        RetryLifecycleOutcome::Timeout => Err(ProviderError::Timeout),
    }
}

fn map_http_error(error: HttpStatusError) -> ProviderError {
    if is_auth_status(error.status) {
        return map_auth_error(error.status);
    }
    map_api_error(error)
}

fn map_auth_error(status: u16) -> ProviderError {
    warn!(status = status, "openai authentication failed");
    ProviderError::from(OpenAiError::Auth)
}

fn map_api_error(error: HttpStatusError) -> ProviderError {
    let body = error.body.unwrap_or_default();
    warn!(
        status = error.status,
        body_len = body.len(),
        "openai api request failed"
    );
    ProviderError::from(OpenAiError::Api {
        status: error.status,
        retry_after_secs: error.retry_after.map(|delay| delay.as_secs()),
    })
}

fn map_network_error(error: &reqwest::Error) -> ProviderError {
    ProviderError::from(OpenAiError::Network(error.to_string()))
}

fn build_http_request(
    http: &reqwest::Client,
    auth: &dyn AuthStrategy,
    body: &Value,
    ctx: &RunCtx,
) -> reqwest::RequestBuilder {
    let url = endpoint_url(auth.base_url(), auth.wire().endpoint_path());
    let mut builder = http.post(url).header(CONTENT_TYPE, "application/json");
    for (name, value) in auth.auth_headers() {
        builder = builder.header(name, value);
    }
    for (name, value) in auth.request_headers(ctx) {
        builder = builder.header(name, value);
    }
    builder.json(body)
}

fn endpoint_url(base: &str, path: &str) -> String {
    join_url_path(base, path)
}

async fn read_body(
    response: reqwest::Response,
    cancel: Option<&CancellationToken>,
    timeout: Duration,
    max_bytes: usize,
) -> Result<Bytes, ProviderError> {
    read_capped_body(response, cancel, timeout, max_bytes)
        .await
        .map_err(map_transport_error)
}

fn map_transport_error(error: TransportError) -> ProviderError {
    match error {
        TransportError::Cancelled => ProviderError::Cancelled,
        TransportError::Timeout => ProviderError::Timeout,
        TransportError::Request(error) => {
            ProviderError::from(OpenAiError::Network(error.to_string()))
        }
        TransportError::BodyTooLarge { max_bytes } => ProviderError::MalformedResponse(format!(
            "openai response exceeds max body size: {max_bytes} bytes"
        )),
        _ => ProviderError::from(OpenAiError::Network("openai transport failed".to_owned())),
    }
}

pub(crate) struct StreamDecoder {
    wire: Box<dyn WireFormatDyn>,
    state: Box<dyn Any + Send + Sync>,
    pending_utf8: Vec<u8>,
    line_buffer: String,
    queued: VecDeque<Result<ProviderEvent, ProviderError>>,
}

impl StreamDecoder {
    fn from_wire(wire: &dyn WireFormatDyn) -> Self {
        Self {
            wire: wire.clone_box(),
            state: wire.new_stream_state(),
            pending_utf8: Vec::new(),
            line_buffer: String::new(),
            queued: VecDeque::new(),
        }
    }

    fn pop_event(&mut self) -> Option<Result<ProviderEvent, ProviderError>> {
        self.queued.pop_front()
    }

    fn feed_chunk(&mut self, chunk: &Bytes) {
        self.pending_utf8.extend_from_slice(chunk);
        match std::str::from_utf8(&self.pending_utf8) {
            Ok(text) => {
                let text = text.to_owned();
                self.pending_utf8.clear();
                self.feed_text(&text);
            }
            Err(error) if error.error_len().is_none() => {}
            Err(_) => {
                self.pending_utf8.clear();
                self.queued.push_back(Err(ProviderError::MalformedResponse(
                    "openai stream emitted invalid utf-8".to_owned(),
                )));
            }
        }
    }

    fn finish(&mut self) {
        if !self.pending_utf8.is_empty() {
            match std::str::from_utf8(&self.pending_utf8) {
                Ok(text) => {
                    let text = text.to_owned();
                    self.feed_text(&text);
                }
                Err(_) => self.queued.push_back(Err(ProviderError::MalformedResponse(
                    "openai stream ended with invalid utf-8".to_owned(),
                ))),
            }
            self.pending_utf8.clear();
        }
        if !self.line_buffer.is_empty() {
            let line = std::mem::take(&mut self.line_buffer);
            self.feed_line(&line);
        }
        self.drain_wire_queue();
    }

    fn feed_text(&mut self, text: &str) {
        self.line_buffer.push_str(text);
        while let Some(index) = self.line_buffer.find('\n') {
            let mut line: String = self.line_buffer.drain(..=index).collect();
            trim_line_end(&mut line);
            self.feed_line(&line);
        }
    }

    fn feed_line(&mut self, line: &str) {
        if let Some(event) = self.wire.parse_sse_event_dyn(line, self.state.as_mut()) {
            self.queued.push_back(Ok(event));
        }
        self.drain_wire_queue();
    }

    fn drain_wire_queue(&mut self) {
        while let Some(event) = self.wire.parse_sse_event_dyn("", self.state.as_mut()) {
            self.queued.push_back(Ok(event));
        }
    }
}

fn trim_line_end(line: &mut String) {
    if line.ends_with('\n') {
        line.pop();
    }
    if line.ends_with('\r') {
        line.pop();
    }
}

struct StreamPumpState {
    bytes: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    decoder: StreamDecoder,
    finished: bool,
}

async fn advance_stream_state(
    mut state: StreamPumpState,
) -> Option<(Result<ProviderEvent, ProviderError>, StreamPumpState)> {
    loop {
        if let Some(event) = state.decoder.pop_event() {
            return Some((event, state));
        }
        if state.finished {
            return None;
        }
        match state.bytes.next().await {
            Some(Ok(chunk)) => state.decoder.feed_chunk(&chunk),
            Some(Err(error)) => {
                state.finished = true;
                state
                    .decoder
                    .queued
                    .push_back(Err(ProviderError::from(OpenAiError::Network(
                        error.to_string(),
                    ))));
            }
            None => {
                state.finished = true;
                state.decoder.finish();
            }
        }
    }
}
