//! Multi-agent MCP HTTP server.
//!
//! Wraps `crabgent_mcp_server::McpHandler` per agent in a single
//! axum router that routes `POST /mcp/<agent_name>` to that agent's
//! handler. Authentication is per-agent: each agent supplies its own
//! `mcp_bearer_token` in config; the handler enforces it via the
//! existing `verify_bearer`. Agents without a token are not exposed.
//!
//! Designed for one-port deployments: local single-agent installs and remote
//! multi-agent hosts use the same code path.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Router,
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::{HeaderMap as AxumHeaderMap, HeaderName, HeaderValue, StatusCode},
    response::Response,
    routing::post,
};
use crabgent_core::{Kernel, ModelTarget};
use crabgent_log::{info, warn};
use crabgent_mcp_server::{McpHandler, McpServer, McpServerConfig};
use secrecy::SecretString;
use tokio_util::sync::CancellationToken;

pub struct AgentMcpBinding {
    pub name: String,
    pub kernel: Arc<Kernel>,
    pub default_model: String,
    pub bearer_token: SecretString,
}

#[derive(Clone)]
struct AppState {
    handlers: Arc<HashMap<String, McpHandler>>,
}

pub async fn run(
    bind: &str,
    agents: Vec<AgentMcpBinding>,
    admin_router: Option<Router>,
    cancel: CancellationToken,
) -> Result<()> {
    if agents.is_empty() && admin_router.is_none() {
        info!("mcp-http: nothing to serve (no agents, no admin), skipping server");
        return Ok(());
    }
    #[allow(clippy::literal_string_with_formatting_args)]
    // axum route param {agent}, not a format placeholder
    let mcp_router = if agents.is_empty() {
        None
    } else {
        let mut handlers: HashMap<String, McpHandler> = HashMap::new();
        for agent in agents {
            let config = McpServerConfig::new(
                agent.bearer_token,
                ModelTarget::id(agent.default_model.as_str()),
            );
            let server = McpServer::builder()
                .with_kernel(Arc::clone(&agent.kernel))
                .with_config(config)
                .build()
                .with_context(|| format!("build mcp server for agent {}", agent.name))?;
            handlers.insert(agent.name.clone(), McpHandler::new(Arc::new(server)));
            info!(agent = %agent.name, "mcp-http: route mounted POST /mcp/{}", agent.name);
        }
        let state = AppState {
            handlers: Arc::new(handlers),
        };
        Some(
            Router::new()
                .route("/mcp/{agent}", post(dispatch))
                .with_state(state),
        )
    };
    let app = match (mcp_router, admin_router) {
        (Some(mcp), Some(admin)) => {
            info!("mcp-http: mounted MCP routes + web admin");
            mcp.merge(admin)
        }
        (Some(mcp), None) => mcp,
        (None, Some(admin)) => {
            info!("mcp-http: mounted web admin only");
            admin
        }
        (None, None) => unreachable!("guard above"),
    };
    let addr: SocketAddr = bind
        .parse()
        .with_context(|| format!("parse mcp_server.bind {bind:?}"))?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind mcp http on {addr}"))?;
    info!(addr = %addr, "mcp-http: listening");
    let drain_grace = std::time::Duration::from_secs(5);
    let cancel_for_serve = cancel.clone();
    let serve = axum::serve(listener, app.into_make_service()).with_graceful_shutdown(async move {
        cancel_for_serve.cancelled().await;
        info!("mcp-http: shutdown signal received, draining connections");
    });
    let drain_cap = async {
        cancel.cancelled().await;
        tokio::time::sleep(drain_grace).await;
    };
    tokio::select! {
        result = serve => {
            result.context("axum serve")?;
        }
        () = drain_cap => {
            warn!(?drain_grace, "mcp-http: connection drain timed out, dropping");
        }
    }
    Ok(())
}

async fn dispatch(
    State(state): State<AppState>,
    AxumPath(agent): AxumPath<String>,
    headers: AxumHeaderMap,
    body: Bytes,
) -> Response {
    let Some(handler) = state.handlers.get(&agent) else {
        warn!(agent, "mcp-http: unknown agent path");
        return not_found();
    };
    // JSON-RPC notifications (no `id`, e.g. notifications/initialized) are
    // accepted with an empty 202 inside `McpHandler::dispatch` upstream, so
    // there is no local interception here.
    let header_vec = to_mcp_headers(&headers);
    let mcp_response = handler.dispatch(&header_vec, &body).await;
    to_axum_response(mcp_response)
}

fn to_mcp_headers(headers: &AxumHeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_owned(), v.to_owned()))
        })
        .collect()
}

fn to_axum_response(response: crabgent_mcp_server::McpResponse) -> Response {
    let status =
        StatusCode::from_u16(response.status_code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = Response::builder().status(status);
    for (name, value) in response.headers {
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            builder = builder.header(name, value);
        }
    }
    builder
        .body(axum::body::Body::from(response.body))
        .unwrap_or_else(|_| internal_error_fallback())
}

fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(axum::body::Body::empty())
        .expect("static response builds")
}

fn internal_error_fallback() -> Response {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body(axum::body::Body::empty())
        .expect("static response builds")
}
