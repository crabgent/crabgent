//! Multi-agent runtime: spawn each agent's poller, the global cron
//! scheduler, hold task-executors, wait on shutdown.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crabgent_core::Kernel;
use crabgent_cron::{CronExecutor, CronScheduler, KernelCronExecutor};
use crabgent_log::{error, info, warn};
use crabgent_store::{Page, Store, TaskStore};
use crabgent_store_sqlite::{SqliteCronStore, SqliteStore, SqliteTaskStore};
use crabgent_task::TaskExecutor;
use secrecy::SecretString;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, AgentPoller, build as build_agent};
use crate::agent_message::{AgentDirectory, DirectoryEntry};
use crate::config::{Config, McpClientConfig};
use crabgent_channel::{ChannelInbox, ChannelSink};
use crabgent_core::model::{ModelId, ModelTarget};

const KERNEL_PAUSE_GRACE: Duration = Duration::from_secs(30);

/// Handle for a single running agent. One agent may run on multiple
/// channels (matrix + telegram), each with its own poller task + inbox
/// stack. All channels feed the SAME kernel.
pub struct AgentHandle {
    pub name: String,
    pub poller_tasks: Vec<JoinHandle<()>>,
    pub kernel: Arc<Kernel>,
    pub channel_sink: Arc<dyn ChannelSink>,
    pub inboxes: Vec<Arc<dyn ChannelInbox>>,
    pub task_executor: Arc<TaskExecutor<SqliteTaskStore>>,
    /// Out-of-run compaction handle + model tool, forwarded to the TUI
    /// bridge so its `/compact` and `/model` commands hit the live agent.
    pub compact_hook: Arc<crabgent_hook_compact::CompactHook>,
    pub goal_runtime: crabgent_hook_goal::GoalRuntime,
    pub model_tool: Option<Arc<crabgent_tool_models::ModelRegistryTool>>,
    pub inject_registry: crabgent_hook_inject::InjectionRegistry,
    pub voice_tts: Option<crate::agent::AgentVoiceTts>,
    pub voice_stt: Option<crate::agent::AgentVoiceStt>,
}

/// Spawn one tokio-task per `[[agents]]` entry, plus a global
/// cron-scheduler bound to the first agent's kernel.
pub async fn spawn_all(cfg: &Config, sqlite: &SqliteStore) -> Result<Runtime> {
    let pair_dir = pairing_dir(&cfg.sqlite_path)?;
    tokio::fs::create_dir_all(&pair_dir)
        .await
        .with_context(|| format!("create pairing-dir {}", pair_dir.display()))?;
    let image_cache_root = cfg.image_cache_path();
    tokio::fs::create_dir_all(&image_cache_root)
        .await
        .with_context(|| format!("create image-cache-dir {}", image_cache_root.display()))?;
    // Shared by every agent's ErrorAuditHook; the parent dir is the
    // store directory, already created by `open_sqlite`.
    let error_audit_path = cfg.error_audit_path();
    let mcp_tools = discover_mcp_tools(&cfg.mcp).await;
    let openai_token = load_openai_token(cfg).await?;
    let embedding_provider = build_embedding_provider(cfg).await?;
    let tui_hub = crate::tui_channel::TuiHub::new();
    let activity_hub = crate::tui_activity::ActivityHub::new();
    let cancel = CancellationToken::new();
    let agent_directory = AgentDirectory::new();
    let handles = build_handles(
        cfg,
        sqlite,
        openai_token.as_ref(),
        &pair_dir,
        &image_cache_root,
        &error_audit_path,
        &mcp_tools,
        embedding_provider.as_ref(),
        tui_hub.clone(),
        activity_hub.clone(),
        &cancel,
        &agent_directory,
    )
    .await?;
    drop(openai_token);
    resume_restart_state(sqlite, &handles).await;
    let (cron_handle, cron_scheduler) =
        spawn_cron(cfg, sqlite, &handles, &activity_hub, cancel.clone())
            .map_or((None, None), |(h, s)| (Some(h), Some(s)));
    let mcp_handle = spawn_mcp(
        cfg,
        sqlite,
        &handles,
        &tui_hub,
        &activity_hub,
        cancel.clone(),
    );
    Ok(Runtime {
        handles,
        cancel,
        cron_handle,
        cron_scheduler,
        mcp_handle,
    })
}

