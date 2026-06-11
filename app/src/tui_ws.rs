//! TUI WebSocket bridge.
//!
//! `GET /tui/<agent>` upgrades to a WebSocket that drives `run_streaming`
//! on the agent's ALREADY-RUNNING kernel (the same `Arc<Kernel>` the
//! daemon's Matrix/Telegram channels use) and streams every `Event` back
//! as one JSON text frame. The TUI is a thin client that connects here
//! and renders the live token / reasoning / tool-call stream. It does not own
//! a kernel; it attaches to the live agents, like any other channel client.
//!
//! Wire protocol (both directions are line-oriented JSON text frames):
//!   client -> server: `{"prompt": "<user text>"}`
//!                     `{"prompt": "<user text>", "steering": true}`
//!   server -> client: the serde-tagged `Event` JSON
//!                      (`{"kind":"token","data":"..."}`, `reasoning`,
//!                      `tool_call_started`, `tool_call_completed`,
//!                      `final`, ...), then a terminal
//!                      `{"kind":"turn_error","data":"..."}` on failure.
//!   Every turn ends with `Event::Final` (success) or `turn_error`.
//!
//! Auth is the per-agent `tui_bearer_token`, falling back to
//! `mcp_bearer_token`, supplied either as `Authorization: Bearer <token>`
//! or `?token=<token>`, compared in constant time. Agents without a token
//! are not exposed here.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use axum::{
    Router,
    extract::{
        Path, Query, State,
        ws::{Message as WsMessage, Utf8Bytes, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use crabgent_core::message::ContentBlock;
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::{
    Event, GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, Kernel, MemoryScope,
    Message, ModelId, ModelTarget, Owner, ReasoningEffort, RunId, RunRequest, Subject,
    WebSearchConfig,
};
use crabgent_hook_compact::{CompactHook, token_count::estimate_tokens as estimate_compact_tokens};
use crabgent_hook_goal::GoalRuntime;
use crabgent_hook_inject::InjectionRegistry;
use crabgent_log::{info, warn};
use crabgent_store::{Session, ThreadGoal, traits::SessionStore};
use crabgent_tool_models::ModelRegistryTool;
use futures::{SinkExt, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

/// Per-agent runtime handle the TUI bridge drives. Built in
/// `runtime::spawn_mcp` from the live `AgentHandle` plus the agent's config.
pub struct TuiAgent {
    pub name: String,
    pub kernel: Arc<Kernel>,
    pub model: String,
    pub system_prompt: String,
    pub fallbacks: Vec<String>,
    pub max_turns: Option<u32>,
    /// Per-agent configured `reasoning_effort` (`low`/`medium`/`high`), the
    /// same string the `ReasoningEffortHook` consumes. Surfaced in the
    /// status line so the user sees the effort the model actually runs at.
    pub reasoning_effort: Option<String>,
    /// Session store, shared with the kernel's `SessionPersistHook` (same
    /// `SQLite` backing). Lets the bridge resolve the per-`tui:<agent>`
    /// session and read its model / effort overrides for the status line.
    pub session_store: Arc<dyn SessionStore>,
    /// Global model + reasoning-effort override stores, shared with the
    /// kernel (same `SQLite` backing). Read for the status line's
    /// override-source detection.
    pub global_model_store: Arc<dyn GlobalModelOverrideStore>,
    pub global_effort_store: Arc<dyn GlobalReasoningEffortOverrideStore>,
    /// Out-of-run compaction handle for the `/compact` command. Same
    /// `CompactHook` the kernel runs, so it compacts the live session.
    pub compact_hook: Arc<CompactHook>,
    /// Host-side goal runtime for the `/goal` command.
    pub goal_runtime: GoalRuntime,
    /// Model registry tool for the `/model` command, when this agent has one.
    pub model_tool: Option<Arc<ModelRegistryTool>>,
    /// Same registry used by the live kernel's `InjectHook`.
    pub inject_registry: InjectionRegistry,
    /// In-process channel for `notify_user(channel="tui", ...)` delivery.
    pub tui_hub: crate::tui_channel::TuiHub,
    /// Background task and cron progress feed for this agent's TUI sessions.
    pub activity_hub: crate::tui_activity::ActivityHub,
    pub bearer_token: SecretString,
}

struct AgentEntry {
    kernel: Arc<Kernel>,
    model: String,
    system_prompt: String,
    fallbacks: Vec<ModelTarget>,
    max_turns: Option<u32>,
    reasoning_effort: Option<String>,
    session_store: Arc<dyn SessionStore>,
    global_model_store: Arc<dyn GlobalModelOverrideStore>,
    global_effort_store: Arc<dyn GlobalReasoningEffortOverrideStore>,
    compact_hook: Arc<CompactHook>,
    goal_runtime: GoalRuntime,
    model_tool: Option<Arc<ModelRegistryTool>>,
    inject_registry: InjectionRegistry,
    tui_hub: crate::tui_channel::TuiHub,
    activity_hub: crate::tui_activity::ActivityHub,
    /// `sha256(bearer_token)` for constant-time comparison; the raw token
    /// never lives in the router state.
    token_hash: [u8; 32],
}

#[derive(Clone)]
struct TuiState {
    agents: Arc<HashMap<String, Arc<AgentEntry>>>,
}

type WsSink = futures::stream::SplitSink<WebSocket, WsMessage>;
type WsStream = futures::stream::SplitStream<WebSocket>;

const TUI_SYSTEM_PROMPT: &str = r#"## TUI session
You are talking to the user through the local TUI.

Normal replies should be plain final assistant text. If you need to push a separate message into this active TUI session, call `notify_user` with `channel="tui"` and `participant_id="{agent}"`. Write the TUI notification body as plain Markdown or plain text, not Matrix or Telegram HTML. The TUI renders that notification like an agent message in the same terminal session. Do not use `tmux` for TUI delivery.

When you call `channel_send` or `notify_user` from TUI to another adapter, format the `body` for the target adapter. Matrix targets need org.matrix.custom.html HTML with short paragraphs, real lists, short inline `<code>` only for exact tokens, and `<pre>` for dense logs. Telegram targets need Telegram-safe HTML."#;
const TUI_AGENT_PLACEHOLDER: &str = "{agent}";

#[derive(Debug, PartialEq, Eq)]
enum ClientInput {
    Prompt { text: String, steering: bool },
    Cancel,
    Ignore,
    Closed,
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
    #[serde(default)]
    session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TuiSession {
    name: Option<String>,
}

impl TuiSession {
    const MAIN: &'static str = "main";
    const MAX_CHARS: usize = 80;

    const fn main() -> Self {
        Self { name: None }
    }

    fn parse(raw: Option<&str>) -> Result<Self, String> {
        let Some(raw) = raw else {
            return Ok(Self::main());
        };
        let name = raw.trim();
        if name.is_empty() || name.eq_ignore_ascii_case(Self::MAIN) {
            return Ok(Self::main());
        }
        if name.chars().count() > Self::MAX_CHARS {
            return Err(format!(
                "session name too long; max {} chars",
                Self::MAX_CHARS
            ));
        }
        if name.chars().any(char::is_control) {
            return Err("session name must not contain control characters".to_owned());
        }
        if name.contains('/') {
            return Err("session name must not contain '/'".to_owned());
        }
        Ok(Self {
            name: Some(name.to_owned()),
        })
    }

    const fn is_main(&self) -> bool {
        self.name.is_none()
    }

    fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(Self::MAIN)
    }

    fn topic(&self, agent: &str) -> String {
        self.name
            .as_ref()
            .map_or_else(|| agent.to_owned(), |name| format!("{agent}/{name}"))
    }

    fn conv(&self, agent: &str) -> String {
        format!("tui:{}", self.topic(agent))
    }
}

/// Inbound client frame: a user prompt or a client-side control op.
#[derive(Deserialize)]
struct ClientFrame {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    op: Option<String>,
    #[serde(default)]
    steering: bool,
}

fn parse_client_text(text: String) -> ClientInput {
    let input = serde_json::from_str::<ClientFrame>(&text).map_or_else(
        |_| ClientInput::Prompt {
            text,
            steering: false,
        },
        |frame| {
            if frame.op.as_deref() == Some("cancel") {
                ClientInput::Cancel
            } else if let Some(text) = frame.prompt {
                ClientInput::Prompt {
                    text,
                    steering: frame.steering,
                }
            } else {
                ClientInput::Ignore
            }
        },
    );
    let ClientInput::Prompt { text, steering } = input else {
        return input;
    };
    if text.trim().is_empty() {
        ClientInput::Ignore
    } else {
        ClientInput::Prompt { text, steering }
    }
}

/// Build the `/tui/<agent>` router, or `None` when no agent is exposed.
#[must_use]
pub fn build_router(agents: Vec<TuiAgent>) -> Option<Router> {
    if agents.is_empty() {
        return None;
    }
    let mut map: HashMap<String, Arc<AgentEntry>> = HashMap::new();
    for a in agents {
        let entry = AgentEntry {
            kernel: a.kernel,
            model: a.model,
            system_prompt: a.system_prompt,
            fallbacks: a
                .fallbacks
                .iter()
                .map(|m| ModelTarget::id(ModelId::new(m)))
                .collect(),
            max_turns: a.max_turns,
            reasoning_effort: a.reasoning_effort,
            session_store: a.session_store,
            global_model_store: a.global_model_store,
            global_effort_store: a.global_effort_store,
            compact_hook: a.compact_hook,
            goal_runtime: a.goal_runtime,
            model_tool: a.model_tool,
            inject_registry: a.inject_registry,
            tui_hub: a.tui_hub,
            activity_hub: a.activity_hub,
            token_hash: sha256(a.bearer_token.expose_secret().as_bytes()),
        };
        info!(agent = %a.name, "tui-ws: route mounted GET /tui/{}", a.name);
        map.insert(a.name, Arc::new(entry));
    }
    let state = TuiState {
        agents: Arc::new(map),
    };
    Some(
        Router::new()
            .route("/tui/{agent}", get(upgrade))
            .with_state(state),
    )
}

async fn upgrade(
    State(state): State<TuiState>,
    Path(agent): Path<String>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(entry) = state.agents.get(&agent).cloned() else {
        warn!(agent, "tui-ws: unknown agent");
        return (StatusCode::NOT_FOUND, "unknown agent").into_response();
    };
    let presented = bearer_from(&headers).or(q.token);
    let Some(token) = presented else {
        return (StatusCode::UNAUTHORIZED, "missing token").into_response();
    };
    if !constant_eq(&sha256(token.as_bytes()), &entry.token_hash) {
        warn!(agent, "tui-ws: token mismatch");
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }
    let session = match TuiSession::parse(q.session.as_deref()) {
        Ok(session) => session,
        Err(err) => return (StatusCode::BAD_REQUEST, err).into_response(),
    };
    let agent_name = agent;
    ws.on_upgrade(move |socket| handle_socket(socket, entry, agent_name, session))
}

/// Per-connection loop: each inbound prompt drives one streamed turn.
/// Conversation history is kept here so multi-turn context works within a
/// single TUI session.
#[allow(clippy::too_many_lines)]
async fn handle_socket(
    socket: WebSocket,
    entry: Arc<AgentEntry>,
    agent: String,
    session: TuiSession,
) {
    let (mut sink, mut stream) = socket.split();
    let topic = session.topic(&agent);
    let (mut tui_rx, tui_backlog) = entry.tui_hub.subscribe_with_backlog(&topic).await;
    let mut activity_rx = entry.activity_hub.subscribe(&agent).await;
    info!(agent, session = session.label(), "tui-ws: client connected");

    // Initial status so the client populates model / effort / override
    // source before the first turn is sent.
    if send_status(&mut sink, &entry, &agent, &session)
        .await
        .is_err()
    {
        return;
    }
    if send_history(&mut sink, &entry, &agent, &session)
        .await
        .is_err()
    {
        return;
    }
    for delivery in tui_backlog {
        if send_tui_delivery(&mut sink, delivery).await.is_err() {
            return;
        }
    }

    loop {
        let input = tokio::select! {
            input = read_client_input(&mut stream, &agent, false) => {
                match input {
                    ClientInput::Prompt { .. } => input,
                    ClientInput::Cancel | ClientInput::Ignore => continue,
                    ClientInput::Closed => break,
                }
            }
            delivery = tui_rx.recv() => {
                match delivery {
                    Ok(delivery) => {
                        if send_tui_delivery(&mut sink, delivery).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(agent, skipped, "tui-ws: dropped lagged notification frames");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
                continue;
            }
            activity = activity_rx.recv() => {
                match activity {
                    Ok(activity) => {
                        if send_activity_delivery(&mut sink, activity).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(agent, skipped, "tui-ws: dropped lagged activity frames");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
                continue;
            }
        };
        let ClientInput::Prompt {
            text: prompt,
            steering,
        } = input
        else {
            continue;
        };

        // Slash commands run in-process against the live agent handles
        // (no channel sink, no crabgent-command dispatch). Most commands only
        // emit a `notice` frame, but `/goal <objective>` and `/goal resume`
        // are host-side triggers for the autonomous goal loop.
        let mut next_turn = if !steering && prompt.starts_with('/') {
            let Ok(flow) = handle_command(&mut sink, &entry, &agent, &session, prompt.trim()).await
            else {
                break;
            };
            if flow.continue_goal {
                goal_continuation(&entry, &agent, &session).await
            } else {
                continue;
            }
        } else {
            Some(if steering {
                PendingTurn::FollowUp(vec![prompt])
            } else {
                PendingTurn::User(prompt)
            })
        };
        while let Some(turn) = next_turn.take() {
            if let Some(count) = turn.follow_up_count()
                && send_json(
                    &mut sink,
                    "notice",
                    &steering_notice("steering applied", count),
                )
                .await
                .is_err()
            {
                break;
            }
            if matches!(&turn, PendingTurn::GoalContinuation(_))
                && send_json(&mut sink, "notice", "goal continuation")
                    .await
                    .is_err()
            {
                break;
            }
            let (run_id, req) = build_tui_request(&entry, &agent, &session, turn.into_messages());
            let outcome = drive_active_turn(
                &mut sink,
                &mut stream,
                &mut tui_rx,
                &mut activity_rx,
                &entry,
                &agent,
                &session,
                &run_id,
                req,
            )
            .await;
            if outcome.close {
                // Drop the recorded usage for the dead run; the sink is gone.
                let _ = crate::usage_relay::take(&run_id);
                break;
            }
            if send_usage(&mut sink, &run_id).await.is_err() {
                break;
            }
            // Refresh status after the turn: a mid-turn fallback or a future
            // /model change can move the effective model or effort.
            if send_status(&mut sink, &entry, &agent, &session)
                .await
                .is_err()
            {
                break;
            }
            next_turn = PendingTurn::follow_up(outcome.unconsumed_prompts);
            if next_turn.is_none() && outcome.continue_goal {
                next_turn = goal_continuation(&entry, &agent, &session).await;
            }
        }
    }
    info!(
        agent,
        session = session.label(),
        "tui-ws: client disconnected"
    );
}

async fn read_client_input(stream: &mut WsStream, agent: &str, active_turn: bool) -> ClientInput {
    let Some(frame) = stream.next().await else {
        return ClientInput::Closed;
    };
    let text = match frame {
        Ok(WsMessage::Text(t)) => t.to_string(),
        Ok(WsMessage::Close(_)) => return ClientInput::Closed,
        Ok(WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Binary(_)) => {
            return ClientInput::Ignore;
        }
        Err(err) => {
            if active_turn {
                warn!(agent, error = %err, "tui-ws: recv error during active turn, closing");
            } else {
                warn!(agent, error = %err, "tui-ws: recv error, closing");
            }
            return ClientInput::Closed;
        }
    };
    parse_client_text(text)
}

enum PendingTurn {
    User(String),
    FollowUp(Vec<String>),
    GoalContinuation(Vec<Message>),
}

impl PendingTurn {
    fn follow_up(prompts: Vec<String>) -> Option<Self> {
        (!prompts.is_empty()).then_some(Self::FollowUp(prompts))
    }

    const fn follow_up_count(&self) -> Option<usize> {
        match self {
            Self::FollowUp(prompts) => Some(prompts.len()),
            Self::User(_) | Self::GoalContinuation(_) => None,
        }
    }

    fn into_messages(self) -> Vec<Message> {
        match self {
            Self::User(prompt) => vec![user_text_message(prompt)],
            Self::FollowUp(prompts) => prompts.into_iter().map(user_text_message).collect(),
            Self::GoalContinuation(messages) => messages,
        }
    }
}

fn user_text_message(text: String) -> Message {
    Message::user(vec![ContentBlock::Text { text }])
}

async fn goal_continuation(
    entry: &AgentEntry,
    agent: &str,
    tui_session: &TuiSession,
) -> Option<PendingTurn> {
    let session = resolve_session(entry, agent, tui_session).await?;
    entry
        .goal_runtime
        .continuation_input(&session.id)
        .await
        .ok()
        .flatten()
        .map(PendingTurn::GoalContinuation)
}

fn build_tui_request(
    entry: &AgentEntry,
    agent: &str,
    session: &TuiSession,
    messages: Vec<Message>,
) -> (RunId, RunRequest) {
    // The TUI is a thin channel: send ONLY the new user turn. The kernel's
    // `SessionPersistHook` prepends the persisted TUI session history.
    let run_id = RunId::new();
    let req = RunRequest {
        run_id: run_id.clone(),
        subject: tui_subject(agent, session),
        model: ModelTarget::id(ModelId::new(&entry.model)),
        explicit_model: None,
        session_model_override: None,
        fallbacks: entry.fallbacks.clone(),
        messages,
        system_prompt: Some(tui_system_prompt(&entry.system_prompt, agent, session)),
        max_turns: entry.max_turns,
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        pause: None,
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
    };
    (run_id, req)
}

fn tui_subject(agent: &str, session: &TuiSession) -> Subject {
    let owner = format!("tui:{agent}");
    let mut subject = Subject::new(owner)
        .with_attr("agent", agent)
        .with_attr("channel", "tui")
        .with_attr("conv", session.conv(agent))
        .with_attr("channel_kind", "direct");
    if !session.is_main() {
        subject = subject.with_attr("tui_session", session.label());
    }
    subject
}

fn tui_owner(agent: &str) -> Owner {
    Owner::new(format!("tui:{agent}"))
}

fn tui_scope(agent: &str, session: &TuiSession) -> MemoryScope {
    let owner = tui_owner(agent);
    let mut scope = MemoryScope::from_subject(&tui_subject(agent, session));
    scope.owner = Some(owner);
    scope
}

fn tui_system_prompt(base: &str, agent: &str, session: &TuiSession) -> String {
    let session_note = if session.is_main() {
        "Current TUI session: main. This is the default terminal session."
    } else {
        "Current TUI session: named. Treat it as a separate conversation thread from the main TUI session and from other named TUI sessions."
    };
    format!(
        "{}\n\n{}\n\n{} Name: {}",
        base,
        TUI_SYSTEM_PROMPT.replace(TUI_AGENT_PLACEHOLDER, agent),
        session_note,
        session.label()
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn drive_active_turn(
    sink: &mut WsSink,
    stream: &mut WsStream,
    tui_rx: &mut tokio::sync::broadcast::Receiver<crate::tui_channel::TuiDelivery>,
    activity_rx: &mut tokio::sync::broadcast::Receiver<crate::tui_activity::ActivityDelivery>,
    entry: &AgentEntry,
    agent: &str,
    session: &TuiSession,
    run_id: &RunId,
    req: RunRequest,
) -> ActiveTurnOutcome {
    let cancel = CancellationToken::new();
    let run = entry.kernel.run_streaming(req, Some(&cancel));
    tokio::pin!(run);
    let mut unconsumed_prompts = VecDeque::new();
    loop {
        tokio::select! {
            item = run.next() => {
                let Some(item) = item else {
                    return ActiveTurnOutcome::completed(unconsumed_prompts);
                };
                match item {
                    Ok(event) => {
                        let final_event = matches!(event, Event::Final(_));
                        if !unconsumed_prompts.is_empty() {
                            let consumed = if final_event {
                                reconcile_prompt_consumption(
                                    &mut unconsumed_prompts,
                                    PromptConsumption::Final,
                                )
                            } else {
                                let pending_count = entry.inject_registry.pending(run_id).await;
                                reconcile_prompt_consumption(
                                    &mut unconsumed_prompts,
                                    PromptConsumption::NonFinal { pending_count },
                                )
                            };
                            if consumed > 0
                                && send_json(
                                    sink,
                                    "notice",
                                    &steering_notice("steering applied", consumed),
                                )
                                .await
                                .is_err()
                            {
                                return ActiveTurnOutcome::closed();
                            }
                        }
                        if send_event(sink, &event).await.is_err() {
                            return ActiveTurnOutcome::closed();
                        }
                        if final_event {
                            if !unconsumed_prompts.is_empty() {
                                info!(
                                    agent,
                                    run_id = %run_id,
                                    count = unconsumed_prompts.len(),
                                    "tui-ws: steering carried to follow-up"
                                );
                            }
                            return ActiveTurnOutcome::completed(unconsumed_prompts);
                        }
                    }
                    Err(err) => {
                        let _ = send_json(sink, "turn_error", &err.to_string()).await;
                        return ActiveTurnOutcome::interrupted();
                    }
                }
            }
            input = read_client_input(stream, agent, true) => {
                match input {
                    ClientInput::Prompt { text: prompt, .. } if prompt.starts_with('/') => {
                        if handle_command(sink, entry, agent, session, prompt.trim())
                            .await
                            .is_err()
                        {
                            cancel.cancel();
                            return ActiveTurnOutcome::closed();
                        }
                    }
                    ClientInput::Prompt { text: prompt, .. } => {
                        entry.inject_registry.submit_user_text(run_id, prompt.clone()).await;
                        unconsumed_prompts.push_back(prompt);
                        info!(agent, run_id = %run_id, "tui-ws: steering queued");
                        if send_json(sink, "notice", "steering queued").await.is_err() {
                            return ActiveTurnOutcome::closed();
                        }
                    }
                    ClientInput::Cancel => {
                        cancel.cancel();
                        let _ = send_json(sink, "notice", "turn cancelled").await;
                        return ActiveTurnOutcome::interrupted();
                    }
                    ClientInput::Ignore => {}
                    ClientInput::Closed => {
                        cancel.cancel();
                        return ActiveTurnOutcome::closed();
                    }
                }
            }
            delivery = tui_rx.recv() => {
                match delivery {
                    Ok(delivery) => {
                        if send_tui_delivery(sink, delivery).await.is_err() {
                            cancel.cancel();
                            return ActiveTurnOutcome::closed();
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(agent, skipped, "tui-ws: dropped lagged notification frames during active turn");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        cancel.cancel();
                        return ActiveTurnOutcome::closed();
                    }
                }
            }
            activity = activity_rx.recv() => {
                match activity {
                    Ok(activity) => {
                        if send_activity_delivery(sink, activity).await.is_err() {
                            cancel.cancel();
                            return ActiveTurnOutcome::closed();
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(agent, skipped, "tui-ws: dropped lagged activity frames during active turn");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        cancel.cancel();
                        return ActiveTurnOutcome::closed();
                    }
                }
            }
        }
    }
}

struct ActiveTurnOutcome {
    close: bool,
    unconsumed_prompts: Vec<String>,
    continue_goal: bool,
}

impl ActiveTurnOutcome {
    const fn closed() -> Self {
        Self {
            close: true,
            unconsumed_prompts: Vec::new(),
            continue_goal: false,
        }
    }

    fn completed(prompts: VecDeque<String>) -> Self {
        Self {
            close: false,
            unconsumed_prompts: prompts.into_iter().collect(),
            continue_goal: true,
        }
    }

    const fn interrupted() -> Self {
        Self {
            close: false,
            unconsumed_prompts: Vec::new(),
            continue_goal: false,
        }
    }
}

fn drop_consumed_prompts(prompts: &mut VecDeque<String>, pending_count: usize) -> usize {
    let consumed = prompts.len().saturating_sub(pending_count);
    for _ in 0..consumed {
        let _ = prompts.pop_front();
    }
    consumed
}

#[derive(Clone, Copy)]
enum PromptConsumption {
    NonFinal { pending_count: usize },
    Final,
}

fn reconcile_prompt_consumption(
    prompts: &mut VecDeque<String>,
    consumption: PromptConsumption,
) -> usize {
    match consumption {
        PromptConsumption::NonFinal { pending_count } => {
            drop_consumed_prompts(prompts, pending_count)
        }
        PromptConsumption::Final => 0,
    }
}

fn steering_notice(base: &str, count: usize) -> String {
    if count <= 1 {
        base.to_owned()
    } else {
        format!("{base} ({count})")
    }
}

async fn send_usage(sink: &mut WsSink, run_id: &RunId) -> Result<(), axum::Error> {
    let Some(u) = crate::usage_relay::take(run_id) else {
        return Ok(());
    };
    let frame = serde_json::json!({
        "kind": "usage",
        "data": {
            "input": u.input_tokens,
            "output": u.output_tokens,
            "cache_read": u.cache_read_tokens,
        },
    })
    .to_string();
    sink.send(WsMessage::Text(frame.into())).await
}

/// One-line help for the TUI command set.
const COMMAND_HELP: &str = "commands: /compact  ·  /model  ·  /model list  ·  /model set <id>  ·  \
     /model clear  ·  /model effort <low|medium|high|clear>  ·  /goal [objective|pause|resume|clear]  ·  \
     client: /agent <name>  ·  /session <name|main>  ·  /help";

/// Dispatch a `/`-prefixed command against the live agent handles and reply
/// with a `notice` frame plus a refreshed status line. Returns the sink's
/// send result so the caller can break the loop on a closed socket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CommandFlow {
    continue_goal: bool,
}

impl CommandFlow {
    const DONE: Self = Self {
        continue_goal: false,
    };

    const CONTINUE_GOAL: Self = Self {
        continue_goal: true,
    };
}

async fn handle_command(
    sink: &mut WsSink,
    entry: &AgentEntry,
    agent: &str,
    session: &TuiSession,
    line: &str,
) -> Result<CommandFlow, axum::Error> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if matches!(parts.as_slice(), ["/compact"]) {
        send_json(sink, "notice", "compacting session...").await?;
        let reply = compact_command(entry, agent, session).await;
        send_json(sink, "notice", &reply).await?;
        send_status(sink, entry, agent, session).await?;
        return Ok(CommandFlow::DONE);
    }
    if parts.first().copied() == Some("/goal") {
        let reply = goal_command(entry, agent, session, line).await;
        send_json(sink, "notice", &reply.text).await?;
        send_status(sink, entry, agent, session).await?;
        return Ok(if reply.continue_goal {
            CommandFlow::CONTINUE_GOAL
        } else {
            CommandFlow::DONE
        });
    }
    let reply = match parts.as_slice() {
        ["/help"] => COMMAND_HELP.to_owned(),
        ["/model"] => model_current_text(entry, agent, session).await,
        ["/model", "list"] => model_op_text(entry, agent, session, &json!({"op": "list"})).await,
        ["/model", "clear"] => {
            model_op_text(entry, agent, session, &json!({"op": "clear_session"})).await
        }
        ["/model", "effort"] => "usage: /model effort <low|medium|high|clear>".to_owned(),
        ["/model", "effort", "clear"] => {
            model_op_text(
                entry,
                agent,
                session,
                &json!({"op": "clear_session_effort"}),
            )
            .await
        }
        ["/model", "effort", level] => {
            model_op_text(
                entry,
                agent,
                session,
                &json!({"op": "set_session_effort", "reasoning_effort": level}),
            )
            .await
        }
        ["/model", "set", id] | ["/model", id] => {
            model_op_text(
                entry,
                agent,
                session,
                &json!({"op": "set_session", "model": id}),
            )
            .await
        }
        _ => format!("unknown command: {line}  ·  {COMMAND_HELP}"),
    };
    send_json(sink, "notice", &reply).await?;
    send_status(sink, entry, agent, session).await?;
    Ok(CommandFlow::DONE)
}

/// Compact the persisted TUI session via the same `CompactHook` the kernel
/// runs, so the next turn sees the compacted window.
async fn compact_command(entry: &AgentEntry, agent: &str, tui_session: &TuiSession) -> String {
    let owner = tui_owner(agent);
    let scope = tui_scope(agent, tui_session);
    let session = match entry
        .session_store
        .find_or_create(&owner, None, &scope)
        .await
    {
        Ok(s) => s,
        Err(e) => return format!("compact failed: cannot resolve session ({e})"),
    };
    if session.messages.is_empty() {
        return "nothing to compact (session is empty)".to_owned();
    }
    let before = session.messages.len();
    let before_tokens = estimate_compact_tokens(&session.messages);
    let before_messages = session.messages.clone();
    let session_id = session.id.clone();
    let subject = tui_subject(agent, tui_session);
    match entry
        .compact_hook
        .compact_session(
            Arc::clone(&entry.session_store),
            session_id.clone(),
            subject,
        )
        .await
    {
        Ok(()) => match entry.session_store.load(&session_id).await {
            Ok(Some(after_session)) if after_session.messages == before_messages => {
                format!(
                    "nothing to compact ({before} messages, ~{before_tokens} tokens below threshold)"
                )
            }
            Ok(Some(after_session)) => {
                format!(
                    "session compacted ({before} -> {} messages)",
                    after_session.messages.len()
                )
            }
            Ok(None) => {
                format!("compact failed: session disappeared after compact ({before} messages)")
            }
            Err(e) => format!("session compacted ({before} messages; cannot reload result: {e})"),
        },
        Err(e) => format!("compact failed: {e}"),
    }
}

#[derive(Debug, Eq, PartialEq)]
struct GoalCommandReply {
    text: String,
    continue_goal: bool,
}

impl GoalCommandReply {
    fn notice(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            continue_goal: false,
        }
    }

    fn continue_goal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            continue_goal: true,
        }
    }
}

async fn goal_command(
    entry: &AgentEntry,
    agent: &str,
    tui_session: &TuiSession,
    line: &str,
) -> GoalCommandReply {
    let owner = tui_owner(agent);
    let Some(session) = resolve_session(entry, agent, tui_session).await else {
        return GoalCommandReply::notice("goal failed: cannot resolve session");
    };
    let rest = line.trim_start_matches("/goal").trim();
    if rest.is_empty() {
        return match entry.goal_runtime.get(&session.id).await {
            Ok(Some(goal)) => GoalCommandReply::notice(format_goal(&goal)),
            Ok(None) => GoalCommandReply::notice("No goal set for this thread."),
            Err(err) => GoalCommandReply::notice(format!("goal failed: {err}")),
        };
    }
    match rest {
        "pause" => GoalCommandReply::notice(goal_bool_reply(
            entry.goal_runtime.pause(&session.id).await,
            "Goal paused.",
            "No goal to pause.",
        )),
        "resume" => goal_bool_reply_with_flow(
            entry.goal_runtime.resume(&session.id).await,
            "Goal resumed.",
            "No goal to resume.",
        ),
        "clear" => GoalCommandReply::notice(goal_bool_reply(
            entry.goal_runtime.clear(&session.id).await,
            "Goal cleared.",
            "No goal to clear.",
        )),
        objective => match entry
            .goal_runtime
            .set_objective(&owner, &session.id, objective, None)
            .await
        {
            Ok(goal) => GoalCommandReply::continue_goal(format!("Goal set: {}", goal.objective)),
            Err(err) => GoalCommandReply::notice(format!("goal failed: {err}")),
        },
    }
}

fn goal_bool_reply(
    result: Result<bool, crabgent_store::StoreError>,
    yes: &str,
    no: &str,
) -> String {
    match result {
        Ok(true) => yes.to_owned(),
        Ok(false) => no.to_owned(),
        Err(err) => format!("goal failed: {err}"),
    }
}

fn goal_bool_reply_with_flow(
    result: Result<bool, crabgent_store::StoreError>,
    yes: &str,
    no: &str,
) -> GoalCommandReply {
    match result {
        Ok(true) => GoalCommandReply::continue_goal(yes),
        Ok(false) => GoalCommandReply::notice(no),
        Err(err) => GoalCommandReply::notice(format!("goal failed: {err}")),
    }
}

fn format_goal(goal: &ThreadGoal) -> String {
    let budget = goal
        .token_budget
        .map_or_else(|| "unbounded".to_owned(), |budget| budget.to_string());
    format!(
        "Goal: {objective}\nStatus: {status}\nTokens used: {tokens} / {budget}\nTime: {time}s",
        objective = goal.objective,
        status = goal.status.as_str(),
        tokens = goal.tokens_used,
        time = goal.time_used_seconds,
    )
}

/// Run one `models` tool op against the current session and summarise the
/// result. The refreshed status line carries the authoritative new state;
/// this notice is a short confirmation.
async fn model_op_text(
    entry: &AgentEntry,
    agent: &str,
    session: &TuiSession,
    args: &serde_json::Value,
) -> String {
    let Some(tool) = entry.model_tool.as_ref() else {
        return "this agent has no model tool; /model is unavailable".to_owned();
    };
    let mut ctx = ToolCtx::new(tui_subject(agent, session));
    if let Some(id) = resolve_session_id(entry, agent, session).await {
        ctx = ctx.with_session_id(id);
    }
    match tool.execute(args.clone(), &ctx).await {
        Ok(v) => summarize_model_result(&v),
        Err(e) => format!("/model failed: {e}"),
    }
}

/// Human-facing current-model line, built from the same resolution the
/// status frame uses.
async fn model_current_text(entry: &AgentEntry, agent: &str, session: &TuiSession) -> String {
    let s = resolve_status(entry, agent, session).await;
    let model = s["model"].as_str().unwrap_or("?");
    let model_src = s["model_source"].as_str().unwrap_or("");
    let effort = s["effort"].as_str().unwrap_or("off");
    let effort_src = s["effort_source"].as_str().unwrap_or("");
    format!("model {model} ({model_src})  ·  effort {effort} ({effort_src})")
}

/// Render a `models` op result as a one-liner for the notice frame. Each
/// op shape maps to a short confirmation; the authoritative new state rides
/// the refreshed status line. A JSON fallback covers any uncovered shape.
// A flat chain of early-return shape checks reads clearer than the nested
// `map_or_else` clippy would prefer for each value-or-null branch.
#[allow(clippy::option_if_let_else)]
fn summarize_model_result(v: &serde_json::Value) -> String {
    // op=get: a single `model` object (checked before list's `models` array
    // and before the set/clear `model` field, both of which it would
    // otherwise be mistaken for).
    if let Some(one) = v["model"].as_object() {
        let id = one
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("?");
        return format!("model {id}");
    }
    // op=list: a `models` array.
    if let Some(models) = v["models"].as_array() {
        let ids: Vec<&str> = models
            .iter()
            .filter_map(|m| m["id"].as_str())
            .take(40)
            .collect();
        return format!("{} models: {}", models.len(), ids.join(", "));
    }
    // Session override writes carry a `session_id`; the changed field is
    // `model` (set/clear_session) or `reasoning_effort`
    // (set/clear_session_effort), each a value-or-null.
    if v.get("session_id").is_some() {
        if let Some(model) = v.get("model") {
            return match model.as_str() {
                Some(id) => format!("model override set: {id}"),
                None => "model override cleared".to_owned(),
            };
        }
        if let Some(effort) = v.get("reasoning_effort") {
            return match effort.as_str() {
                Some(level) => format!("effort override set: {level}"),
                None => "effort override cleared".to_owned(),
            };
        }
    }
    // Global override writes (no session_id): `model` or `reasoning_effort`.
    if let Some(model) = v.get("model") {
        return match model.as_str() {
            Some(id) => format!("global model override set: {id}"),
            None => "global model override cleared".to_owned(),
        };
    }
    if let Some(effort) = v.get("reasoning_effort") {
        return match effort.as_str() {
            Some(level) => format!("global effort override set: {level}"),
            None => "global effort override cleared".to_owned(),
        };
    }
    v.to_string()
}

/// Resolve the persisted TUI session id (opaque string) for the `models`
/// tool's `ToolCtx`.
async fn resolve_session_id(
    entry: &AgentEntry,
    agent: &str,
    session: &TuiSession,
) -> Option<String> {
    resolve_session(entry, agent, session)
        .await
        .map(|s| s.id.to_string())
}

async fn resolve_session(entry: &AgentEntry, agent: &str, session: &TuiSession) -> Option<Session> {
    let owner = tui_owner(agent);
    let scope = tui_scope(agent, session);
    entry
        .session_store
        .find_or_create(&owner, None, &scope)
        .await
        .ok()
}

const HISTORY_REPLAY_MAX_ITEMS: usize = 200;
const NOTIFY_USER_RECORD_PREFIX: &str = "[notify_user record]";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct HistoryItem {
    role: &'static str,
    text: String,
}

impl HistoryItem {
    const fn user(text: String) -> Self {
        Self { role: "user", text }
    }

    const fn assistant(text: String) -> Self {
        Self {
            role: "assistant",
            text,
        }
    }

    const fn notice(text: String) -> Self {
        Self {
            role: "notice",
            text,
        }
    }
}

fn session_history_items(messages: &[Message]) -> Vec<HistoryItem> {
    let mut items = messages.iter().filter_map(history_item).collect::<Vec<_>>();
    if items.len() <= HISTORY_REPLAY_MAX_ITEMS {
        return items;
    }

    let omitted = items.len() - HISTORY_REPLAY_MAX_ITEMS;
    let mut kept = Vec::with_capacity(HISTORY_REPLAY_MAX_ITEMS + 1);
    kept.push(HistoryItem::notice(format!(
        "older session history omitted: {omitted} items"
    )));
    kept.extend(items.drain(omitted..));
    kept
}

fn history_item(message: &Message) -> Option<HistoryItem> {
    match message {
        Message::User { content, .. } => user_history_text(content).map(HistoryItem::user),
        Message::Assistant { text, .. } if !text.trim().is_empty() => {
            Some(HistoryItem::assistant(text.trim().to_owned()))
        }
        Message::ChannelOutbound { body, .. } if !body.trim().is_empty() => {
            Some(HistoryItem::assistant(body.trim().to_owned()))
        }
        _ => None,
    }
}

fn user_history_text(content: &[ContentBlock]) -> Option<String> {
    let parts = content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } | ContentBlock::Transcript { text, .. } => {
                let text = text.trim();
                (!text.is_empty() && !text.starts_with(NOTIFY_USER_RECORD_PREFIX))
                    .then(|| text.to_owned())
            }
            ContentBlock::Image(_) => Some("[image]".to_owned()),
            ContentBlock::Audio(_) => Some("[audio]".to_owned()),
            ContentBlock::File(_) => Some("[file]".to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Resolve the effective model + reasoning effort and their override
/// sources for `agent`, mirroring the kernel's resolution so the status
/// line is truthful. Reads the same persisted session + global override
/// stores the kernel run consults.
async fn resolve_status(
    entry: &AgentEntry,
    agent: &str,
    session: &TuiSession,
) -> serde_json::Value {
    let owner = tui_owner(agent);
    let scope = tui_scope(agent, session);
    // Match the exact owner + scope tuple the `SessionPersistHook` resolves
    // from `tui_subject(agent)`, so status/model/goal/history all address
    // the same row as live turns and TUI notify_user persistence.
    let session = entry
        .session_store
        .find_or_create(&owner, None, &scope)
        .await
        .ok();
    let session_model = session.as_ref().and_then(|s| s.model_override.clone());
    let session_effort = session.as_ref().and_then(|s| s.reasoning_effort_override);
    let goal = match session.as_ref() {
        Some(session) => entry.goal_runtime.get(&session.id).await.ok().flatten(),
        None => None,
    };

    let global_model = entry
        .global_model_store
        .get_global_model_override()
        .await
        .ok()
        .flatten();
    let global_effort = entry
        .global_effort_store
        .get_global_reasoning_effort_override()
        .await
        .ok()
        .flatten();

    // Model precedence (pure): session override > global override > config
    // default.
    let (model, model_source) = model_fields(
        &entry.model,
        session_model,
        global_model.map(|g| g.as_str().to_owned()),
    );

    // Capabilities of the EFFECTIVE model: provider label + the model's own
    // default effort, used when nothing overrides it.
    let info = entry.kernel.models().get(&ModelId::new(&model));
    let provider = info.map(|i| i.provider.clone());
    let caps_effort = info.and_then(|i| i.caps.reasoning_effort);

    // Effort precedence (pure): session/global overrides are explicit runtime
    // values. The per-agent `ReasoningEffortHook` fills cfg.reasoning_effort
    // only when no explicit value already exists, so config beats only the
    // model default.
    let cfg_effort = entry
        .reasoning_effort
        .as_deref()
        .and_then(crate::reasoning_hook::ReasoningEffortHook::parse);
    let (effort, effort_source) =
        effort_fields(cfg_effort, session_effort, global_effort, caps_effort);

    serde_json::json!({
        "model": model,
        "model_source": model_source,
        "provider": provider,
        "effort": effort.map(ReasoningEffort::as_str),
        "effort_source": effort_source,
        "goal": goal.as_ref().map(|goal| {
            serde_json::json!({
                "objective": goal.objective,
                "status": goal.status.as_str(),
                "tokens_used": goal.tokens_used,
                "token_budget": goal.token_budget,
                "time_used_seconds": goal.time_used_seconds,
            })
        }),
    })
}

/// Pure model-source precedence, split out from [`resolve_status`] so it is
/// testable without a store. Session override beats global override beats the
/// configured default. Source labels match `ResolvedSource::as_str`.
// A flat precedence chain reads clearer here than a nested `map_or_else`.
#[allow(clippy::option_if_let_else)]
fn model_fields(
    config_model: &str,
    session_model: Option<String>,
    global_model: Option<String>,
) -> (String, &'static str) {
    if let Some(m) = session_model {
        (m, "session-override")
    } else if let Some(g) = global_model {
        (g, "global-override")
    } else {
        (config_model.to_owned(), "config-default")
    }
}

/// Pure effort-source precedence. Session/global values are explicit runtime
/// overrides. The per-agent `ReasoningEffortHook` fills the configured value
/// only when the request has none, so config beats only the model capability
/// default. The `config` label is bridge-specific (the hook layer that
/// `EffortSource` does not model); the rest match `EffortSource::as_str`.
// A flat precedence chain reads clearer here than a nested `map_or`.
#[allow(clippy::option_if_let_else)]
const fn effort_fields(
    config_effort: Option<ReasoningEffort>,
    session_effort: Option<ReasoningEffort>,
    global_effort: Option<ReasoningEffort>,
    caps_effort: Option<ReasoningEffort>,
) -> (Option<ReasoningEffort>, &'static str) {
    if let Some(e) = session_effort {
        (Some(e), "session-override")
    } else if let Some(e) = global_effort {
        (Some(e), "global-override")
    } else if let Some(e) = config_effort {
        (Some(e), "config")
    } else {
        (caps_effort, "model-default")
    }
}

async fn send_status(
    sink: &mut futures::stream::SplitSink<WebSocket, WsMessage>,
    entry: &AgentEntry,
    agent: &str,
    session: &TuiSession,
) -> Result<(), axum::Error> {
    let data = resolve_status(entry, agent, session).await;
    let frame = serde_json::json!({ "kind": "status", "data": data }).to_string();
    sink.send(WsMessage::Text(Utf8Bytes::from(frame))).await
}

async fn send_history(
    sink: &mut futures::stream::SplitSink<WebSocket, WsMessage>,
    entry: &AgentEntry,
    agent: &str,
    tui_session: &TuiSession,
) -> Result<(), axum::Error> {
    let Some(session) = resolve_session(entry, agent, tui_session).await else {
        return send_value(
            sink,
            "history",
            json!({
                "session_id": "",
                "items": [],
            }),
        )
        .await;
    };
    send_value(
        sink,
        "history",
        json!({
            "session_id": session.id.to_string(),
            "items": session_history_items(&session.messages),
        }),
    )
    .await
}

async fn send_event(
    sink: &mut futures::stream::SplitSink<WebSocket, WsMessage>,
    event: &Event,
) -> Result<(), axum::Error> {
    let json = serde_json::to_string(event)
        .unwrap_or_else(|_| r#"{"kind":"turn_error","data":"event serialize failed"}"#.to_owned());
    sink.send(WsMessage::Text(Utf8Bytes::from(json))).await
}

async fn send_tui_delivery(
    sink: &mut futures::stream::SplitSink<WebSocket, WsMessage>,
    delivery: crate::tui_channel::TuiDelivery,
) -> Result<(), axum::Error> {
    let data = serde_json::json!({
        "kind": "tui_channel",
        "from": delivery.from,
        "message": delivery.body,
        "level": "info",
    });
    send_value(sink, "notification", data).await
}

async fn send_activity_delivery(
    sink: &mut futures::stream::SplitSink<WebSocket, WsMessage>,
    delivery: crate::tui_activity::ActivityDelivery,
) -> Result<(), axum::Error> {
    let data = serde_json::json!({
        "agent": delivery.agent,
        "source": delivery.source.as_str(),
        "id": delivery.id,
        "state": delivery.state.as_str(),
        "line": delivery.line,
    });
    send_value(sink, "activity", data).await
}

async fn send_json(
    sink: &mut futures::stream::SplitSink<WebSocket, WsMessage>,
    kind: &str,
    data: &str,
) -> Result<(), axum::Error> {
    let json = serde_json::json!({ "kind": kind, "data": data }).to_string();
    sink.send(WsMessage::Text(Utf8Bytes::from(json))).await
}

async fn send_value(
    sink: &mut futures::stream::SplitSink<WebSocket, WsMessage>,
    kind: &str,
    data: serde_json::Value,
) -> Result<(), axum::Error> {
    let json = serde_json::json!({ "kind": kind, "data": data }).to_string();
    sink.send(WsMessage::Text(Utf8Bytes::from(json))).await
}

fn bearer_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::to_owned)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn constant_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_router_none_when_empty() {
        assert!(build_router(Vec::new()).is_none());
    }

    #[test]
    fn constant_eq_matches() {
        let a = sha256(b"token-abc");
        let b = sha256(b"token-abc");
        let c = sha256(b"token-xyz");
        assert!(constant_eq(&a, &b));
        assert!(!constant_eq(&a, &c));
    }

    #[test]
    fn bearer_parsed_from_header() {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, "Bearer secret123".parse().unwrap());
        assert_eq!(bearer_from(&h).as_deref(), Some("secret123"));
    }

    #[test]
    fn bearer_none_without_prefix() {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, "secret123".parse().unwrap());
        assert_eq!(bearer_from(&h), None);
    }

    #[test]
    fn tui_system_prompt_explains_tui_notify_target() {
        let prompt = tui_system_prompt("base", "local", &TuiSession::main());
        assert!(prompt.contains("notify_user"));
        assert!(prompt.contains(r#"channel="tui""#));
        assert!(prompt.contains(r#"participant_id="local""#));
        assert!(prompt.contains("Do not use `tmux`"));
        assert!(prompt.contains("org.matrix.custom.html"));
        assert!(prompt.contains("Telegram-safe HTML"));
        assert!(prompt.contains("Current TUI session: main"));
    }

    #[test]
    fn tui_subject_carries_direct_tui_scope() {
        let session = TuiSession::main();
        let subject = tui_subject("worker", &session);

        assert_eq!(subject.id(), "tui:worker");
        assert_eq!(subject.attr("agent"), Some("worker"));
        assert_eq!(subject.attr("channel"), Some("tui"));
        assert_eq!(subject.attr("conv"), Some("tui:worker"));
        assert_eq!(subject.attr("channel_kind"), Some("direct"));
        assert_eq!(subject.attr("tui_session"), None);

        let scope = tui_scope("worker", &session);
        assert_eq!(scope.owner.as_ref().map(Owner::as_str), Some("tui:worker"));
        assert_eq!(scope.channel.as_deref(), Some("tui"));
        assert_eq!(scope.conv.as_deref(), Some("tui:worker"));
        assert_eq!(scope.agent.as_deref(), Some("worker"));
        assert_eq!(scope.kind.as_deref(), Some("direct"));
    }

    #[test]
    fn named_tui_session_changes_conv_but_keeps_owner() {
        let session = TuiSession::parse(Some("moss rechnungen")).expect("valid session");
        let subject = tui_subject("local", &session);

        assert_eq!(session.label(), "moss rechnungen");
        assert_eq!(session.topic("local"), "local/moss rechnungen");
        assert_eq!(subject.id(), "tui:local");
        assert_eq!(subject.attr("conv"), Some("tui:local/moss rechnungen"));
        assert_eq!(subject.attr("tui_session"), Some("moss rechnungen"));

        let scope = tui_scope("local", &session);
        assert_eq!(scope.owner.as_ref().map(Owner::as_str), Some("tui:local"));
        assert_eq!(scope.conv.as_deref(), Some("tui:local/moss rechnungen"));
        assert_eq!(scope.agent.as_deref(), Some("local"));
    }

    #[test]
    fn named_tui_session_rejects_path_separator() {
        assert!(TuiSession::parse(Some("a/b")).is_err());
    }

    #[test]
    fn goal_resume_reply_continues_only_when_goal_was_resumed() {
        assert_eq!(
            goal_bool_reply_with_flow(Ok(true), "Goal resumed.", "No goal to resume."),
            GoalCommandReply::continue_goal("Goal resumed.")
        );
        assert_eq!(
            goal_bool_reply_with_flow(Ok(false), "Goal resumed.", "No goal to resume."),
            GoalCommandReply::notice("No goal to resume.")
        );
    }

    #[test]
    fn client_text_parses_cancel_control_frame() {
        assert_eq!(
            parse_client_text(r#"{"op":"cancel"}"#.to_owned()),
            ClientInput::Cancel
        );
    }

    #[test]
    fn client_text_parses_json_prompt_and_legacy_text() {
        assert_eq!(
            parse_client_text(r#"{"prompt":"mach was"}"#.to_owned()),
            ClientInput::Prompt {
                text: "mach was".to_owned(),
                steering: false,
            }
        );
        assert_eq!(
            parse_client_text("mach was".to_owned()),
            ClientInput::Prompt {
                text: "mach was".to_owned(),
                steering: false,
            }
        );
        assert_eq!(
            parse_client_text(r#"{"prompt":"   "}"#.to_owned()),
            ClientInput::Ignore
        );
    }

    #[test]
    fn client_text_parses_steering_prompt_flag() {
        assert_eq!(
            parse_client_text(r#"{"prompt":"lenk das um","steering":true}"#.to_owned()),
            ClientInput::Prompt {
                text: "lenk das um".to_owned(),
                steering: true,
            }
        );
    }

    #[test]
    fn session_history_items_render_chat_and_skip_noise() {
        let messages = vec![
            Message::user(vec![ContentBlock::Text {
                text: "  hallo  ".to_owned(),
            }]),
            Message::user(vec![ContentBlock::Text {
                text: "[notify_user record] already delivered".to_owned(),
            }]),
            Message::Assistant {
                text: "  antwort  ".to_owned(),
                tool_calls: Vec::new(),
            },
            Message::ToolResult {
                call_id: "call-1".to_owned(),
                output: json!({"secret":"skip"}),
                is_error: false,
            },
            Message::ChannelOutbound {
                conv: Owner::new("tui:local"),
                body: "  notify body  ".to_owned(),
                channel: "tui".to_owned(),
                message_id: "m1".to_owned(),
                thread_root: None,
                broadcast: false,
            },
        ];

        let items = session_history_items(&messages);

        assert_eq!(
            items,
            vec![
                HistoryItem::user("hallo".to_owned()),
                HistoryItem::assistant("antwort".to_owned()),
                HistoryItem::assistant("notify body".to_owned()),
            ]
        );
    }

    #[test]
    fn session_history_items_caps_long_replay() {
        let messages = (0..=HISTORY_REPLAY_MAX_ITEMS)
            .map(|idx| Message::Assistant {
                text: format!("antwort {idx}"),
                tool_calls: Vec::new(),
            })
            .collect::<Vec<_>>();

        let items = session_history_items(&messages);

        assert_eq!(items.len(), HISTORY_REPLAY_MAX_ITEMS + 1);
        assert!(matches!(
            &items[0],
            HistoryItem { role: "notice", text } if text.contains("older session history omitted")
        ));
        assert_eq!(items[1], HistoryItem::assistant("antwort 1".to_owned()));
    }

    #[test]
    fn drop_consumed_prompts_keeps_only_registry_pending_tail() {
        let mut prompts =
            VecDeque::from(["first".to_owned(), "second".to_owned(), "third".to_owned()]);
        assert_eq!(
            reconcile_prompt_consumption(
                &mut prompts,
                PromptConsumption::NonFinal { pending_count: 1 },
            ),
            2
        );
        assert_eq!(prompts.into_iter().collect::<Vec<_>>(), vec!["third"]);
    }

    #[test]
    fn final_event_keeps_unconsumed_prompts_for_follow_up() {
        let mut prompts = VecDeque::from(["first".to_owned(), "second".to_owned()]);
        assert_eq!(
            reconcile_prompt_consumption(&mut prompts, PromptConsumption::Final),
            0
        );
        assert_eq!(
            prompts.into_iter().collect::<Vec<_>>(),
            vec!["first", "second"]
        );
    }

    #[test]
    fn pending_turn_preserves_follow_up_messages() {
        let turn =
            PendingTurn::follow_up(vec!["eins".to_owned(), "zwei".to_owned()]).expect("follow-up");

        assert_eq!(turn.follow_up_count(), Some(2));
        let messages = turn.into_messages();
        let texts = messages
            .iter()
            .filter_map(|message| match message {
                Message::User { content, .. } => content.first(),
                _ => None,
            })
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(texts, vec!["eins", "zwei"]);
        assert!(PendingTurn::follow_up(Vec::new()).is_none());
    }

    #[test]
    fn steering_notice_counts_multiple_messages() {
        assert_eq!(steering_notice("steering applied", 1), "steering applied");
        assert_eq!(
            steering_notice("steering applied", 2),
            "steering applied (2)"
        );
    }

    #[test]
    fn model_fields_precedence() {
        assert_eq!(
            model_fields("cfg", Some("sess".to_owned()), Some("glob".to_owned())),
            ("sess".to_owned(), "session-override")
        );
        assert_eq!(
            model_fields("cfg", None, Some("glob".to_owned())),
            ("glob".to_owned(), "global-override")
        );
        assert_eq!(
            model_fields("cfg", None, None),
            ("cfg".to_owned(), "config-default")
        );
    }

    #[test]
    fn effort_fields_precedence() {
        use crabgent_core::ReasoningEffort::{High, Low, Medium};
        // Session and global overrides beat the configured agent default.
        assert_eq!(
            effort_fields(Some(High), Some(Low), Some(Low), Some(Low)),
            (Some(Low), "session-override")
        );
        assert_eq!(
            effort_fields(None, Some(Medium), Some(Low), Some(Low)),
            (Some(Medium), "session-override")
        );
        assert_eq!(
            effort_fields(Some(Medium), None, Some(Low), Some(High)),
            (Some(Low), "global-override")
        );
        assert_eq!(
            effort_fields(Some(Medium), None, None, Some(High)),
            (Some(Medium), "config")
        );
        assert_eq!(
            effort_fields(None, None, None, Some(High)),
            (Some(High), "model-default")
        );
        // Nothing set: effort is unknown (model has no knob).
        assert_eq!(
            effort_fields(None, None, None, None),
            (None, "model-default")
        );
    }
}
