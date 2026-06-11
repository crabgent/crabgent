use std::sync::Arc;
use std::time::Duration;

use reqwest::{StatusCode, header::HeaderMap};
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::config::McpServerConfig;
use crate::{JsonRpcError, McpCallResult, McpError, McpToolDef, McpToolList};

mod builder;
mod parse;

pub use builder::McpClientBuilder;
use parse::parse_rpc_response;

const MCP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const MCP_CLIENT_NAME: &str = "crabgent-mcp-client";
const ERR_SESSION_NOT_FOUND: i64 = -32_001;

/// Connect-phase deadline. A configured MCP endpoint that does not complete the
/// TCP/TLS handshake within this window is treated as unreachable.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct McpClient {
    config: McpServerConfig,
    http: reqwest::Client,
    session: Arc<Mutex<Option<McpSession>>>,
}

struct McpResponseBody {
    status: StatusCode,
    is_sse: bool,
    body: String,
    session_id: Option<String>,
}

#[derive(Clone)]
enum McpSession {
    Stateful(String),
    Stateless,
}

impl McpSession {
    fn request_header_value(&self) -> Option<String> {
        match self {
            Self::Stateful(session_id) => Some(session_id.clone()),
            Self::Stateless => None,
        }
    }
}

impl McpClient {
    pub(crate) fn new(config: McpServerConfig) -> Result<Self, McpError> {
        // Keep the HTTP client owned per configured server so future transport
        // settings cannot bleed across MCP server boundaries.
        let http = reqwest::Client::builder()
            // Never auto-follow 3xx: a malicious or compromised MCP server could
            // answer with a redirect to an internal address (cloud metadata,
            // link-local) and have the client exfiltrate that response back into
            // the kernel/LLM. The MCP Streamable-HTTP transport does not require
            // redirect following, so reject every 3xx outright (SSRF fail-closed).
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            // Per-read idle deadline (resets after every successful read). A
            // healthy SSE stream with regular events survives indefinitely; a
            // slow-loris trickle below the response cap trips this instead of
            // pinning the worker. A whole-request `.timeout()` is intentionally
            // avoided because it would also kill legitimate long-lived streams.
            .read_timeout(config.read_idle_timeout)
            .build()
            .map_err(|err| {
                McpError::InvalidConfig(format!("failed to build MCP HTTP client: {err}"))
            })?;

        Ok(Self {
            config,
            http,
            session: Arc::new(Mutex::new(None)),
        })
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub(crate) const fn max_output_bytes(&self) -> usize {
        self.config.max_output_bytes
    }

    #[crabgent_log::instrument(skip(self), err, fields(server = %self.name()))]
    pub async fn discover(&self) -> Result<McpToolList, McpError> {
        let result = self
            .rpc_call(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }))
            .await?;