fn spawn_mcp(
    cfg: &Config,
    sqlite: &SqliteStore,
    handles: &[AgentHandle],
    tui_hub: &crate::tui_channel::TuiHub,
    activity_hub: &crate::tui_activity::ActivityHub,
    cancel: CancellationToken,
) -> Option<JoinHandle<()>> {
    let Some(mcp_cfg) = cfg.mcp_server.as_ref() else {
        if cfg.web.is_some() {
            warn!("[web] is configured but [mcp_server] is not; admin UI cannot start");
        }
        return None;
    };
    let mut bindings = Vec::new();
    let mut tui_agents = Vec::new();
    let mut web_voice_agents = Vec::new();
    for (agent_cfg, handle) in cfg.agents.iter().zip(handles.iter()) {
        web_voice_agents.push(web_voice_agent(agent_cfg, handle));
        let mcp_token = non_empty_token(agent_cfg.mcp_bearer_token.as_ref());
        if let Some(token) = mcp_token {
            bindings.push(mcp_binding(agent_cfg, handle, token));
        } else if agent_cfg
            .mcp_bearer_token
            .as_ref()
            .is_some_and(|token| token.trim().is_empty())
        {
            warn!(agent = %agent_cfg.name, "mcp-http: mcp_bearer_token is empty, skipping MCP route");
        }

        let tui_token = non_empty_token(agent_cfg.tui_bearer_token.as_ref()).or(mcp_token);
        if let Some(token) = tui_token {
            tui_agents.push(tui_agent(
                agent_cfg,
                handle,
                sqlite,
                tui_hub,
                activity_hub,
                token,
            ));
        } else if agent_cfg
            .tui_bearer_token
            .as_ref()
            .is_some_and(|token| token.trim().is_empty())
        {
            warn!(agent = %agent_cfg.name, "mcp-http: tui_bearer_token is empty, skipping TUI route");
        }
    }
    let admin_router = cfg.web.as_ref().map(|web| {
        let agent_names = cfg.agents.iter().map(|a| a.name.clone()).collect();
        let auth_token = SecretString::from(web.auth_token.clone());
        let state = crate::web_admin::WebAdminState::new(
            sqlite.memory().clone(),
            sqlite.cron().clone(),
            sqlite.session().clone(),
            agent_names,
            &auth_token,
        );
        info!("mcp-http: web admin enabled at /admin");
        crate::web_admin::build_router(state).merge(crate::web_voice::build_router(
            web_voice_agents,
            &auth_token,
        ))
    });
    // Fold the TUI WebSocket router (`/tui/<agent>`) into the same axum app
    // that serves `/mcp/<agent>` and `/admin`, so the TUI attaches to the
    // already-running agents over the existing port.
    let admin_router = match (admin_router, crate::tui_ws::build_router(tui_agents)) {
        (Some(admin), Some(tui)) => Some(admin.merge(tui)),
        (Some(admin), None) => Some(admin),
        (None, Some(tui)) => Some(tui),
        (None, None) => None,
    };
    if bindings.is_empty() && admin_router.is_none() {
        info!("mcp-http: no MCP/TUI agents and no [web] block, server not started");
        return None;
    }
    let bind = mcp_cfg.bind.clone();
    let handle = tokio::spawn(async move {
        if let Err(err) = crate::mcp_http::run(&bind, bindings, admin_router, cancel).await {
            error!(error = %err, "mcp-http: server exited with error");
        } else {
            info!("mcp-http: server exited cleanly");
        }
    });
    Some(handle)
}

fn non_empty_token(token: Option<&String>) -> Option<&String> {
    token.filter(|token| !token.trim().is_empty())
}

fn web_voice_agent(
    agent_cfg: &crate::config::Agent,
    handle: &AgentHandle,
) -> crate::web_voice::WebVoiceAgent {
    crate::web_voice::WebVoiceAgent {
        name: handle.name.clone(),
        kernel: Arc::clone(&handle.kernel),
        model: agent_cfg.model.clone(),
        system_prompt: agent_cfg.system_prompt.clone(),
        fallbacks: agent_cfg.fallback_models.clone(),
        max_turns: agent_cfg.max_turns,
        inject_registry: handle.inject_registry.clone(),
        tts: handle.voice_tts.clone(),
        stt: handle.voice_stt.clone(),
    }
}

fn mcp_binding(
    agent_cfg: &crate::config::Agent,
    handle: &AgentHandle,
    token: &str,
) -> crate::mcp_http::AgentMcpBinding {
    crate::mcp_http::AgentMcpBinding {
        name: handle.name.clone(),
        kernel: Arc::clone(&handle.kernel),
        default_model: agent_cfg.model.clone(),
        bearer_token: SecretString::from(token.to_owned()),
    }
}

