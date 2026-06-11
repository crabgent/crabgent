use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::McpServerError;

pub type HeaderValue = str;

pub const PROTOCOL_VERSION: &str = crate::config::MCP_PROTOCOL_VERSION;
pub const ERR_PARSE: i64 = -32_700;
pub const ERR_INVALID_REQUEST: i64 = -32_600;
pub const ERR_METHOD_NOT_FOUND: i64 = -32_601;
pub const ERR_INVALID_PARAMS: i64 = -32_602;
pub const ERR_INTERNAL: i64 = -32_603;
pub const ERR_SESSION_NOT_FOUND: i64 = -32_001;
pub const SSE_DATA_PREFIX: &str = "data: ";
const SSE_FRAME_SUFFIX: &str = "\n\n";
const INTERNAL_ERROR_FRAME: &[u8] =
    br#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"internal error"}}"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    pub params: Value,
}

/// JSON-RPC notification: a message with a `method` but no `id`. Per the MCP
/// spec the server processes it without producing a response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonRpcNotification {
    pub method: String,
    pub params: Value,
}

/// Parsed inbound JSON-RPC message. A message carrying an `id` is a request
/// that expects a response; an id-less message is a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub fn parse_message(body: &[u8]) -> Result<JsonRpcMessage, McpServerError> {
    let value = serde_json::from_slice::<Value>(body)
        .map_err(|err| McpServerError::Parse(err.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| invalid_request("request body must be a JSON object"))?;

    let jsonrpc = required_string(object, "jsonrpc")?;
    if jsonrpc != "2.0" {
        return Err(invalid_request("jsonrpc must be 2.0"));
    }

    let method = required_string(object, "method")?.to_owned();
    let params = object.get("params").cloned().unwrap_or(Value::Null);

    // An id-less message is a JSON-RPC notification (e.g.
    // `notifications/initialized`); a present id marks a request that expects a
    // response. A `null` id is neither valid request nor valid notification.
    let Some(id) = object.get("id").cloned() else {
        return Ok(JsonRpcMessage::Notification(JsonRpcNotification {
            method,
            params,
        }));
    };
    if id == Value::Null {
        return Err(invalid_request("id must not be null"));
    }

    Ok(JsonRpcMessage::Request(JsonRpcRequest {
        jsonrpc: jsonrpc.to_owned(),
        id,
        method,
        params,
    }))
}

#[must_use]
pub fn error_response(
    id: Value,
    code: i64,
    message: impl Into<String>,
    data: Option<Value>,
) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data,
        }),
    }
}

#[must_use]
pub fn success_response(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    }
}

#[must_use]
pub fn encode_sse_frame(response: &JsonRpcResponse) -> Bytes {
    encode_sse_frame_with(response, |frame, response| {
        serde_json::to_writer(frame, response)
    })
}

fn encode_sse_frame_with(
    response: &JsonRpcResponse,
    serialize: impl FnOnce(&mut Vec<u8>, &JsonRpcResponse) -> Result<(), serde_json::Error>,
) -> Bytes {
    let mut frame = Vec::with_capacity(128);
    frame.extend_from_slice(SSE_DATA_PREFIX.as_bytes());
    if !matches!(serialize(&mut frame, response), Ok(())) {
        frame.truncate(SSE_DATA_PREFIX.len());
        frame.extend_from_slice(INTERNAL_ERROR_FRAME);
    }
    frame.extend_from_slice(SSE_FRAME_SUFFIX.as_bytes());
    Bytes::from(frame)
}

pub fn validate_protocol_version(header: Option<&HeaderValue>) -> Result<(), McpServerError> {
    match header {
        Some(version) if version == PROTOCOL_VERSION => Ok(()),
        Some(_) => Err(invalid_request("unsupported MCP protocol version")),
        None => Ok(()),
    }
}

impl From<McpServerError> for JsonRpcError {
    fn from(err: McpServerError) -> Self {
        Self {
            code: error_code(&err),
            message: redacted_message(&err),
            data: None,
        }
    }
}

/// Client-facing error message for a `McpServerError`. Variants that wrap
/// internal detail (`KernelRun` carries `KernelError` Display with model and
/// provider specifics, `ToolExecution`, `Internal`) collapse to a generic
/// string so JSON-RPC error bodies never leak internals. Single source of
/// truth for both the `From` impl and the HTTP handler's error responses.
#[must_use]
pub fn redacted_message(err: &McpServerError) -> String {
    match err {
        McpServerError::KernelRun(_) => "kernel run failed".into(),
        McpServerError::ToolExecution(_) => "tool execution failed".into(),
        McpServerError::Internal(_) => "internal error".into(),
        McpServerError::AuthRequired => "authentication required".into(),
        McpServerError::MethodNotFound(_) | McpServerError::ToolNotFound(_) => {
            "method not found".into()
        }
        _ => err.to_string(),
    }
}

