use std::collections::HashMap;
use std::sync::Arc;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use crabgent_core::Subject;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::auth::{HeaderMap, verified_bearer};
use crate::session::McpSessionId;
use crate::tools;
use crate::wire::{
    ERR_METHOD_NOT_FOUND, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    error_code, error_response, parse_message, redacted_message, success_response,
    validate_protocol_version,
};
use crate::{McpServer, McpServerError};

pub const MCP_SESSION_ID_HEADER: &str = "Mcp-Session-Id";
pub const MCP_VERSION_HEADER: &str = "Mcp-Protocol-Version";
const CONTENT_TYPE_HEADER: &str = "Content-Type";
const APPLICATION_JSON: &str = "application/json";

pub struct McpHandler {
    server: Arc<McpServer>,
}

#[derive(Debug)]
pub struct McpResponse {
    pub status_code: u16,
    pub headers: HashMap<String, String>,
    pub body: Bytes,
}

impl McpHandler {
    #[must_use]
    pub const fn new(server: Arc<McpServer>) -> Self {
        Self { server }
    }

    pub async fn dispatch(&self, headers: &HeaderMap, body: &[u8]) -> McpResponse {
        let Ok(bearer) = verified_bearer(headers, &self.server.config().bearer_token) else {
            return empty_response(401);
        };

        // Reject oversized bodies before parsing so a large payload with a valid
        // bearer cannot force a full in-memory JSON parse (fail-closed DoS guard).
        if body.len() > self.server.config().max_request_bytes {
            return json_rpc_error_response(Value::Null, &request_too_large_error());
        }

        let request = match parse_message(body) {
            Ok(JsonRpcMessage::Request(request)) => request,
            Ok(JsonRpcMessage::Notification(notification)) => {
                return handle_notification(&notification);
            }
            Err(err) => return json_rpc_error_response(Value::Null, &err),
        };

        let response = match request.method.as_str() {
            "initialize" => self.handle_initialize(headers, bearer, &request).await,
            _ => self.handle_session_method(headers, bearer, &request).await,
        };

        response.unwrap_or_else(|err| json_rpc_error_response(request.id.clone(), &err))
    }

    async fn handle_initialize(
        &self,
        headers: &HeaderMap,
        bearer: &str,
        request: &JsonRpcRequest,
    ) -> Result<McpResponse, McpServerError> {
        validate_protocol_version(header_value(headers, MCP_VERSION_HEADER))?;
        if header_value(headers, MCP_SESSION_ID_HEADER).is_some() {
            return Err(McpServerError::InvalidRequest(
                "initialize must not include Mcp-Session-Id".into(),
            ));
        }

        let subject = self.request_subject(bearer);
        let session_id = self.server.session_registry.create(subject).await?;
        let result = json!({
            "sessionId": session_id.to_string(),
            "protocolVersion": self.server.config().protocol_version,
            "serverInfo": {
                "name": "crabgent-mcp-server",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": {},
            },
        });
        let mut response = json_response(200, &success_response(request.id.clone(), result));
        response
            .headers
            .insert(MCP_SESSION_ID_HEADER.to_owned(), session_id.to_string());
        Ok(response)
    }

    async fn handle_session_method(
        &self,
        headers: &HeaderMap,
        bearer: &str,
        request: &JsonRpcRequest,
    ) -> Result<McpResponse, McpServerError> {
        let session_id = require_session_id(headers)?;
        let session = self
            .server
            .session_registry
            .get(&session_id)
            .await
            .ok_or(McpServerError::SessionNotFound)?;
        // Bind the session to its creator: a follow-up request must derive the
        // same subject as the bearer that created the session. A mismatch is
        // reported as SessionNotFound so we do not leak that the id exists under
        // a different subject (fail-closed against session impersonation).
        if self.request_subject(bearer).id() != session.subject.id() {
            return Err(McpServerError::SessionNotFound);
        }
        let rpc_response = match request.method.as_str() {
            "tools/list" | "tools/call" => {
                tools::handle_tools_dispatch(&self.server, &session, request).await
            }
            method => error_response(
                request.id.clone(),
                ERR_METHOD_NOT_FOUND,
                method_not_found_message(method),
                None,
            ),
        };
        Ok(json_response(200, &rpc_response))
    }

    fn request_subject(&self, bearer: &str) -> Subject {
        self.server
            .config()
            .subject_override
            .clone()
            .unwrap_or_else(|| derive_subject(bearer))
    }
}

// Mirror the existing parse-error path (ERR_PARSE / HTTP 400) for an oversized
// body. The message carries no body content or token material.
fn request_too_large_error() -> McpServerError {
    McpServerError::Parse("request body exceeds maximum size".into())
}

