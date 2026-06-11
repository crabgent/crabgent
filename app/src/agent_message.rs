//! Channel-independent agent-to-agent messaging.
//!
//! Every agent's kernel is registered in a shared [`AgentDirectory`].
//! The [`AgentMessageTool`] lets one agent invoke another agent's
//! kernel directly without touching matrix/telegram/slack. A
//! matrix-only agent can talk to a telegram-only agent because the
//! routing lives in the same host process, above the channel layer.
//!
//! Semantics:
//! - The caller's `agent_message(to, body)` tool call **blocks** until
//!   the target's kernel produces a final assistant text; that text is
//!   returned as the tool result. The target's reply does NOT reach
//!   the caller's chat — the caller is responsible for relaying it via
//!   `channel_send` if desired.
//! - The target receives the body as a synthesised user turn on a
//!   synthetic `agent:<peer>` subject. The subject carries no channel
//!   attrs, so the target's own `channel_send` will fail with
//!   `InvalidOwnerFormat`; tools that produce a final text without
//!   side-effects (memory, calendar, models, ...) work fine.
//! - Loop guard: the subject carries a `agent_message_depth` attr that
//!   each hop increments; the tool refuses to send when the cap
//!   ([`DEFAULT_MAX_DEPTH`]) is reached.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::model::ModelTarget;
use crabgent_core::run::RunRequest;
use crabgent_core::subject::Subject;
use crabgent_core::tool::{Tool, ToolCtx, parse_args};
use crabgent_core::{ContentBlock, Kernel, Message, RunId};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

const TOOL_NAME: &str = "agent_message";
const DEFAULT_MAX_DEPTH: usize = 4;
const DEPTH_ATTR: &str = "agent_message_depth";
const FROM_ATTR: &str = "agent_message_from";

/// Subject attr carrying the owner string of the human who originated
/// this agent-to-agent chain. Set on the target subject so the target's
/// `MemoryRecallHook` recalls that human's memory instead of the
/// `agent:<peer>` pseudo-owner (read-only impersonation). Carried forward
/// unchanged across multi-hop relays.
pub const ORIGIN_OWNER_ATTR: &str = "agent_message_origin_owner";

/// Per-agent record shared between every other agent's
/// [`AgentMessageTool`] for lookups.
pub struct DirectoryEntry {
    pub name: String,
    pub kernel: Arc<Kernel>,
    pub model: ModelTarget,
    pub system_prompt: Option<String>,
    pub max_turns: Option<u32>,
    pub fallbacks: Vec<ModelTarget>,
}

/// Process-wide registry of every agent's [`DirectoryEntry`].
///
/// Built empty at runtime startup and populated by `runtime::build_handles`
/// after each agent's kernel is constructed. The shared [`Arc`] is handed
/// to every agent's [`AgentMessageTool`] so lookups always see the latest
/// registrations.
#[derive(Default)]
pub struct AgentDirectory {
    inner: Mutex<HashMap<String, Arc<DirectoryEntry>>>,
}

impl AgentDirectory {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn register(&self, entry: Arc<DirectoryEntry>) {
        let name = entry.name.clone();
        self.inner.lock().await.insert(name, entry);
    }

    pub async fn get(&self, name: &str) -> Option<Arc<DirectoryEntry>> {
        self.inner.lock().await.get(name).cloned()
    }

    /// Sorted list of every registered agent name minus `exclude`.
    /// Used at startup to populate the system-prompt peer hint.
    #[allow(dead_code)]
    pub async fn peer_names(&self, exclude: &str) -> Vec<String> {
        let mut names: Vec<String> = {
            let guard = self.inner.lock().await;
            guard
                .keys()
                .filter(|n| n.as_str() != exclude)
                .cloned()
                .collect()
        };
        names.sort();
        names
    }
}

#[derive(Debug, Deserialize)]
struct Args {
    to: String,
    body: String,
}

