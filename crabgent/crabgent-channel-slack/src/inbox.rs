//! Slack inbox glue.

use std::collections::HashMap;
use std::sync::Arc;

use crabgent_channel::{Channel, ChannelInbox};
use crabgent_command::{
    CommandAgentName, CommandError, CommandPrefix, CommandRegistry, CommandWiring, SessionStore,
};
use crabgent_core::PolicyHook;
use tokio::task::JoinHandle;

use crate::api::{ConversationType, SlackConversation, SlackHttpClient};
use crate::channel::SlackChannel;
use crate::channel_names::SlackChannelNames;
use crate::connection::SocketModePool;
use crate::dispatch::{ListenerRegistry, SlackEventListener};
use crate::ids::SlackChannelId;

pub const DEFAULT_COMMAND_PREFIX: &str = "/";

/// Conversation kinds listed by [`SlackInbox::pre_warm_channel_names`]: public
/// and private channels for readable names, plus IMs so the kind cache and any
/// later lookups see them (IMs carry no name and are filtered out of the map).
const PRE_WARM_TYPES: &[ConversationType] = &[
    ConversationType::PublicChannel,
    ConversationType::PrivateChannel,
    ConversationType::Im,
];

/// Slack inbox runner and listener registration point.
pub struct SlackInbox {
    pool: Arc<SocketModePool>,
    registry: Arc<ListenerRegistry>,
    inbox: Arc<dyn ChannelInbox>,
    commands: Option<CommandWiring>,
}

impl SlackInbox {
    #[must_use]
    pub fn new(
        pool: Arc<SocketModePool>,
        registry: Arc<ListenerRegistry>,
        inbox: Arc<dyn ChannelInbox>,
    ) -> Self {
        Self {
            pool,
            registry,
            inbox,
            commands: None,
        }
    }

    #[must_use]
    pub fn with_commands(
        self,
        registry: CommandRegistry,
        agent_name: CommandAgentName,
        prefix: Option<CommandPrefix>,
        store: Arc<dyn SessionStore>,
        policy: Arc<dyn PolicyHook>,
    ) -> Self {
        self.try_with_commands(registry, agent_name, prefix, store, policy)
            .unwrap_or_else(|err| panic_invalid_command_config(&err))
    }

    pub fn try_with_commands(
        mut self,
        registry: CommandRegistry,
        agent_name: CommandAgentName,
        prefix: Option<CommandPrefix>,
        store: Arc<dyn SessionStore>,
        policy: Arc<dyn PolicyHook>,
    ) -> Result<Self, CommandError> {
        self.commands = Some(CommandWiring::try_new(
            self.inbox.as_ref(),
            registry,
            agent_name,
            prefix.unwrap_or_else(default_command_prefix),
            store,
            policy,
        )?);
        Ok(self)
    }

    /// Resolve readable channel names and the workspace label once, before
    /// the event loop starts.
    ///
    /// Runs `conversations.list` (cursor-paginated) plus a single `auth.test`
    /// and folds the result into a [`SlackChannelNames`] map keyed by channel
    /// id. The caller feeds the returned value into
    /// [`crate::channel::SlackChannel::with_channel_names`] so `conv_display`
    /// resolves from the map without a dispatch-time round-trip.
    ///
    /// Fail-soft by design: if the bot lacks `channels:read`/`groups:read` or
    /// either call fails, the missing component is logged and left empty (an
    /// empty map, or no workspace), never an error. The `<inbound>` tag then
    /// simply omits the corresponding attribute. The user chose pre-warm over
    /// a lazy/TTL cache, so channels created after this call are absent until
    /// the next process start.
    pub async fn pre_warm_channel_names(&self) -> SlackChannelNames {
        let client = self.pool.http_client();
        let names = pre_warm_names(client.as_ref()).await;
        let workspace = pre_warm_workspace(client.as_ref()).await;
        SlackChannelNames::new(names, workspace)
    }

    /// Spawn the Socket Mode connection pool loop.
    pub fn spawn_run(&self) -> JoinHandle<()> {
        let pool = Arc::clone(&self.pool);
        tokio::spawn(async move {
            pool.run().await;
        })
    }

    pub fn register_listener(&self, listener: Arc<dyn SlackEventListener>) {
        self.registry.register(listener);
    }

    #[must_use]
    pub fn inbox(&self) -> Arc<dyn ChannelInbox> {
        let Some(commands) = &self.commands else {
            return Arc::clone(&self.inbox);
        };
        let channel: Arc<dyn Channel> = Arc::new(SlackChannel::new(self.pool.http_client()));
        commands.wrap_inbox(Arc::clone(&self.inbox), channel)
    }

    #[must_use]
    pub fn command_prefix(&self) -> Option<&CommandPrefix> {
        self.commands.as_ref().map(CommandWiring::prefix)
    }
}

async fn pre_warm_names(client: &SlackHttpClient) -> HashMap<SlackChannelId, String> {
    match client.conversations_list(PRE_WARM_TYPES).await {
        Ok(conversations) => name_map_from_conversations(conversations),
        Err(error) => {
            crabgent_log::warn!(
                %error,
                "Slack conversations.list pre-warm failed; channel names unavailable"
            );
            HashMap::new()
        }
    }
}

async fn pre_warm_workspace(client: &SlackHttpClient) -> Option<String> {
    match client.auth_test().await {
        Ok(auth) => auth.team,
        Err(error) => {
            crabgent_log::warn!(%error, "Slack auth.test pre-warm failed; workspace name unavailable");
            None
        }
    }
}

/// Fold a `conversations.list` result into a channel-id keyed name map.
///
/// IMs and entries without a name carry no readable label and are skipped;
/// invalid channel ids are dropped (the Slack API should not return them, but
/// the typed id constructor is the boundary guard).
fn name_map_from_conversations(
    conversations: Vec<SlackConversation>,
) -> HashMap<SlackChannelId, String> {
    conversations
        .into_iter()
        .filter(|conv| !conv.is_im)
        .filter_map(|conv| {
            let name = conv.name?;
            let id = SlackChannelId::new(conv.id).ok()?;
            Some((id, name))
        })
        .collect()
}

fn default_command_prefix() -> CommandPrefix {
    CommandPrefix::parse(DEFAULT_COMMAND_PREFIX).expect("static Slack command prefix is valid")
}

#[expect(
    clippy::panic,
    reason = "with_commands preserves the existing convenience-builder panic contract; try_with_commands is the fallible alternative"
)]
fn panic_invalid_command_config(err: &CommandError) -> ! {
    panic!(
        "invalid command dispatch configuration: {err}. Command dispatch must wrap mandatory channel gates explicitly"
    )
}

#[cfg(test)]
mod command_tests;