fn require_session_id(headers: &HeaderMap) -> Result<McpSessionId, McpServerError> {
    let value = header_value(headers, MCP_SESSION_ID_HEADER).ok_or_else(|| {
        McpServerError::InvalidRequest("Mcp-Session-Id header is required".into())
    })?;
    McpSessionId::parse(value)
}

fn derive_subject(bearer: &str) -> Subject {
    let mut hasher = Sha256::new();
    hasher.update(b"mcp-subject-v1\0");
    hasher.update(bearer.as_bytes());
    let digest = hasher.finalize();
    let digest_bytes: &[u8] = digest.as_ref();
    let subject_bytes = digest_bytes.get(..8).unwrap_or(digest_bytes);
    Subject::new(format!("mcp-{}", URL_SAFE_NO_PAD.encode(subject_bytes)))
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _value)| key.eq_ignore_ascii_case(name))
        .map(|(_key, value)| value.as_str())
}

fn json_response(status_code: u16, response: &JsonRpcResponse) -> McpResponse {
    let mut headers = HashMap::new();
    headers.insert(CONTENT_TYPE_HEADER.to_owned(), APPLICATION_JSON.to_owned());
    McpResponse {
        status_code,
        headers,
        body: response_body(response),
    }
}

fn json_rpc_error_response(id: Value, err: &McpServerError) -> McpResponse {
    let response = error_response(id, error_code(err), redacted_message(err), None);
    json_response(error_status(err), &response)
}

fn response_body(response: &JsonRpcResponse) -> Bytes {
    match serde_json::to_vec(response) {
        Ok(body) => Bytes::from(body),
        Err(_) => Bytes::from_static(
            br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"internal error"}}"#,
        ),
    }
}

fn empty_response(status_code: u16) -> McpResponse {
    McpResponse {
        status_code,
        headers: HashMap::new(),
        body: Bytes::new(),
    }
}

const fn error_status(err: &McpServerError) -> u16 {
    match err {
        McpServerError::Parse(_)
        | McpServerError::InvalidRequest(_)
        | McpServerError::InvalidParams(_) => 400,
        McpServerError::SessionNotFound => 404,
        McpServerError::Internal(_)
        | McpServerError::KernelRun(_)
        | McpServerError::ToolExecution(_) => 500,
        McpServerError::MethodNotFound(_)
        | McpServerError::AuthRequired
        | McpServerError::ToolNotFound(_) => 200,
    }
}

// The JSON-RPC method name is client-supplied routing data, not a secret, so
// echo it back to make -32601 responses debuggable.
fn method_not_found_message(method: &str) -> String {
    format!("method not found: {method}")
}

fn handle_notification(_notification: &JsonRpcNotification) -> McpResponse {
    // Per the MCP spec a JSON-RPC notification (no `id`) receives no response
    // body; `notifications/initialized` and any other notification are accepted
    // and acknowledged with an empty 202.
    empty_response(202)
}

#[cfg(test)]
mod tests {
    use crabgent_core::{AllowAllPolicy, Kernel, ModelInfo, ModelTarget, Subject};
    use crabgent_test_support::StubProvider;
    use secrecy::SecretString;

    use super::*;
    use crate::auth::AUTHORIZATION_HEADER;
    use crate::{McpServerBuilder, McpServerConfig};

    const TEST_TOKEN: &str = "secret-test-token-12345";
    const TEST_MODEL: &str = "test-model";

    fn test_provider() -> StubProvider {
        StubProvider::with_text("mock-reply")
            .with_name("test-provider")
            .with_models(vec![ModelInfo::minimal(TEST_MODEL, "test-provider")])
    }

    fn test_kernel() -> Arc<Kernel> {
        Arc::new(
            Kernel::builder()
                .provider(test_provider())
                .policy(AllowAllPolicy)
                .try_build()
                .expect("test provider advertises one valid model"),
        )
    }

    fn test_config() -> McpServerConfig {
        McpServerConfig::new(SecretString::from(TEST_TOKEN), ModelTarget::id(TEST_MODEL))
    }

