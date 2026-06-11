//! Command-dispatch wiring for the Telegram poller.

use std::sync::Arc;

use crabgent_channel::{Channel, ChannelInbox};
use crabgent_command::{
    CommandAgentName, CommandError, CommandPrefix, CommandRegistry, CommandWiring, SessionStore,
};
use crabgent_core::PolicyHook;

use super::TelegramPoller;

pub const DEFAULT_COMMAND_PREFIX: &str = "/";

#[derive(Clone)]
pub(super) struct CommandConfig {
    wiring: CommandWiring,
}

impl TelegramPoller {
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
        self.commands = Some(CommandConfig {
            wiring: CommandWiring::try_new(
                self.inbox.as_ref(),
                registry,
                agent_name,
                prefix.unwrap_or_else(default_command_prefix),
                store,
                policy,
            )?,
        });
        Ok(self)
    }

    #[must_use]
    pub fn command_prefix(&self) -> Option<&CommandPrefix> {
        self.commands
            .as_ref()
            .map(|commands| commands.wiring.prefix())
    }

    pub(super) fn dispatch_inbox(&self) -> Arc<dyn ChannelInbox> {
        let Some(commands) = &self.commands else {
            return Arc::clone(&self.inbox);
        };
        let channel: Arc<dyn Channel> = self.channel.clone();
        commands.wiring.wrap_inbox(Arc::clone(&self.inbox), channel)
    }
}

fn default_command_prefix() -> CommandPrefix {
    CommandPrefix::parse(DEFAULT_COMMAND_PREFIX).expect("static Telegram command prefix is valid")
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