fn tui_agent(
    agent_cfg: &crate::config::Agent,
    handle: &AgentHandle,
    sqlite: &SqliteStore,
    tui_hub: &crate::tui_channel::TuiHub,
    activity_hub: &crate::tui_activity::ActivityHub,
    token: &str,
) -> crate::tui_ws::TuiAgent {
    crate::tui_ws::TuiAgent {
        name: handle.name.clone(),
        kernel: Arc::clone(&handle.kernel),
        model: agent_cfg.model.clone(),
        system_prompt: agent_cfg.system_prompt.clone(),
        fallbacks: agent_cfg.fallback_models.clone(),
        max_turns: agent_cfg.max_turns,
        reasoning_effort: agent_cfg.reasoning_effort.clone(),
        session_store: Arc::new(sqlite.session().clone()),
        global_model_store: Arc::new(sqlite.global_override().clone()),
        global_effort_store: Arc::new(sqlite.global_override().clone()),
        compact_hook: Arc::clone(&handle.compact_hook),
        goal_runtime: handle.goal_runtime.clone(),
        model_tool: handle.model_tool.clone(),
        inject_registry: handle.inject_registry.clone(),
        tui_hub: tui_hub.clone(),
        activity_hub: activity_hub.clone(),
        bearer_token: SecretString::from(token.to_owned()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn build_handles(
    cfg: &Config,
    sqlite: &SqliteStore,
    openai_token: Option<&crate::openai_oauth::OpenAiTokenSource>,
    pair_dir: &Path,
    image_cache_root: &Path,
    error_audit_path: &Path,
    mcp_tools: &[Arc<dyn crabgent_core::Tool>],
    embedding_provider: Option<&Arc<dyn crabgent_core::EmbeddingProvider>>,
    tui_hub: crate::tui_channel::TuiHub,
    activity_hub: crate::tui_activity::ActivityHub,
    cancel: &CancellationToken,
    agent_directory: &Arc<AgentDirectory>,
) -> Result<Vec<AgentHandle>> {
    let mut handles = Vec::with_capacity(cfg.agents.len());
    let openai_api_key = cfg.openai.as_ref().and_then(|o| o.api_key.as_deref());
    let openai_image_api_key = cfg.openai.as_ref().and_then(|o| o.image_api_key.as_deref());
    let google_api_key = cfg.google.as_ref().map(|g| g.api_key.as_str());
    let cortecs_cfg = cfg.cortecs.as_ref();
    for a in &cfg.agents {
        let agent = build_agent(
            a,
            &cfg.memory,
            cfg.stt.as_ref(),
            cfg.voice.as_ref(),
            sqlite,
            openai_token,
            openai_api_key,
            openai_image_api_key,
            google_api_key,
            cortecs_cfg,
            pair_dir,
            image_cache_root,
            error_audit_path,
            mcp_tools,
            embedding_provider.cloned(),
            Arc::clone(agent_directory),
            tui_hub.clone(),
            activity_hub.clone(),
        )
        .await?;
        info!(agent = %agent.name, "spawning channel poller");
        let kernel = Arc::clone(&agent.kernel);
        let agent_name = agent.name.clone();
        handles.push(spawn_one(agent, cancel));
        let entry = DirectoryEntry {
            name: agent_name,
            kernel,
            model: ModelTarget::id(ModelId::new(&a.model)),
            system_prompt: Some(a.system_prompt.clone()),
            max_turns: a.max_turns,
            fallbacks: a
                .fallback_models
                .iter()
                .map(|id| ModelTarget::id(ModelId::new(id)))
                .collect(),
        };
        agent_directory.register(Arc::new(entry)).await;
    }
    Ok(handles)
}

/// Resolve the `OpenAI` token path, attempt to load + refresh on
/// startup. Returns `None` when no `[openai]` section is configured
/// or the token cache is missing. Hard-errors only on malformed JSON
/// or filesystem failures.
async fn load_openai_token(cfg: &Config) -> Result<Option<crate::openai_oauth::OpenAiTokenSource>> {
    // An explicit api_key short-circuits OAuth loading entirely: the
    // provider builder will use ApiKeyAuth instead.
    if cfg
        .openai
        .as_ref()
        .and_then(|o| o.api_key.as_deref())
        .is_some()
    {
        return Ok(None);
    }
    let needs_openai = cfg
        .agents
        .iter()
        .any(|a| a.provider == crate::config::AgentProvider::OpenAi);
    if !needs_openai {
        return Ok(None);
    }
    let path = match cfg.openai.as_ref().and_then(|o| o.token_path.clone()) {
        Some(path) => path,
        None => crate::openai_oauth::default_token_path()?,
    };
    let token = crate::openai_oauth::load_or_refresh(&path).await?;
    if token.is_none() {
        warn!(
            path = %path.display(),
            "agent.provider=openai but no usable cached token. Run `openai-login` first."
        );
    }
    Ok(token.map(|token| crate::openai_oauth::OpenAiTokenSource { path, token }))
}

async fn build_embedding_provider(
    cfg: &Config,
) -> Result<Option<Arc<dyn crabgent_core::EmbeddingProvider>>> {
    let Some(embed_cfg) = cfg.embedding.as_ref() else {
        info!("embedding: disabled (no [embedding] block); memory falls back to FTS-only");
        return Ok(None);
    };
    match embed_cfg {
        crate::config::EmbeddingConfig::Fastembed => build_fastembed_provider().await,
        crate::config::EmbeddingConfig::Openai {
            base_url,
            model,
            dim,
            api_key,
        } => build_openai_embedding_provider(base_url, model, *dim, api_key),
    }
}

// Result mirrors build_fastembed_provider's fallible signature so both arms of
// the EmbeddingConfig dispatch share one return type.
#[allow(clippy::unnecessary_wraps)]
fn build_openai_embedding_provider(
    base_url: &str,
    model: &str,
    dim: usize,
    api_key: &str,
) -> Result<Option<Arc<dyn crabgent_core::EmbeddingProvider>>> {
    use crabgent_core::ModelId;
    use crabgent_provider_openai::OpenAiEmbeddingProvider;
    use secrecy::SecretString;
    info!(
        base_url = base_url,
        model = model,
        dim = dim,
        "embedding: using OpenAI-compatible remote endpoint"
    );
    let provider = OpenAiEmbeddingProvider::with_openai_compatible_base_url(
        SecretString::from(api_key.to_owned()),
        base_url,
        ModelId::new(model),
        dim,
    );
    Ok(Some(
        Arc::new(provider) as Arc<dyn crabgent_core::EmbeddingProvider>
    ))
}

#[cfg(feature = "fastembed")]
async fn build_fastembed_provider() -> Result<Option<Arc<dyn crabgent_core::EmbeddingProvider>>> {
    info!("embedding: loading FastEmbed BGE-M3 (ONNX), this can take a moment on first start");
    let provider =
        tokio::task::spawn_blocking(|| crabgent_embedding_fastembed::FastEmbedProvider::bge_m3())
            .await
            .context("embedding: fastembed init thread panicked")?
            .context("embedding: FastEmbed BGE-M3 init failed")?;
    info!(
        "embedding: FastEmbed BGE-M3 ready (dim={})",
        crabgent_embedding_fastembed::FastEmbedProvider::default_dim(),
    );
    Ok(Some(
        Arc::new(provider) as Arc<dyn crabgent_core::EmbeddingProvider>
    ))
}

#[cfg(not(feature = "fastembed"))]
#[allow(clippy::unused_async)]
async fn build_fastembed_provider() -> Result<Option<Arc<dyn crabgent_core::EmbeddingProvider>>> {
    warn!(
        "embedding: [embedding].provider = \"fastembed\" requested but this binary \
         was built without the `fastembed` cargo feature; recall falls back to FTS-only",
    );
    Ok(None)
}

async fn discover_mcp_tools(cfg: &McpClientConfig) -> Vec<Arc<dyn crabgent_core::Tool>> {
    use crabgent_mcp_client::{McpServerConfig, discover_servers};
    if cfg.servers.is_empty() {
        return Vec::new();
    }
    let mut server_configs = Vec::with_capacity(cfg.servers.len());
    for entry in &cfg.servers {
        match McpServerConfig::new(&entry.name, &entry.base_url) {
            Ok(mut c) => {
                if let Some(token) = &entry.token {
                    c = c.with_token(SecretString::from(token.clone()));
                }
                server_configs.push(c);
            }
            Err(err) => {
                warn!(server = %entry.name, error = ?err, "MCP server config invalid; skipping");
            }
        }
    }
    // Discovery must never hang startup: a server that accepts the TCP
    // connection but stalls the MCP handshake (e.g. a peer host mid-restart)
    // would otherwise block the whole process before any agent spawns,
    // because the HTTP client has no default timeout. Bound it and soft-fail
    // to no-tools on timeout, same as an unreachable server.
    let factories = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        discover_servers(&server_configs),
    )
    .await
    .unwrap_or_else(|_| {
        warn!("MCP discovery timed out after 15s; starting without MCP tools");
        Vec::new()
    });
    let mut tools = Vec::new();
    for factory in factories {
        tools.extend(factory.into_tools());
    }
    // Apply per-server tool allowlists (config `tools = [...]`). Discovery
    // prefixes every tool name `{server}__{tool}`; a server without an
    // allowlist keeps all of its tools.
    let allow: std::collections::HashMap<&str, &Vec<String>> = cfg
        .servers
        .iter()
        .filter_map(|e| e.tools.as_ref().map(|list| (e.name.as_str(), list)))
        .collect();
    if !allow.is_empty() {
        let before = tools.len();
        tools.retain(|tool| mcp_tool_allowed(&allow, tool.name()));
        info!(
            kept = tools.len(),
            dropped = before - tools.len(),
            "MCP tool allowlist applied"
        );
    }
    info!(mcp_tool_count = tools.len(), "MCP tool discovery done");
    tools
}

/// Keep a discovered MCP tool only if its server has no allowlist or the
/// tool's unprefixed name is on it. Discovered names are `{server}__{tool}`.
fn mcp_tool_allowed(
    allow: &std::collections::HashMap<&str, &Vec<String>>,
    tool_name: &str,
) -> bool {
    let (server, bare) = tool_name.split_once("__").unwrap_or(("", tool_name));
    allow
        .get(server)
        .is_none_or(|list| list.iter().any(|name| name == bare))
}

fn spawn_one(agent: Agent, cancel: &CancellationToken) -> AgentHandle {
    let name = agent.name.clone();
    let kernel = Arc::clone(&agent.kernel);
    let channel_sink = Arc::clone(&agent.channel_sink);
    let task_executor = Arc::clone(&agent.task_executor);
    let compact_hook = Arc::clone(&agent.compact_hook);
    let goal_runtime = agent.goal_runtime.clone();
    let model_tool = agent.model_tool.clone();
    let inject_registry = agent.inject_registry.clone();
    let voice_tts = agent.voice_tts.clone();
    let voice_stt = agent.voice_stt.clone();
    let mut poller_tasks = Vec::with_capacity(agent.pollers.len());
    let mut inboxes = Vec::with_capacity(agent.pollers.len());
    for (poller, inbox) in agent.pollers {
        inboxes.push(inbox);
        let label = name.clone();
        let kind = match &poller {
            AgentPoller::Telegram(_) => "telegram",
            AgentPoller::Matrix(_) => "matrix",
        };
        info!(agent = %name, channel = kind, "spawning channel poller");
        let cancel_for_task = cancel.clone();
        poller_tasks.push(tokio::spawn(async move {
            let result = match poller {
                AgentPoller::Telegram(p) => p.run(cancel_for_task).await,
                AgentPoller::Matrix(p) => p.run(cancel_for_task).await,
            };
            if let Err(err) = result {
                error!(agent = %label, channel = kind, "channel poller exited with error: {err}");
                return;
            }
            info!(agent = %label, channel = kind, "channel poller exited cleanly");
        }));
    }
    AgentHandle {
        name,
        poller_tasks,
        kernel,
        channel_sink,
        inboxes,
        task_executor,
        compact_hook,
        goal_runtime,
        model_tool,
        inject_registry,
        voice_tts,
        voice_stt,
    }
}

fn spawn_cron(
    cfg: &Config,
    sqlite: &SqliteStore,
    handles: &[AgentHandle],
    activity_hub: &crate::tui_activity::ActivityHub,
    cancel: CancellationToken,
) -> Option<(JoinHandle<()>, Arc<CronScheduler<SqliteCronStore>>)> {
    let Some(first) = handles.first() else {
        warn!("no agents configured, skipping cron scheduler");
        return None;
    };
    let cron_store = Arc::new(sqlite.cron().clone());

    // Build a per-agent KernelCronExecutor so each cron job runs under
    // its OWN agent's kernel + system_prompt + model. No default fallback:
    // jobs without (or with an unknown) scope.agent fail loudly rather
    // than running under the wrong identity.
    let mut dispatch = crate::cron_dispatch::AgentDispatchCronExecutor::new();
    for (agent_cfg, handle) in cfg.agents.iter().zip(handles.iter()) {
        let (kernel, exec) = build_cron_exec_for(
            agent_cfg,
            &handle.kernel,
            crate::tui_activity::ActivityHub::clone(activity_hub),
        );
        dispatch.insert(agent_cfg.name.clone(), kernel, exec);
    }

    let channel_cron_delivery = Arc::new(crate::cron_delivery::ChannelCronDelivery::from_handles(
        handles,
    ));
    let error_delivery: Arc<dyn crabgent_cron::CronDelivery> = channel_cron_delivery;
    let final_delivery: Arc<dyn crabgent_cron::CronDelivery> = Arc::new(
        crate::cron_delivery::FinalTextCronDelivery::new(error_delivery.clone()),
    );
    let cron_executor: Arc<dyn CronExecutor> = Arc::new(
        crate::cron_dispatch::ErrorDeliveringCronExecutor::new(Arc::new(dispatch), error_delivery),
    );
    let scheduler_observer: Arc<dyn crabgent_cron::CronObserver> =
        Arc::new(crate::tui_activity::TuiCronObserver::global(
            crate::tui_activity::ActivityHub::clone(activity_hub),
        ));
    // CronScheduler::new requires an Arc<Kernel>, but our dispatch
    // executor ignores ctx.kernel and resolves per-job. Pass first.kernel
    // purely to satisfy the constructor.
    let scheduler = CronScheduler::new(cron_store, Arc::clone(&first.kernel), cron_executor)
        .with_job_timeout(Duration::from_hours(1))
        .with_max_concurrent(32)
        .with_observer(scheduler_observer)
        .with_delivery(final_delivery)
        .with_pre_processor(Arc::new(
            crate::shell_pre_processor::ShellPreProcessor::new(),
        ));
    let scheduler = Arc::new(scheduler);
    info!(default_agent = %first.name, "spawning cron scheduler");
    let scheduler_for_task = Arc::clone(&scheduler);
    let handle = tokio::spawn(async move {
        if let Err(err) = scheduler_for_task.run(cancel).await {
            error!("cron scheduler exited with error: {err}");
        } else {
            info!("cron scheduler exited cleanly");
        }
    });
    Some((handle, scheduler))
}

async fn resume_restart_state(sqlite: &SqliteStore, handles: &[AgentHandle]) {
    resume_suspended_goals(handles).await;
    resume_paused_tasks(sqlite, handles).await;
}

async fn resume_suspended_goals(handles: &[AgentHandle]) {
    let Some(first) = handles.first() else {
        return;
    };
    match first.goal_runtime.resume_suspended().await {
        Ok(goals) if goals.is_empty() => {}
        Ok(goals) => {
            info!(
                count = goals.len(),
                "resumed system-suspended goals after restart"
            );
        }
        Err(err) => warn!(error = %err, "failed to resume system-suspended goals"),
    }
}

async fn resume_paused_tasks(sqlite: &SqliteStore, handles: &[AgentHandle]) {
    for handle in handles {
        let agent = handle.name.clone();
        let kernel = Arc::clone(&handle.kernel);
        match handle
            .task_executor
            .resume_paused_with(move |task| {
                let task_agent = task
                    .resume_spec
                    .as_ref()
                    .and_then(|spec| spec.subject_attrs.get("agent"))
                    .map(String::as_str);
                (task_agent == Some(agent.as_str())).then(|| Arc::clone(&kernel))
            })
            .await
        {
            Ok(ids) if ids.is_empty() => {}
            Ok(ids) => info!(
                agent = %handle.name,
                count = ids.len(),
                "resumed paused tasks after restart"
            ),
            Err(err) => warn!(
                agent = %handle.name,
                error = %err,
                "failed to resume paused tasks after restart"
            ),
        }
    }
    report_unmatched_paused_tasks(sqlite, handles).await;
}

async fn report_unmatched_paused_tasks(sqlite: &SqliteStore, handles: &[AgentHandle]) {
    let known_agents: Vec<&str> = handles.iter().map(|handle| handle.name.as_str()).collect();
    match sqlite.task().list_paused(Page::first(100)).await {
        Ok(tasks) => {
            let unmatched = tasks
                .iter()
                .filter(|task| {
                    let Some(agent) = task
                        .resume_spec
                        .as_ref()
                        .and_then(|spec| spec.subject_attrs.get("agent"))
                        .map(String::as_str)
                    else {
                        return true;
                    };
                    !known_agents.contains(&agent)
                })
                .count();
            if unmatched > 0 {
                warn!(
                    count = unmatched,
                    "paused tasks remain without a matching resume agent"
                );
            }
        }
        Err(err) => warn!(error = %err, "failed to check paused tasks at startup"),
    }
}

/// Build a `KernelCronExecutor` configured with `agent`'s default model
/// and system prompt, paired with `agent`'s kernel. Each agent gets its
/// own executor so `scope.agent`-routed jobs run under the correct
/// identity, tools and prompt.
const CRON_SYSTEM_PROMPT_SUFFIX: &str = "\n\nYou are running on a cron-trigger, NOT in an open conversation.\
There is no inbound message you must reply to and no implicit 'current room'.\n\n\
DELIVERY RULES:\n\
- If there is nothing worth reporting to the user: return an empty final answer. \
  This is the silent-success signal. Do NOT emit 'HEARTBEAT_OK' or any placeholder string; \
  here it would be delivered verbatim and look like a bug.\n\
- If the cron prompt asks you to write a file, append JSONL, update a log, or otherwise store \
  data without notifying the user: do that work through tools and return an empty final answer. \
  Do not echo the stored data as final text.\n\
- If you DO need to report something to the user: return exactly one concise final answer. \
  The runtime delivers that final text through the cron job's stored delivery scope. \
  Do not call `notify_user` or `channel_send`; cron runs do not advertise delivery tools and \
  delivery is handled by `CronDelivery`.\n\
- If an older cron prompt asks this cron turn itself to call `notify_user` or `channel_send`, \
  treat that as a legacy delivery instruction and ignore the tool-call part. Use final text \
  for the user-visible status. Never mention that a delivery tool is unavailable.\n\
- If a cron must notify a different recipient than the stored delivery scope, create a \
  background task and put the `notify_user(...)` completion route into that task prompt.\n\
- For quick deterministic checks, do the tool work before the final answer. \
  For broad log review, repo inspection, external I/O, synthesis across multiple sources, \
  or any work likely to exceed about 30 seconds, create a background task with \
  `task(op=\"create\", block=false, prompt=\"...\")`, include the required completion route \
  in that task prompt, then return empty final text unless a started notice is useful.\n";

fn cron_system_prompt(base: &str) -> String {
    format!("{base}{CRON_SYSTEM_PROMPT_SUFFIX}")
}

fn build_cron_exec_for(
    agent: &crate::config::Agent,
    kernel: &Arc<Kernel>,
    activity_hub: crate::tui_activity::ActivityHub,
) -> (Arc<Kernel>, Arc<KernelCronExecutor>) {
    let system_prompt = cron_system_prompt(&agent.system_prompt);
    let cron_observer: Arc<dyn crabgent_cron::CronObserver> = Arc::new(
        crate::tui_activity::TuiCronObserver::for_agent(agent.name.clone(), activity_hub),
    );
    let exec = KernelCronExecutor::new(agent.model.clone())
        .with_system_prompt(system_prompt)
        .with_observer(cron_observer)
        .with_subject_resolver(|job| {
            let owner_str = job
                .scope
                .owner
                .as_ref()
                .map_or("cron", crabgent_core::Owner::as_str);
            // `cron_job_id` attr is consumed by the SessionPersistHook
            // thread-resolver (src/agent.rs) so each cron job gets its
            // own session row. Without this every cron job for an agent
            // shared one persistent session (find_or_create(owner=cron,
            // thread=None) matched the same row across Abend-Check,
            // Mail-Check, GTD-Pull, etc.), accumulating 100+ messages
            // and forcing the LLM to pick a topic from the stew instead
            // of executing the current cron's prompt.
            let mut subject =
                crabgent_core::Subject::new(owner_str).with_attr("cron_job_id", job.id.to_string());
            if let Some(rest) = owner_str.strip_prefix("matrix:") {
                subject = subject.with_attr("participant_id", rest);
            } else if let Some(rest) = owner_str.strip_prefix("telegram:") {
                subject = subject.with_attr("participant_id", rest);
            }
            if let Some(channel) = &job.scope.channel {
                subject = subject.with_attr("channel", channel);
            }
            if let Some(conv) = &job.scope.conv {
                subject = subject.with_attr("conv", conv);
            }
            if let Some(agent) = &job.scope.agent {
                subject = subject.with_attr("agent", agent);
            }
            if let Some(kind) = &job.scope.kind {
                subject = subject.with_attr("channel_kind", kind);
            }
            if let Some(channel) = cron_delivery_ctx_str(job, "channel") {
                subject = subject.with_attr("delivery_channel", channel);
            }
            if let Some(conv) = cron_delivery_ctx_str(job, "conv") {
                subject = subject.with_attr("delivery_conv", conv);
            }
            if let Some(participant) = cron_delivery_participant(job) {
                subject = subject.with_attr("delivery_participant_id", participant);
            }
            subject
        });
    (Arc::clone(kernel), Arc::new(exec))
}

fn cron_delivery_participant(job: &crabgent_store::records::CronJob) -> Option<&str> {
    cron_delivery_ctx_str(job, "participant_id")
        .or_else(|| cron_delivery_ctx_str(job, "recipient"))
        .or_else(|| cron_delivery_ctx_str(job, "user"))
}

fn cron_delivery_ctx_str<'a>(
    job: &'a crabgent_store::records::CronJob,
    key: &str,
) -> Option<&'a str> {
    job.delivery_ctx
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn pairing_dir(sqlite_path: &Path) -> Result<std::path::PathBuf> {
    sqlite_path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("sqlite_path has no parent: {}", sqlite_path.display()))
}

