use crabgent_core::{
    Action, MemoryId, MemoryScope, Owner, PolicyDecision, PolicyHook, StrictPolicy, Subject,
};

fn owner_scope(owner: &str) -> MemoryScope {
    MemoryScope::for_owner(Owner::new(owner))
}

#[tokio::test]
async fn memory_actions_require_subject_owner_scope() {
    let policy = StrictPolicy::builder().allow_memory_any().build();
    let subject = Subject::new("alice");
    let allowed = Action::MemoryGet {
        id: MemoryId::new(),
        scope: owner_scope("alice"),
    };
    let cross_owner = Action::MemoryGet {
        id: MemoryId::new(),
        scope: owner_scope("bob"),
    };
    let global = Action::MemoryGet {
        id: MemoryId::new(),
        scope: MemoryScope::global(),
    };

    assert!(matches!(
        policy.allow(&subject, &allowed).await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy.allow(&subject, &cross_owner).await,
        PolicyDecision::Deny(_)
    ));
    assert!(matches!(
        policy.allow(&subject, &global).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn memory_lifecycle_actions_require_subject_owner_scope() {
    let policy = StrictPolicy::builder().allow_memory_any().build();
    let subject = Subject::new("alice");
    let id = MemoryId::new();
    let allowed = Action::MemoryArchive {
        id: id.clone(),
        scope: owner_scope("alice"),
    };
    let cross_owner = Action::MemoryExtendExpiry {
        id,
        scope: owner_scope("bob"),
    };

    assert!(matches!(
        policy.allow(&subject, &allowed).await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy.allow(&subject, &cross_owner).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn session_search_requires_subject_owner_scope() {
    let policy = StrictPolicy::builder().allow_session_search().build();
    let subject = Subject::new("alice");
    let allowed = Action::SessionSearch {
        query: "needle".into(),
        scope: owner_scope("alice"),
    };
    let cross_owner = Action::SessionSearch {
        query: "needle".into(),
        scope: owner_scope("bob"),
    };

    assert!(matches!(
        policy.allow(&subject, &allowed).await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy.allow(&subject, &cross_owner).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn subject_channel_attrs_narrow_allowed_memory_scope() {
    let policy = StrictPolicy::builder().allow_memory_store().build();
    let subject = Subject::new("alice")
        .with_attr("channel", "slack")
        .with_attr("conv", "slack:T1/D1");
    let allowed = Action::MemoryStore {
        scope: owner_scope("alice")
            .with_channel("slack")
            .with_conv("slack:T1/D1"),
    };
    let too_broad = Action::MemoryStore {
        scope: owner_scope("alice"),
    };

    assert!(matches!(
        policy.allow(&subject, &allowed).await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        policy.allow(&subject, &too_broad).await,
        PolicyDecision::Deny(_)
    ));
}