        let tools = result
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::Discovery("tools/list response missing tools".to_string()))?;

        let mut defs = Vec::with_capacity(tools.len());
        for tool in tools {
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                crabgent_log::warn!(server = %self.config.name, "MCP tool without name skipped");
                continue;
            };

            if !valid_tool_name(name) {
                crabgent_log::warn!(
                    server = %self.config.name,
                    tool = %name,
                    "MCP tool with invalid name skipped"
                );
                continue;
            }

            defs.push(McpToolDef {
                name: name.to_string(),
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                input_schema: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object"})),
            });
        }

        Ok(McpToolList { tools: defs })
    }

    #[crabgent_log::instrument(skip(self, args, cancel), err, fields(server = %self.name(), tool = %tool_name))]
    pub async fn call_tool(
        &self,
        tool_name: &str,
        args: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<McpCallResult, McpError> {
        let result = self
            .rpc_call_with_cancel(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": tool_name,
                        "arguments": args
                    }
                }),
                cancel,
            )
            .await?;

        let call_result = McpCallResult {
            content: Value::String(extract_text_content(&result)),
            is_error: result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        };

        if call_result.is_error {
            let message = match &call_result.content {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            return Err(McpError::ToolCall(message));
        }

        Ok(call_result)
    }

    async fn rpc_call(&self, body: Value) -> Result<Value, McpError> {
        self.rpc_call_with_cancel(body, None).await
    }

    async fn rpc_call_with_cancel(
        &self,
        body: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, McpError> {
        match self.rpc_call_once(&body, cancel).await {
            Err(McpError::SessionNotFound) => {
                self.clear_session().await;
                self.rpc_call_once(&body, cancel).await
            }
            result => result,
        }
    }

    async fn rpc_call_once(
        &self,
        body: &Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, McpError> {
        let session_id = self.ensure_session(cancel).await?;
        let request = self.request(body, session_id.as_deref());
        let response = send_request(request, cancel).await?;

        self.handle_response(response).await
    }

    async fn ensure_session(
        &self,
        cancel: Option<&CancellationToken>,
    ) -> Result<Option<String>, McpError> {
        // Copy the active session out under the lock and release the guard
        // before any network I/O. Holding the Mutex across the initialize
        // round-trip would serialize every concurrent call_tool/discover on
        // the same client behind a full network round-trip.
        if let Some(active_session) = self.session.lock().await.as_ref() {
            return Ok(active_session.request_header_value());
        }

        let next_session = self.initialize_session(cancel).await?;
        let request_header_value = next_session.request_header_value();

        // Re-acquire briefly to store the result. Two callers can race the
        // initialize, but that only means one redundant initialize: the last
        // writer wins and both observe a usable session. "initialize once" is
        // best-effort, not a deadlock-prone lock-across-I/O guarantee.
        let mut session = self.session.lock().await;
        if let Some(active_session) = session.as_ref() {
            return Ok(active_session.request_header_value());
        }
        *session = Some(next_session);
        Ok(request_header_value)
    }

    async fn initialize_session(
        &self,
        cancel: Option<&CancellationToken>,
    ) -> Result<McpSession, McpError> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": MCP_CLIENT_NAME,
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        });
        let response = send_request(self.request(&body, None), cancel).await?;
        let response = self.read_response_body(response).await?;
        self.check_http_status(&response)?;
        let result = self.handle_rpc_body(&response)?;
        let session_id = response.session_id.or_else(|| {
            result
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });

        // Mcp-Session-Id is optional for Streamable HTTP. Stateless servers
        // omit it and expect subsequent requests without a session header.
        // Some servers return the session id in initialize.result.sessionId
        // instead of the response header, so accept both forms.
        Ok(session_id.map_or(McpSession::Stateless, McpSession::Stateful))
    }

    async fn clear_session(&self) {
        *self.session.lock().await = None;
    }

    fn request(&self, body: &Value, session_id: Option<&str>) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .post(&self.config.base_url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&body);

        if let Some(token) = &self.config.token {
            request = request.bearer_auth(token.expose_secret());
        }
        if let Some(session_id) = session_id {
            request = request.header(MCP_SESSION_ID_HEADER, session_id);
        }

        request
    }

    async fn handle_response(&self, response: reqwest::Response) -> Result<Value, McpError> {
        let response = self.read_response_body(response).await?;
        self.check_http_status(&response)?;
        self.handle_rpc_body(&response)
    }

    async fn read_response_body(
        &self,
        response: reqwest::Response,
    ) -> Result<McpResponseBody, McpError> {
        let status = response.status();
        let is_sse = content_type_is_sse(response.headers());
        let session_id = response
            .headers()
            .get(MCP_SESSION_ID_HEADER)
            .map(|value| {
                value.to_str().map(str::to_owned).map_err(|err| {
                    McpError::Decode(format!("invalid Mcp-Session-Id header: {err}"))
                })
            })
            .transpose()?;
        let body = self
            .read_body_capped(response, self.config.max_response_bytes)
            .await?;
        Ok(McpResponseBody {
            status,
            is_sse,
            body,
            session_id,
        })
    }

    fn check_http_status(&self, response: &McpResponseBody) -> Result<(), McpError> {
        if is_http_auth_status(response.status) {
            return self.auth_http_error(response);
        }

        if response_is_session_not_found_http(response) {
            return Err(McpError::SessionNotFound);
        }

        if response.status.is_success() {
            return Ok(());
        }

        self.http_error(response)
    }

    fn auth_http_error(&self, response: &McpResponseBody) -> Result<(), McpError> {
        crabgent_log::warn!(
            server = %self.config.name,
            status = response.status.as_u16(),
            body = %crabgent_log::redact_text(&response.body),
            "MCP server authentication failed"
        );
        Err(McpError::AuthFailed)
    }

    fn http_error(&self, response: &McpResponseBody) -> Result<(), McpError> {
        crabgent_log::warn!(
            server = %self.config.name,
            status = response.status.as_u16(),
            body = %crabgent_log::redact_text(&response.body),
            "MCP server returned HTTP error"
        );
        Err(McpError::ToolCall(format!(
            "MCP server returned {}",
            response.status.as_u16()
        )))
    }

    fn handle_rpc_body(&self, response: &McpResponseBody) -> Result<Value, McpError> {
        let rpc = parse_rpc_response(&response.body, response.is_sse)?;
        if let Some(error) = rpc.error {
            return self.handle_rpc_error(error);
        }

        rpc.result
            .ok_or_else(|| McpError::Decode("JSON-RPC response missing result".to_string()))
    }

    fn handle_rpc_error(&self, error: JsonRpcError) -> Result<Value, McpError> {
        if error.code == ERR_SESSION_NOT_FOUND {
            return Err(McpError::SessionNotFound);
        }
        if is_auth_error(&error.message) {
            self.warn_rpc_auth_error(&error);
            return Err(McpError::AuthFailed);
        }
        Err(McpError::JsonRpc {
            code: error.code,
            message: error.message,
        })
    }

    fn warn_rpc_auth_error(&self, error: &JsonRpcError) {
        crabgent_log::warn!(
            server = %self.config.name,
            code = error.code,
            message = %crabgent_log::redact_text(&error.message),
            "MCP server returned JSON-RPC auth error"
        );
    }

    async fn read_body_capped(
        &self,
        mut response: reqwest::Response,
        max_bytes: usize,
    ) -> Result<String, McpError> {
        let mut body = Vec::new();

        while let Some(chunk) = response.chunk().await? {
            body.extend_from_slice(&chunk);
            if body.len() > max_bytes {
                return Err(McpError::OutputCapExceeded);
            }
        }

        String::from_utf8(body).map_err(|err| McpError::Decode(err.to_string()))
    }
}

