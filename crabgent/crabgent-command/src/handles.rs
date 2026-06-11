//! Shared command handles for adapter wiring.

use std::sync::Arc;

use crabgent_core::PolicyHook;
use crabgent_store::SessionStore;

use crate::agent_name::CommandAgentName;
use crate::error::CommandError;
use crate::registry::CommandRegistry;

/// Shared handles required by `CommandDispatchInbox`.
#[derive(Clone)]
pub struct CommandHandles {
    registry: Arc<CommandRegistry>,
    store: Arc<dyn SessionStore>,
    policy: Arc<dyn PolicyHook>,
    agent_name: CommandAgentName,
}

impl CommandHandles {
    /// Build handles from a non-empty registry.
    pub fn new(
        registry: CommandRegistry,
        store: Arc<dyn SessionStore>,
        policy: Arc<dyn PolicyHook>,
        agent_name: CommandAgentName,
    ) -> Result<Self, CommandError> {
        if registry.is_empty() {
            return Err(CommandError::EmptyRegistry);
        }
        Ok(Self {
            registry: Arc::new(registry),
            store,
            policy,
            agent_name,
        })
    }

    /// Borrow the command registry.
    #[must_use]
    pub fn registry(&self) -> &CommandRegistry {
        &self.registry
    }

    /// Clone the session store handle.
    #[must_use]
    pub fn store(&self) -> Arc<dyn SessionStore> {
        Arc::clone(&self.store)
    }

    /// Clone the policy hook handle.
    #[must_use]
    pub fn policy(&self) -> Arc<dyn PolicyHook> {
        Arc::clone(&self.policy)
    }

    /// Borrow the agent identity used for command session scope.
    #[must_use]
    pub const fn agent_name(&self) -> &CommandAgentName {
        &self.agent_name
    }

    #[cfg(test)]
    pub(crate) fn new_unchecked(
        registry: CommandRegistry,
        store: Arc<dyn SessionStore>,
        policy: Arc<dyn PolicyHook>,
        agent_name: CommandAgentName,
    ) -> Self {
        Self {
            registry: Arc::new(registry),
            store,
            policy,
            agent_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crabgent_core::AllowAllPolicy;
    use crabgent_store::memory::MemorySessionStore;

    use super::*;

    #[test]
    fn handles_rejects_empty_registry() {
        let result = CommandHandles::new(
            CommandRegistry::new(),
            Arc::new(MemorySessionStore::default()),
            Arc::new(AllowAllPolicy),
            CommandAgentName::parse("worker").expect("valid test agent name"),
        );
        assert!(matches!(result, Err(CommandError::EmptyRegistry)));
    }
}
