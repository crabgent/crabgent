//! Command registry.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::command::Command;
use crate::error::CommandError;
use crate::name::CommandName;

/// Immutable command registry used by adapter inboxes.
#[derive(Clone, Default)]
pub struct CommandRegistry {
    commands: BTreeMap<CommandName, Arc<dyn Command>>,
}

impl CommandRegistry {
    /// Build an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a command by its name.
    pub fn register(&mut self, command: Arc<dyn Command>) -> Result<(), CommandError> {
        let name = command.name().clone();
        if self.commands.contains_key(&name) {
            return Err(CommandError::DuplicateRegistration(name.to_string()));
        }
        self.commands.insert(name, command);
        Ok(())
    }

    /// Return a new registry with `command` registered.
    pub fn with_command(mut self, command: Arc<dyn Command>) -> Result<Self, CommandError> {
        self.register(command)?;
        Ok(self)
    }

    /// Look up a command by name.
    #[must_use]
    pub fn get(&self, name: &CommandName) -> Option<Arc<dyn Command>> {
        self.commands.get(name).cloned()
    }

    /// Number of registered commands.
    #[must_use]
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// `true` when no commands are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Iterate over registered command names in registration-name order.
    /// Used by `CommandDispatchInbox` to surface the available commands
    /// when a user types an unknown prefix-name.
    pub fn names(&self) -> impl Iterator<Item = &CommandName> {
        self.commands.keys()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use crabgent_core::Action;

    use super::*;
    use crate::command::{CommandCtx, CommandOutput};

    struct StubCommand {
        name: CommandName,
    }

    impl StubCommand {
        fn new(name: &str) -> Self {
            Self {
                name: CommandName::parse(name).expect("valid test command name"),
            }
        }
    }

    #[async_trait]
    impl Command for StubCommand {
        fn name(&self) -> &CommandName {
            &self.name
        }

        fn description(&self) -> &'static str {
            "stub command"
        }

        async fn policy_action(
            &self,
            _input: &str,
            _ctx: &CommandCtx,
        ) -> Result<Action, CommandError> {
            Ok(Action::custom("stub.inner"))
        }

        async fn execute(
            &self,
            _input: &str,
            _ctx: &CommandCtx,
        ) -> Result<CommandOutput, CommandError> {
            Ok(CommandOutput::new("ok"))
        }
    }

    #[test]
    fn register_lookup_by_name() {
        let mut registry = CommandRegistry::new();
        registry
            .register(Arc::new(StubCommand::new("compact")))
            .expect("register command");
        let name = CommandName::parse("compact").expect("valid command name");
        assert!(registry.get(&name).is_some());
    }

    #[test]
    fn duplicate_registration_errors() {
        let mut registry = CommandRegistry::new();
        registry
            .register(Arc::new(StubCommand::new("compact")))
            .expect("register command");
        let err = registry
            .register(Arc::new(StubCommand::new("compact")))
            .expect_err("duplicate registration must fail");
        assert!(matches!(err, CommandError::DuplicateRegistration(_)));
    }

    #[test]
    fn with_command_updates_len_and_empty_state() {
        let registry = CommandRegistry::new();
        assert!(registry.is_empty());
        let registry = registry
            .with_command(Arc::new(StubCommand::new("models")))
            .expect("register command");
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
    }

    #[test]
    fn lookup_missing_returns_none() {
        let registry = CommandRegistry::new();
        let name = CommandName::parse("missing").expect("valid command name");
        assert!(registry.get(&name).is_none());
    }
}
