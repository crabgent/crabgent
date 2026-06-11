//! Multi-agent local and chat host.
//!
//! User journey: open TUI/Web or write a Matrix/Telegram message to one of the
//! configured agents -> the agent's `Kernel` runs the LLM with sessions,
//! memory, tool-cache compaction and tools -> the response returns through the
//! same channel.

mod agent;
mod agent_message;
mod audio_native_stt;
mod brand;
mod channel_read_adapter;
mod config;
mod cron_delivery;
mod cron_dispatch;
mod dump_hook;
mod error_audit_hook;
mod flight_recorder;
mod generate_image_tool;
mod hear_again_stt;
mod invite;
mod mcp_http;
mod memory_recall_hook;
mod memory_scope;
mod openai_oauth;
mod reasoning_hook;
mod runtime;
mod session_persisting_sink;
mod shell_pre_processor;
mod skill_scope_wrapper;
mod speaker_id;
mod temperature_hook;
mod tmux_channel;
mod tui_activity;
mod tui_channel;
mod tui_client;
mod tui_ws;
mod usage_relay;
mod voice_output;
mod web_admin;
mod web_search_hook;
mod web_voice;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crabgent_log::{error, info};
use crabgent_store_sqlite::SqliteStore;

#[derive(Parser, Debug)]
#[command(
    name = "crabgent",
    about = "Multi-agent local, web, Matrix and Telegram host using crabgent"
)]
struct Cli {
    /// Path to the TOML config file.
    #[arg(short, long, default_value = "config.toml", env = "CRABGENT_CONFIG")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the multi-agent runtime (default if no subcommand given).
    Run,
    /// Interactive `OpenAI` `OAuth` login. Opens a browser, listens on
    /// port 1455 for the `OAuth` callback, persists the token JSON
    /// under the app's config directory, or `[openai].token_path` if set.
    /// (or `[openai].token_path` if set).
    OpenaiLogin,
    /// Interactive TUI client. Connects to a running daemon's
    /// `/tui/<agent>` WebSocket and drives a chat session against the live
    /// agent kernel.
    Tui {
        /// Agent to connect to. Defaults to the first agent in the config.
        agent: Option<String>,
        /// Daemon host:port. Defaults to the config's `mcp_server.bind`.
        #[arg(long)]
        host: Option<String>,
        /// Named TUI session to open. Defaults to the main session.
        #[arg(long)]
        session: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();
    let command = args.command.unwrap_or(Command::Run);
    // The TUI owns the terminal: tracing to stdout would corrupt the
    // ratatui screen, so it is the one path that skips `init_tracing`.
    if !matches!(command, Command::Tui { .. }) {
        init_tracing();
    }
    match command {
        Command::Run => run(args.config).await,
        Command::OpenaiLogin => openai_login(args.config).await,
        Command::Tui {
            agent,
            host,
            session,
        } => tui(args.config, agent, host, session).await,
    }
}

async fn tui(
    config_path: PathBuf,
    agent: Option<String>,
    host: Option<String>,
    session: Option<String>,
) -> Result<()> {
    let agent = match agent {
        Some(agent) => agent,
        None => tui_client::default_agent(&config_path)?,
    };
    let session = tui_client::normalize_session_arg(session.as_deref())?;
    let token = tui_client::resolve_token(&config_path, &agent)?;
    let host = host
        .or_else(|| tui_client::default_host(&config_path))
        .context("no --host given and config has no [mcp_server] bind")?;
    tui_client::run(config_path, agent, host, token, session).await
}

async fn run(config_path: PathBuf) -> Result<()> {
    info!(config = %config_path.display(), "starting runtime");
    let cfg = config::Config::load(&config_path)?;
    info!(agents = cfg.agents.len(), "config loaded");
    let sqlite = open_sqlite(&cfg.sqlite_path).await?;
    let runtime = runtime::spawn_all(&cfg, &sqlite).await?;
    wait_for_shutdown_signal().await;
    runtime.shutdown().await;
    info!("runtime exited");
    Ok(())
}

async fn openai_login(config_path: PathBuf) -> Result<()> {
    let token_path = if config_path.exists() {
        config::Config::load_unresolved(&config_path)
            .ok()
            .and_then(|cfg| cfg.openai.and_then(|o| o.token_path))
            .map_or_else(openai_oauth::default_token_path, Ok)?
    } else {
        openai_oauth::default_token_path()?
    };
    let token = openai_oauth::login().await?;
    openai_oauth::write_token(&token_path, &token)?;
    eprintln!("Token saved at {}", token_path.display());
    if let Some(account) = &token.account_id {
        eprintln!("Account: {account}");
    }
    Ok(())
}

/// Idempotent via `try_init`, safe to call multiple times.
fn init_tracing() {
    crabgent_log::init("info,matrix_sdk=warn,matrix_sdk_base=warn");
}

async fn open_sqlite(path: &std::path::Path) -> Result<SqliteStore> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create sqlite parent {}", parent.display()))?;
    }
    SqliteStore::open(path)
        .await
        .with_context(|| format!("open sqlite at {}", path.display()))
}

async fn wait_for_shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(err) => {
            error!("install SIGTERM handler failed: {err}; falling back to SIGINT-only");
            if let Err(err) = tokio::signal::ctrl_c().await {
                error!("ctrl_c await failed: {err}");
            }
            return;
        }
    };
    tokio::select! {
        res = tokio::signal::ctrl_c() => {
            if let Err(err) = res {
                error!("ctrl_c await failed: {err}");
            } else {
                info!("received SIGINT, shutting down");
            }
        }
        _ = sigterm.recv() => {
            info!("received SIGTERM, shutting down");
        }
    }
}
