use thiserror::Error;

#[derive(Debug, Error)]
pub enum McpError {
    #[error("MCP HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("MCP JSON-RPC error {code}: {message}")]
    JsonRpc { code: i64, message: String },

    #[error("invalid MCP config: {0}")]
    InvalidConfig(String),

    #[error("MCP discovery failed: {0}")]
    Discovery(String),

    #[error("MCP tool call failed: {0}")]
    ToolCall(String),

    #[error("MCP server authentication failed")]
    AuthFailed,

    #[error("MCP session not found")]
    SessionNotFound,

    #[error("MCP output cap exceeded")]
    OutputCapExceeded,

    #[error("MCP request cancelled")]
    Cancelled,

    #[error("MCP decode failed: {0}")]
    Decode(String),
}
