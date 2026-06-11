//! Memory-relation tests for [`super::StrictPolicy`].
//!
//! Relations are a distinct policy family: `MemoryAny` must not absorb them,
//! and each op gates on the edge's own scope being within the subject.

use super::strict::{ActionMatcher, Rule, StrictPolicy};
use super::{PolicyDecision, PolicyHook};
use crate::{Action, MemoryScope, Owner, Subject};

fn subject() -> Subject {
    Subject::new("alice")
}

fn own_scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("alice"))
}

fn other_scope() -> MemoryScope {
    MemoryScope::for_owner(Owner::new("bob"))
}

fn store_action(scope: MemoryScope) -> Action {
    Action::RelationStore { scope }
}

fn delete_action() -> Action {
    Action::RelationDelete { scope: own_scope() }
}

fn expand_action() -> Action {
    Action::RelationExpand { scope: own_scope() }
}

#[tokio::test]
async fn allow_relation_store_grants_own_scope() {
    let policy = StrictPolicy::builder().allow_relation_store().build();

    let decision = policy.allow(&subject(), &store_action(own_scope())).await;

    assert!(matches!(decision, PolicyDecision::Allow));
}

#[tokio::test]
async fn allow_relation_store_denies_other_owner_scope() {
    let policy = StrictPolicy::builder().allow_relation_store().build();

    let decision = policy.allow(&subject(), &store_action(other_scope())).await;

    assert!(matches!(decision, PolicyDecision::Deny(_)));
}

#[tokio::test]
async fn allow_relation_delete_grants() {
    let policy = StrictPolicy::builder().allow_relation_delete().build();

    let decision = policy.allow(&subject(), &delete_action()).await;

    assert!(matches!(decision, PolicyDecision::Allow));
}

#[tokio::test]
async fn allow_relation_expand_grants() {
    let policy = StrictPolicy::builder().allow_relation_expand().build();

    let decision = policy.allow(&subject(), &expand_action()).await;

    assert!(matches!(decision, PolicyDecision::Allow));
}

#[tokio::test]
async fn allow_relation_any_grants_all_three() {
    let policy = StrictPolicy::builder().allow_relation_any().build();

    for action in [store_action(own_scope()), delete_action(), expand_action()] {
        let decision = policy.allow(&subject(), &action).await;
        assert!(matches!(decision, PolicyDecision::Allow), "{action:?}");
    }
}

#[tokio::test]
async fn memory_any_does_not_grant_relations() {
    let policy = StrictPolicy::builder().allow_memory_any().build();

    for action in [store_action(own_scope()), delete_action(), expand_action()] {
        let decision = policy.allow(&subject(), &action).await;
        assert!(matches!(decision, PolicyDecision::Deny(_)), "{action:?}");
    }
}

#[tokio::test]
async fn relation_any_does_not_grant_plain_memory() {
    let policy = StrictPolicy::builder().allow_relation_any().build();

    let decision = policy
        .allow(&subject(), &Action::MemoryStore { scope: own_scope() })
        .await;

    assert!(matches!(decision, PolicyDecision::Deny(_)));
}

#[tokio::test]
async fn relation_store_denied_without_grant() {
    let policy = StrictPolicy::builder().allow_memory_search().build();

    let decision = policy.allow(&subject(), &store_action(own_scope())).await;

    assert!(matches!(decision, PolicyDecision::Deny(_)));
}

#[tokio::test]
async fn raw_matcher_relation_store_matches() {
    let policy = StrictPolicy::builder()
        .rule(Rule::allow(ActionMatcher::RelationStore).requires_scope_from_subject())
        .build();

    let decision = policy.allow(&subject(), &store_action(own_scope())).await;

    assert!(matches!(decision, PolicyDecision::Allow));
}
