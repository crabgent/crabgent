use crabgent_core::KernelError;
use thiserror::Error;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpServerError {
    #[error("parse error: {0}")]
    Parse(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("method not found: {0}")]
    MethodNotFound(String),

    #[error("invalid params: {0}")]
    InvalidParams(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("session not found")]
    SessionNotFound,

    #[error("authentication required")]
    AuthRequired,

    #[error("tool not found: {0}")]
    ToolNotFound(String),

    #[error("kernel run failed: {0}")]
    KernelRun(#[from] KernelError),

    #[error("tool execution failed: {0}")]
    ToolExecution(String),
}