/// Aggregated runtime view: agent handles + cron handle + cancel token.
pub struct Runtime {
    pub handles: Vec<AgentHandle>,
    pub cancel: CancellationToken,
    pub cron_handle: Option<JoinHandle<()>>,
    pub cron_scheduler: Option<Arc<CronScheduler<SqliteCronStore>>>,
    pub mcp_handle: Option<JoinHandle<()>>,
}

impl Runtime {
    /// Cancel all background tasks, then await their join.
    pub async fn shutdown(self) {
        info!(count = self.handles.len(), "shutting down agents");
        self.cancel.cancel();
        // Drain kernel-consumers first so they release in-flight kernel runs
        // before the kernel itself drops. Cron jobs and inbox-spawned runs
        // both call kernel.run(); task executor manages its own sub-runs.
        if let Some(scheduler) = self.cron_scheduler {
            info!("draining cron scheduler");
            scheduler.shutdown().await;
        }
        if let Some(cron) = self.cron_handle
            && let Err(err) = cron.await
        {
            error!("join error on cron shutdown: {err}");
        }
        for h in self.handles {
            for (idx, task) in h.poller_tasks.into_iter().enumerate() {
                if let Err(err) = task.await {
                    error!(agent = %h.name, poller = idx, "join error on shutdown: {err}");
                }
            }
            let grace = h.kernel.shutdown_grace();
            info!(agent = %h.name, ?grace, count = h.inboxes.len(), "draining inboxes");
            for inbox in &h.inboxes {
                inbox.shutdown(grace).await;
            }
            info!(agent = %h.name, "draining task executor");
            h.task_executor.shutdown().await;
            info!(agent = %h.name, "draining in-flight kernel runs");
            h.kernel.shutdown_with_pause(KERNEL_PAUSE_GRACE).await;
            drop(h.kernel);
            drop(h.inboxes);
            drop(h.task_executor);
        }
        if let Some(mcp) = self.mcp_handle
            && let Err(err) = mcp.await
        {
            error!("join error on mcp-http shutdown: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{cron_system_prompt, mcp_tool_allowed};

    #[test]
    fn allowed_when_server_has_no_allowlist() {
        let allow = HashMap::new();
        assert!(mcp_tool_allowed(&allow, "assistant__bash"));
    }

    #[test]
    fn allowlist_keeps_listed_and_drops_others() {
        let chat = vec!["chat".to_owned()];
        let mut allow = HashMap::new();
        allow.insert("assistant", &chat);
        assert!(mcp_tool_allowed(&allow, "assistant__chat"));
        assert!(!mcp_tool_allowed(&allow, "assistant__bash"));
        assert!(!mcp_tool_allowed(&allow, "assistant__file_write"));
    }

    #[test]
    fn allowlist_only_constrains_its_own_server() {
        let chat = vec!["chat".to_owned()];
        let mut allow = HashMap::new();
        allow.insert("assistant", &chat);
        // A different server has no entry, so its tools pass through.
        assert!(mcp_tool_allowed(&allow, "other__bash"));
    }

    #[test]
    fn cron_prompt_uses_final_text_delivery() {
        let prompt = cron_system_prompt("base prompt");

        assert!(prompt.contains("CronDelivery"));
        assert!(prompt.contains("return exactly one concise final answer"));
        assert!(prompt.contains("Do not call `notify_user` or `channel_send`"));
        assert!(prompt.contains("legacy delivery instruction"));
        assert!(prompt.contains("Never mention that a delivery tool is unavailable"));
        assert!(prompt.contains("task(op=\"create\", block=false"));
        assert!(prompt.contains("broad log review"));
        assert!(!prompt.contains("Plain-text replies WITHOUT a tool call"));
        assert!(!prompt.contains("call `notify_user(user="));
    }
}
