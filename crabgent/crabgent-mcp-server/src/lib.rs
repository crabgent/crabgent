//! MCP server-side adapter for crabgent.

mod auth;
mod builder;
mod config;
mod error;
mod handler;
mod session;
mod tools;
mod wire;

pub use auth::{AUTHORIZATION_HEADER, HeaderMap, verify_bearer};
pub use builder::{McpServer, McpServerBuilder, ToolFilter};
pub use config::{MCP_PROTOCOL_VERSION, McpServerConfig};
pub use error::McpServerError;
pub use handler::{MCP_SESSION_ID_HEADER, MCP_VERSION_HEADER, McpHandler, McpResponse};
pub use session::{McpSessionEntry, McpSessionId, McpSessionRegistry};
pub use wire::{
    ERR_INTERNAL, ERR_INVALID_PARAMS, ERR_INVALID_REQUEST, ERR_METHOD_NOT_FOUND, ERR_PARSE,
    ERR_SESSION_NOT_FOUND, HeaderValue, JsonRpcError, JsonRpcMessage, JsonRpcNotification,
    JsonRpcRequest, JsonRpcResponse, PROTOCOL_VERSION, encode_sse_frame, error_response,
    parse_message, success_response, validate_protocol_version,
};
