use crabgent_channel::{ChannelKind, ChannelSubjectExt};
use crabgent_core::{
    Action,
    memory::MemoryScope,
    owner::Owner,
    policy::{PolicyDecision, PolicyHook},
    subject::Subject,
};

fn subject_for(kind: ChannelKind) -> Subject {
    Subject::new("matrix:%40bob%3Aexample.org").with_channel(
        "matrix",
        &Owner::new("matrix:!room:example.org"),
        kind,
    )
}

#[tokio::test]
async fn restricted_tool_is_allowed_in_direct_room() {
    let policy = crabgent_runtime::build(&crabgent_runtime::MatrixPolicyConfig::default());
    let subject = subject_for(ChannelKind::Direct);

    assert!(matches!(
        policy.allow(&subject, &Action::tool("bash")).await,
        PolicyDecision::Allow
    ));
}

#[tokio::test]
async fn restricted_tool_is_denied_in_group_room() {
    let policy = crabgent_runtime::build(&crabgent_runtime::MatrixPolicyConfig::default());
    let subject = subject_for(ChannelKind::Group);

    assert!(matches!(
        policy.allow(&subject, &Action::tool("bash")).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn safe_tool_is_allowed_in_group_room() {
    let policy = crabgent_runtime::build(&crabgent_runtime::MatrixPolicyConfig::default());
    let subject = subject_for(ChannelKind::Group);

    assert!(matches!(
        policy.allow(&subject, &Action::tool("read_file")).await,
        PolicyDecision::Allow
    ));
}

#[tokio::test]
async fn memory_relation_ops_are_allowed_for_run_scope() {
    let policy = crabgent_runtime::build(&crabgent_runtime::MatrixPolicyConfig::default());
    let subject = subject_for(ChannelKind::Direct);
    let scope = MemoryScope::from_subject(&subject);

    for action in [
        Action::RelationStore {
            scope: scope.clone(),
        },
        Action::RelationDelete {
            scope: scope.clone(),
        },
        Action::RelationExpand { scope },
    ] {
        assert!(
            matches!(policy.allow(&subject, &action).await, PolicyDecision::Allow),
            "{action:?}"
        );
    }
}
