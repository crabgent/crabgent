//! Streamable HTTP and SSE MCP client integration for crabgent.

/// Alias keeps `#[crabgent_log::instrument]` proc-macro expansion (which emits
/// `::tracing::*` paths) resolving to `crabgent_log` without re-introducing a
/// direct `tracing` dep.
extern crate crabgent_log as tracing;

pub mod client;
pub mod config;
pub mod discovery;
pub mod error;
pub mod tool;
pub mod types;

pub use client::{McpClient, McpClientBuilder};
pub use config::McpServerConfig;
pub use discovery::discover_servers;
pub use error::McpError;
pub use tool::{McpTool, McpToolFactory};
pub use types::{JsonRpcError, JsonRpcResponse, McpCallResult, McpToolDef, McpToolList};
