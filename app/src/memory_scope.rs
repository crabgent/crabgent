//! Downstream memory scope normalization.
//!
//! The upstream subject-derived scope is channel precise: owner plus
//! channel, conversation, agent and kind. This deployment normalizes memory
//! to a person scope instead: Matrix, Telegram and TUI identities for the same
//! human should see the same memory, and channel/conversation must not narrow
//! the result.

use std::collections::HashMap;

use crabgent_core::{MemoryScope, Owner, Subject};

use crate::agent_message::ORIGIN_OWNER_ATTR;
use crate::config::UserIdentity;

#[derive(Debug, Clone, Default)]
pub struct MemoryScopeResolver {
    owner_aliases: HashMap<String, String>,
}

impl MemoryScopeResolver {
    #[must_use]
    pub fn new(users: &[UserIdentity]) -> Self {
        let mut owner_aliases = HashMap::new();
        for user in users {
            let Some(canonical_owner) = preferred_owner(&user.owners) else {
                continue;
            };
            for owner in &user.owners {
                owner_aliases.insert(owner.clone(), canonical_owner.clone());
            }
        }
        Self { owner_aliases }
    }

    #[must_use]
    pub fn memory_scope_for_subject(&self, subject: &Subject) -> MemoryScope {
        self.memory_scope_for_owner_and_agent(
            subject_owner_key(subject),
            agent_from_subject(subject).as_deref(),
        )
    }

    #[must_use]
    pub fn memory_scope_for_owner_and_agent(
        &self,
        owner: &str,
        agent: Option<&str>,
    ) -> MemoryScope {
        let mut scope = MemoryScope::global();
        scope.owner = Some(Owner::new(self.canonical_owner(owner)));
        scope.agent = agent.map(str::to_owned);
        scope
    }

    #[must_use]
    pub fn canonical_owner(&self, owner: &str) -> String {
        self.owner_aliases
            .get(owner)
            .cloned()
            .unwrap_or_else(|| owner.to_owned())
    }
}

#[must_use]
pub fn agent_from_subject(subject: &Subject) -> Option<String> {
    subject
        .attr("agent")
        .map(str::to_owned)
        .or_else(|| subject.id().strip_prefix("tui:").map(str::to_owned))
        .or_else(|| subject.id().strip_prefix("agent:").map(str::to_owned))
}

fn subject_owner_key(subject: &Subject) -> &str {
    subject
        .attr(ORIGIN_OWNER_ATTR)
        .unwrap_or_else(|| subject.id())
}

fn preferred_owner(owners: &[String]) -> Option<String> {
    owners
        .iter()
        .find(|owner| !owner.starts_with("tui:"))
        .or_else(|| owners.first())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice_user() -> UserIdentity {
        UserIdentity {
            canonical: "alice".to_owned(),
            owners: vec![
                "matrix:@alice%3Aserver".to_owned(),
                "telegram:42".to_owned(),
                "tui:worker".to_owned(),
            ],
        }
    }

    #[test]
    fn tui_subject_maps_to_canonical_owner_and_agent_scope() {
        let resolver = MemoryScopeResolver::new(&[alice_user()]);
        let subject = Subject::new("tui:worker");
        let scope = resolver.memory_scope_for_subject(&subject);

        assert_eq!(
            scope.owner.as_ref().map(Owner::as_str),
            Some("matrix:@alice%3Aserver")
        );
        assert_eq!(scope.agent.as_deref(), Some("worker"));
        assert!(scope.channel.is_none());
        assert!(scope.conv.is_none());
        assert!(scope.kind.is_none());
    }

    #[test]
    fn origin_owner_overrides_agent_message_subject_for_reads() {
        let resolver = MemoryScopeResolver::new(&[alice_user()]);
        let subject = Subject::new("agent:worker")
            .with_attr("agent", "worker")
            .with_attr(ORIGIN_OWNER_ATTR, "telegram:42");
        let scope = resolver.memory_scope_for_subject(&subject);

        assert_eq!(
            scope.owner.as_ref().map(Owner::as_str),
            Some("matrix:@alice%3Aserver")
        );
        assert_eq!(scope.agent.as_deref(), Some("worker"));
    }

    #[test]
    fn unknown_owner_stays_isolated() {
        let resolver = MemoryScopeResolver::new(&[alice_user()]);
        let scope = resolver.memory_scope_for_owner_and_agent("telegram:99", Some("nova"));

        assert_eq!(scope.owner.as_ref().map(Owner::as_str), Some("telegram:99"));
        assert_eq!(scope.agent.as_deref(), Some("nova"));
    }
}