/// System prompt for an agent-to-agent peer-call. Overrides the target's
/// default channel-oriented prompt so the peer knows it is in PEER-CALL
/// mode. When the caller's subject carries a channel plus participant id,
/// the target is told it MAY reach that user directly via `notify_user`,
/// which opens or reuses the target's own DM with the user and therefore
/// needs no shared conversation. The target's own static prompt (persona,
/// language, domain knowledge) is appended below so the persona survives.
fn compose_peer_system_prompt(
    caller: &str,
    base: Option<&str>,
    inherited_channel: Option<&str>,
    inherited_participant: Option<&str>,
) -> String {
    let context_line = match (inherited_channel, inherited_participant) {
        (Some(channel), Some(participant)) => format!(
            "\n- The caller is relaying for user `{participant}` on channel \
             `{channel}`. If the caller asked you to message that user \
             directly, call `notify_user` with channel=`{channel}`, \
             participant_id=`{participant}` and your message body. \
             `notify_user` opens or reuses YOUR OWN direct conversation \
             with the user, so it lands even though you share no \
             conversation with the caller. Do NOT use `channel_send` for \
             this: it needs a conv your bot user already belongs to. Any \
             auto-recalled memory shown to you this turn is that user's."
        ),
        _ => "\n- You have NO inherited user context here. Do NOT call \
              channel_send, channel_react, channel_edit, channel_delete, \
              channel_upload or notify_user — your only output is the plain \
              final text returned to the caller."
            .to_owned(),
    };
    let header = format!(
        "PEER-CALL MODE: you are being invoked by peer agent `{caller}` via the \
         agent_message tool inside the same host process. Rules:\n\
         - Your final assistant text is what the caller receives via the tool \
           result; keep it short and direct.\n\
         - DO NOT call agent_message recursively unless the caller explicitly \
           asked you to fan out further.\n\
         - You MAY call read-only tools (memory, session_search, calendar, \
           models, consolidate_memory) when the question genuinely needs \
           them.\n\
         - The first user message is the caller's prompt, prefixed with \
           `[from agent {caller}]`.{context_line}"
    );
    match base {
        Some(b) if !b.is_empty() => format!(
            "{header}\n\nORIGINAL AGENT PROMPT (persona + domain rules, channel \
             defaults are overridden above):\n{b}"
        ),
        _ => header,
    }
}

/// The human owner that originated this (possibly multi-hop) agent
/// message. A deeper hop carries [`ORIGIN_OWNER_ATTR`] forward unchanged;
/// the first hop derives it from the caller's own subject id, which for a
/// channel run is the human's owner string. An agent-pseudo subject
/// (`agent:<name>`) with no inherited origin yields `None`, so a chain not
/// rooted in a human never impersonates anyone.
fn derive_origin(caller: &Subject) -> Option<String> {
    if let Some(origin) = caller.attr(ORIGIN_OWNER_ATTR) {
        return Some(origin.to_owned());
    }
    let id = caller.id();
    (!id.starts_with("agent:")).then(|| id.to_owned())
}