fn content_type_is_sse(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"))
}

fn is_http_auth_status(status: StatusCode) -> bool {
    status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN
}

fn response_is_session_not_found_http(response: &McpResponseBody) -> bool {
    response.status == StatusCode::NOT_FOUND
        && response_is_session_not_found(&response.body, response.is_sse)
}

fn response_is_session_not_found(body: &str, is_sse: bool) -> bool {
    parse_rpc_response(body, is_sse)
        .ok()
        .and_then(|rpc| rpc.error)
        .is_some_and(|error| error.code == ERR_SESSION_NOT_FOUND)
}

async fn send_request(
    request: reqwest::RequestBuilder,
    cancel: Option<&CancellationToken>,
) -> Result<reqwest::Response, McpError> {
    let response = if let Some(cancel) = cancel {
        tokio::select! {
            () = cancel.cancelled() => return Err(McpError::Cancelled),
            response = request.send() => response?,
        }
    } else {
        request.send().await?
    };

    Ok(response)
}

fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

// Heuristic match against common 401-flavoured JSON-RPC error wording from
// arbitrary MCP servers. These are generic auth phrasings, not a contract with
// any one server, so do not couple them to another crate's error constants.
fn is_auth_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("invalid_auth")
        || lower.contains("token_revoked")
        || lower.contains("authentication required")
}

fn extract_text_content(result: &Value) -> String {
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return result.to_string();
    };

    let text = content
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();

    if text.is_empty() {
        result.to_string()
    } else {
        text.join("\n")
    }
}