/// Canonical `McpServerError` -> JSON-RPC error code mapping. Single source of
/// truth: `McpServerError` is `#[non_exhaustive]`, so a new variant updates
/// only this `match`.
pub const fn error_code(err: &McpServerError) -> i64 {
    match err {
        McpServerError::Parse(_) => ERR_PARSE,
        McpServerError::InvalidRequest(_) => ERR_INVALID_REQUEST,
        McpServerError::MethodNotFound(_) | McpServerError::ToolNotFound(_) => ERR_METHOD_NOT_FOUND,
        McpServerError::InvalidParams(_) => ERR_INVALID_PARAMS,
        McpServerError::SessionNotFound => ERR_SESSION_NOT_FOUND,
        McpServerError::Internal(_)
        | McpServerError::AuthRequired
        | McpServerError::KernelRun(_)
        | McpServerError::ToolExecution(_) => ERR_INTERNAL,
    }
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a str, McpServerError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_request(format!("{key} is required")))
}

fn invalid_request(message: impl Into<String>) -> McpServerError {
    McpServerError::InvalidRequest(message.into())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parse_request_valid() {
        let message = parse_message(
            br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"cursor":null}}"#,
        )
        .expect("valid JSON-RPC request should parse");
        let JsonRpcMessage::Request(req) = message else {
            panic!("message with id parses as a request");
        };

        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.id, json!(1));
        assert_eq!(req.method, "tools/list");
        assert_eq!(req.params, json!({"cursor": null}));
    }

    #[test]
    fn parse_notification_without_id() {
        let message = parse_message(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .expect("id-less message parses as a notification");
        let JsonRpcMessage::Notification(notification) = message else {
            panic!("message without id parses as a notification");
        };

        assert_eq!(notification.method, "notifications/initialized");
        assert_eq!(notification.params, Value::Null);
    }

    #[test]
    fn parse_missing_method() {
        let err = parse_message(br#"{"jsonrpc":"2.0","id":1}"#)
            .expect_err("missing method must be invalid request");

        assert!(matches!(err, McpServerError::InvalidRequest(_)));
    }

    #[test]
    fn parse_malformed_json() {
        let err = parse_message(br#"{"jsonrpc":"2.0","id":1"#)
            .expect_err("malformed JSON must be parse error");

        assert!(matches!(err, McpServerError::Parse(_)));
    }

    #[test]
    fn parse_null_id_is_invalid_request() {
        let err = parse_message(br#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#)
            .expect_err("null id must be invalid request");

        assert!(
            matches!(err, McpServerError::InvalidRequest(message) if message == "id must not be null")
        );
    }

    #[test]
    fn encode_sse_frame_format() {
        let response = success_response(json!(1), json!({"ok": true}));
        let frame = encode_sse_frame(&response);
        let text = std::str::from_utf8(&frame).expect("SSE frame is UTF-8 JSON text");

        assert!(text.starts_with(SSE_DATA_PREFIX));
        assert!(text.ends_with(SSE_FRAME_SUFFIX));
        assert!(text.contains(r#""jsonrpc":"2.0""#));
    }

    #[test]
    fn encode_sse_frame_error_fallback_clean() {
        let response = success_response(json!(1), json!({"ok": true}));
        let frame = encode_sse_frame_with(&response, |frame, _response| {
            frame.extend_from_slice(br#"{"partial":"json""#);
            Err(serde_json::Error::io(std::io::Error::other(
                "forced write failure",
            )))
        });
        let text = std::str::from_utf8(&frame).expect("fallback SSE frame is UTF-8 JSON text");
        let fallback =
            std::str::from_utf8(INTERNAL_ERROR_FRAME).expect("fallback frame is UTF-8 JSON text");

        assert_eq!(
            text,
            format!("{SSE_DATA_PREFIX}{fallback}{SSE_FRAME_SUFFIX}")
        );
        assert!(!text.contains("partial"));
    }

    #[test]
    fn validate_protocol_version_match() {
        let header: &HeaderValue = PROTOCOL_VERSION;

        validate_protocol_version(Some(header)).expect("matching protocol version is valid");
    }

    #[test]
    fn validate_protocol_version_mismatch() {
        let err = validate_protocol_version(Some("2024-11-05"))
            .expect_err("mismatched protocol version must fail");

        assert!(matches!(err, McpServerError::InvalidRequest(_)));
    }

    #[test]
    fn error_response_shape() {
        let response = error_response(json!("abc"), ERR_INVALID_PARAMS, "bad params", None);
        let value = serde_json::to_value(response).expect("response serializes");

        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], "abc");
        assert_eq!(value["error"]["code"], ERR_INVALID_PARAMS);
        assert!(value.get("result").is_none());
    }

    #[test]
    fn success_response_shape() {
        let response = success_response(json!(7), json!({"answer": 42}));
        let value = serde_json::to_value(response).expect("response serializes");

        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
        assert_eq!(value["result"]["answer"], 42);
        assert!(value.get("error").is_none());
    }

    #[test]
    fn server_error_maps_to_json_rpc_error() {
        let error = JsonRpcError::from(McpServerError::SessionNotFound);

        assert_eq!(error.code, ERR_SESSION_NOT_FOUND);
        assert_eq!(error.message, "session not found");
        assert_eq!(error.data, None);
    }

    #[test]
    fn kernel_run_error_message_is_redacted() {
        let inner = "model gpt-4o provider-secret-detail";
        let error = JsonRpcError::from(McpServerError::KernelRun(
            crabgent_core::KernelError::Internal(inner.into()),
        ));

        assert_eq!(error.code, ERR_INTERNAL);
        assert_eq!(error.message, "kernel run failed");
        assert!(
            !error.message.contains(inner),
            "JSON-RPC error body must not leak KernelError detail"
        );
    }
}
