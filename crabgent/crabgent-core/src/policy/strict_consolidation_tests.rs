//! Consolidation-specific tests for [`super::StrictPolicy`].

use super::strict::{ActionMatcher, Rule, StrictPolicy};
use super::{PolicyDecision, PolicyHook};
use crate::{Action, MemoryScope, Owner, Subject};

fn subject() -> Subject {
    Subject::new("alice")
}

fn own_scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("alice"))
}

fn memory_consolidate_action() -> Action {
    Action::MemoryConsolidate { scope: own_scope() }
}

#[tokio::test]
async fn action_matcher_memory_consolidate_matches_action() {
    let policy = StrictPolicy::builder()
        .rule(Rule::allow(ActionMatcher::MemoryConsolidate).requires_scope_from_subject())
        .build();

    let decision = policy.allow(&subject(), &memory_consolidate_action()).await;

    assert!(matches!(decision, PolicyDecision::Allow));
}

#[tokio::test]
async fn strict_policy_builder_allow_memory_consolidate_grants() {
    let policy = StrictPolicy::builder().allow_memory_consolidate().build();

    let decision = policy.allow(&subject(), &memory_consolidate_action()).await;

    assert!(matches!(decision, PolicyDecision::Allow));
}

#[tokio::test]
async fn strict_policy_denies_memory_consolidate_without_grant() {
    let policy = StrictPolicy::builder().allow_memory_search().build();

    let decision = policy.allow(&subject(), &memory_consolidate_action()).await;

    assert!(matches!(decision, PolicyDecision::Deny(_)));
}

#[tokio::test]
async fn allow_memory_any_does_not_grant_consolidate() {
    let policy = StrictPolicy::builder().allow_memory_any().build();

    let decision = policy.allow(&subject(), &memory_consolidate_action()).await;

    assert!(matches!(decision, PolicyDecision::Deny(_)));
}