fn tui_participant_from_subject(subject: &Subject) -> Option<String> {
    subject
        .id()
        .strip_prefix("tui:")
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

pub struct AgentMessageTool {
    self_name: String,
    directory: Arc<AgentDirectory>,
    max_depth: usize,
}

impl AgentMessageTool {
    #[must_use]
    pub fn new(self_name: impl Into<String>, directory: Arc<AgentDirectory>) -> Self {
        Self {
            self_name: self_name.into(),
            directory,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

#[async_trait]
impl Tool for AgentMessageTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        "Send a prompt directly to a peer agent running in the same \
         host process. Channel-independent: a matrix-only agent \
         can address a telegram-only agent. The peer's kernel runs the \
         body as if it were a user turn and the final assistant text \
         comes back as the tool result. The peer's reply does NOT reach \
         your own chat: forward it with channel_send if you want your \
         user to see it. Args: `to` (peer name) and `body` (the prompt). \
         Depth is capped to prevent infinite loops; you cannot message \
         yourself."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["to", "body"],
            "properties": {
                "to": {
                    "type": "string",
                    "description": "Name of the peer agent as configured in this host."
                },
                "body": {
                    "type": "string",
                    "description": "Prompt body passed to the peer kernel as a user message."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let args: Args = parse_args(args)?;
        if args.to == self.self_name {
            return Err(ToolError::InvalidArgs(
                "agent_message cannot target yourself".into(),
            ));
        }
        let depth = ctx
            .subject
            .attr(DEPTH_ATTR)
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        if depth >= self.max_depth {
            return Err(ToolError::Execution(format!(
                "agent_message depth cap {} reached; refusing further hops",
                self.max_depth
            )));
        }
        let entry = self
            .directory
            .get(&args.to)
            .await
            .ok_or_else(|| ToolError::NotFound(format!("agent {}", args.to)))?;
        let body = format!("[from agent {}] {}", self.self_name, args.body);
        let mut subject = Subject::new(format!("agent:{}", entry.name))
            .with_attr("agent", entry.name.as_str())
            .with_attr(FROM_ATTR, self.self_name.as_str())
            .with_attr(DEPTH_ATTR, (depth + 1).to_string());
        // Inherit caller's channel context so the target CAN channel_send back
        // to the originating chat if it judges that useful (target's own bot
        // user must be present in the conv for the send to land; otherwise
        // matrix-side permission denies and the tool result surfaces the
        // failure). channel_kind is inherited too so policy checks that gate
        // on direct-vs-group keep working.
        let tui_participant = tui_participant_from_subject(&ctx.subject);
        let inherited_channel = ctx
            .subject
            .attr("channel")
            .map(str::to_owned)
            .or_else(|| tui_participant.as_ref().map(|_| "tui".to_owned()));
        let inherited_conv = ctx
            .subject
            .attr("conv")
            .map(str::to_owned)
            .or_else(|| tui_participant.as_ref().map(|p| format!("tui:{p}")));
        let inherited_kind = ctx
            .subject
            .attr("channel_kind")
            .map(str::to_owned)
            .or_else(|| tui_participant.as_ref().map(|_| "direct".to_owned()));
        let inherited_participant = ctx
            .subject
            .attr("participant_id")
            .map(str::to_owned)
            .or(tui_participant);
        if let Some(channel) = inherited_channel.as_deref() {
            subject = subject.with_attr("channel", channel);
        }
        if let Some(conv) = inherited_conv.as_deref() {
            subject = subject.with_attr("conv", conv);
        }
        if let Some(kind) = inherited_kind.as_deref() {
            subject = subject.with_attr("channel_kind", kind);
        }
        if let Some(participant) = inherited_participant.as_deref() {
            subject = subject.with_attr("participant_id", participant);
        }
        // Anchor the target's memory recall to the originating human so a
        // relayed peer-call surfaces that human's own memories. Read-only:
        // writes still land under `agent:<peer>`
        // because the subject id is unchanged.
        if let Some(origin) = derive_origin(&ctx.subject) {
            subject = subject.with_attr(ORIGIN_OWNER_ATTR, origin.as_str());
        }
        let system_prompt = compose_peer_system_prompt(
            &self.self_name,
            entry.system_prompt.as_deref(),
            inherited_channel.as_deref(),
            inherited_participant.as_deref(),
        );
        let request = RunRequest {
            run_id: RunId::new(),
            subject,
            model: entry.model.clone(),
            explicit_model: None,
            session_model_override: None,
            fallbacks: entry.fallbacks.clone(),
            messages: vec![Message::User {
                content: vec![ContentBlock::Text { text: body }],
                timestamp: None,
            }],
            system_prompt: Some(system_prompt),
            max_turns: entry.max_turns,
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
            web_search: crabgent_core::WebSearchConfig::default(),
            cancel_reason: None,
            pause: None,
        };
        let response = entry
            .kernel
            .run(request, None)
            .await
            .map_err(|err| ToolError::Execution(format!("peer kernel run failed: {err}")))?;
        Ok(json!({
            "from": entry.name,
            "response": response,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_origin_first_hop_uses_human_subject_id() {
        let caller = Subject::new("telegram:42");
        assert_eq!(derive_origin(&caller).as_deref(), Some("telegram:42"));
    }

    #[test]
    fn derive_origin_agent_subject_without_inherited_origin_is_none() {
        let caller = Subject::new("agent:worker");
        assert_eq!(derive_origin(&caller), None);
    }

    #[test]
    fn derive_origin_carries_inherited_origin_across_hops() {
        // agent-a -> agent-b -> agent-c: the second hop's caller is the
        // agent-pseudo `agent:assistant`, but it carries the human forward.
        let caller = Subject::new("agent:assistant").with_attr(ORIGIN_OWNER_ATTR, "telegram:42");
        assert_eq!(derive_origin(&caller).as_deref(), Some("telegram:42"));
    }

    #[test]
    fn tui_subject_derives_tui_participant() {
        let caller = Subject::new("tui:local");
        assert_eq!(
            tui_participant_from_subject(&caller).as_deref(),
            Some("local")
        );
        assert_eq!(
            tui_participant_from_subject(&Subject::new("agent:local")),
            None
        );
    }
}
