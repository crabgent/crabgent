//! Command-side double: [`StubCommand`].
//!
//! Folds the per-file `StubCommand` doubles into one configurable fixture. It
//! parses its registry name, reports a fixed description, returns a configurable
//! policy [`Action`], counts `execute` calls, and on `execute` sends a
//! `stub: <input>` reply through the command context's sink. Adapter dispatch
//! tests that drive a live channel sink opt out of the sink send with
//! [`StubCommand::without_sink_reply`] and assert against [`StubCommand::calls`].

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use crabgent_command::{Command, CommandCtx, CommandError, CommandName, CommandOutput};
use crabgent_core::Action;

/// A configurable [`Command`] test double.
///
/// ```
/// use crabgent_command::Command;
/// use crabgent_test_support::StubCommand;
///
/// let cmd = StubCommand::new("stub");
/// assert_eq!(cmd.name().as_str(), "stub");
/// ```
pub struct StubCommand {
    name: CommandName,
    description: &'static str,
    policy_action: Action,
    reply_via_sink: bool,
    calls: AtomicUsize,
}

impl StubCommand {
    /// Build a stub command whose registry name is `name`.
    ///
    /// Panics if `name` is not a valid lowercase command name; tests pass a
    /// literal, so a malformed name is a test bug, not a runtime condition.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: CommandName::parse(name).expect("valid stub command name"),
            description: "stub",
            policy_action: Action::custom("command.stub"),
            reply_via_sink: true,
            calls: AtomicUsize::new(0),
        }
    }

    /// Override the description returned by [`Command::description`].
    #[must_use]
    pub const fn with_description(mut self, description: &'static str) -> Self {
        self.description = description;
        self
    }

    /// Override the [`Action`] returned by [`Command::policy_action`].
    #[must_use]
    pub fn with_policy_action(mut self, action: Action) -> Self {
        self.policy_action = action;
        self
    }

    /// Stop [`Command::execute`] from sending its reply through the command
    /// context's sink.
    ///
    /// Adapter command-dispatch tests wire a live channel sink (Slack socket,
    /// Matrix client, Telegram API), where a `send` would hit the network. They
    /// only assert dispatch happened, via [`StubCommand::calls`], so the sink
    /// send is skipped while `execute` still returns its [`CommandOutput`].
    #[must_use]
    pub const fn without_sink_reply(mut self) -> Self {
        self.reply_via_sink = false;
        self
    }

    /// Number of times [`Command::execute`] has been invoked.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Command for StubCommand {
    fn name(&self) -> &CommandName {
        &self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    async fn policy_action(&self, _input: &str, _ctx: &CommandCtx) -> Result<Action, CommandError> {
        Ok(self.policy_action.clone())
    }

    async fn execute(&self, input: &str, ctx: &CommandCtx) -> Result<CommandOutput, CommandError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let reply = format!("stub: {input}");
        if self.reply_via_sink {
            ctx.send_reply(reply.clone())
                .await
                .map_err(|err| CommandError::Execution(err.to_string()))?;
        }
        Ok(CommandOutput::new(reply))
    }
}