    fn auth_headers_for(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION_HEADER.to_owned(), format!("Bearer {token}"));
        headers
    }

    fn auth_headers() -> HeaderMap {
        auth_headers_for(TEST_TOKEN)
    }

    #[test]
    fn same_bearer_yields_same_subject() {
        let first = derive_subject(TEST_TOKEN);
        let second = derive_subject(TEST_TOKEN);

        assert_eq!(first.id(), second.id());
        assert!(first.id().starts_with("mcp-"));
    }

    #[test]
    fn different_bearer_yields_different_subject() {
        let first = derive_subject(TEST_TOKEN);
        let second = derive_subject("secret-test-token-other");

        assert_ne!(first.id(), second.id());
    }

    #[tokio::test]
    async fn subject_override_wins() {
        let override_subject = Subject::new("configured-subject");
        let server = Arc::new(
            McpServerBuilder::new()
                .with_kernel(test_kernel())
                .with_config(test_config().with_subject_override(override_subject.clone()))
                .build()
                .expect("test server has kernel and config"),
        );
        let handler = McpHandler::new(Arc::clone(&server));
        let response = handler
            .dispatch(
                &auth_headers(),
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
            )
            .await;
        let session_id = response
            .headers
            .get(MCP_SESSION_ID_HEADER)
            .expect("initialize returns session header")
            .parse::<McpSessionId>()
            .expect("initialize returns valid session id");
        let entry = server
            .session_registry
            .get(&session_id)
            .await
            .expect("initialized session is stored");

        assert_eq!(entry.subject.id(), override_subject.id());
    }

    fn decode_error_code(response: &McpResponse) -> i64 {
        let decoded: JsonRpcResponse =
            serde_json::from_slice(&response.body).expect("response body is valid JSON-RPC");
        decoded
            .error
            .expect("response carries a JSON-RPC error")
            .code
    }

    #[tokio::test]
    async fn oversized_body_is_rejected_before_parse() {
        let server = Arc::new(
            McpServerBuilder::new()
                .with_kernel(test_kernel())
                .with_config(test_config().with_max_request_bytes(64))
                .build()
                .expect("test server has kernel and config"),
        );
        let handler = McpHandler::new(Arc::clone(&server));
        // Valid `initialize` JSON, padded past the 64-byte cap. A successful
        // parse would create a session; the guard must fire first.
        let padding = "x".repeat(256);
        let body =
            format!(r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","_pad":"{padding}"}}"#);

        let response = handler.dispatch(&auth_headers(), body.as_bytes()).await;

        assert_eq!(response.status_code, 400);
        assert_eq!(decode_error_code(&response), crate::wire::ERR_PARSE);
        assert!(!response.headers.contains_key(MCP_SESSION_ID_HEADER));
        // Not parsed: no session was created for the oversized request.
        assert!(server.session_registry.is_empty().await);
    }

    #[tokio::test]
    async fn session_bound_to_creator_subject() {
        let server = Arc::new(
            McpServerBuilder::new()
                .with_kernel(test_kernel())
                .with_config(test_config())
                .build()
                .expect("test server has kernel and config"),
        );
        let handler = McpHandler::new(Arc::clone(&server));
        // Session created under a different subject than the request's bearer
        // derives (the per-tenant-token impersonation case).
        let session_id = server
            .session_registry
            .create(Subject::new("other-tenant"))
            .await
            .expect("session is created");
        let mut headers = auth_headers();
        headers.insert(MCP_SESSION_ID_HEADER.to_owned(), session_id.to_string());

        let response = handler
            .dispatch(
                &headers,
                br#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"chat","arguments":{"message":"hello"}}}"#,
            )
            .await;

        assert_eq!(response.status_code, 404);
        assert_eq!(
            decode_error_code(&response),
            crate::wire::ERR_SESSION_NOT_FOUND
        );
    }

    #[tokio::test]
    async fn same_subject_reuses_session() {
        let server = Arc::new(
            McpServerBuilder::new()
                .with_kernel(test_kernel())
                .with_config(test_config())
                .build()
                .expect("test server has kernel and config"),
        );
        let handler = McpHandler::new(Arc::clone(&server));
        let headers = auth_headers();
        let init = handler
            .dispatch(
                &headers,
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
            )
            .await;
        let session_id = init
            .headers
            .get(MCP_SESSION_ID_HEADER)
            .expect("initialize returns session header")
            .to_owned();
        let mut next_headers = headers;
        next_headers.insert(MCP_SESSION_ID_HEADER.to_owned(), session_id);

        let response = handler
            .dispatch(
                &next_headers,
                br#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"chat","arguments":{"message":"hello"}}}"#,
            )
            .await;

        assert_eq!(response.status_code, 200);
    }

    #[tokio::test]
    async fn chat_dispatch_single_lookup() {
        let server = Arc::new(
            McpServerBuilder::new()
                .with_kernel(test_kernel())
                .with_config(test_config())
                .build()
                .expect("test server has kernel and config"),
        );
        let handler = McpHandler::new(Arc::clone(&server));
        let headers = auth_headers();
        let response = handler
            .dispatch(
                &headers,
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
            )
            .await;
        let session_id = response
            .headers
            .get(MCP_SESSION_ID_HEADER)
            .expect("initialize returns session header")
            .to_owned();
        let mut next_headers = headers;
        next_headers.insert(MCP_SESSION_ID_HEADER.to_owned(), session_id);

        let response = handler
            .dispatch(
                &next_headers,
                br#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"chat","arguments":{"message":"hello"}}}"#,
            )
            .await;

        assert_eq!(response.status_code, 200);
        assert_eq!(server.session_registry.get_call_count(), 1);
    }
}
