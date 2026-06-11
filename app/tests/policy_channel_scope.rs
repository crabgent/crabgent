use std::{collections::HashMap, sync::Arc};

use crabgent_channel::channel_send_action;
use crabgent_core::{
    Action,
    memory::MemoryScope,
    owner::Owner,
    policy::{PolicyDecision, PolicyHook},
    subject::Subject,
};
use crabgent_runtime::{RoomVisibility, VisibilityResolver};
use matrix_sdk::ruma::OwnedRoomId;

#[tokio::test]
async fn matrix_channel_send_is_allowed() {
    let policy = crabgent_runtime::build(&crabgent_runtime::MatrixPolicyConfig::default());
    let subject = Subject::new("matrix:%40bob%3Aexample.org");
    let action = channel_send_action(Some("matrix"), &Owner::new("matrix:!room:example.org"));

    assert!(matches!(
        policy.allow(&subject, &action).await,
        PolicyDecision::Allow
    ));
}

#[tokio::test]
async fn non_matrix_channel_send_is_denied() {
    let policy = crabgent_runtime::build(&crabgent_runtime::MatrixPolicyConfig::default());
    let subject = Subject::new("matrix:%40bob%3Aexample.org");
    let action = channel_send_action(Some("telegram"), &Owner::new("telegram:123"));

    assert!(matches!(
        policy.allow(&subject, &action).await,
        PolicyDecision::Deny(_)
    ));
}

fn make_subject(kind: &str, visibility: Option<&str>, conv: &str, shared: Option<&str>) -> Subject {
    let mut subject = Subject::new("matrix:%40bob%3Aexample.org")
        .with_attr("channel", "matrix")
        .with_attr("conv", conv)
        .with_attr("channel_kind", kind);
    if let Some(visibility) = visibility {
        subject = subject.with_attr("channel_visibility", visibility);
    }
    if let Some(shared) = shared {
        subject = subject.with_attr("shared_room_ids", shared);
    }
    subject
}

fn make_visibility_map(
    entries: &[(&str, RoomVisibility)],
) -> Arc<dyn VisibilityResolver + Send + Sync> {
    Arc::new(crabgent_runtime::MapVisibilityResolver::new(
        entries
            .iter()
            .map(|(room, visibility)| (room_id(room), *visibility))
            .collect::<HashMap<_, _>>(),
    ))
}

fn room_id(raw: &str) -> OwnedRoomId {
    let raw = raw.strip_prefix("matrix:").unwrap_or(raw);
    OwnedRoomId::try_from(raw.to_owned()).expect("valid room id")
}

fn read_action(conv: &str) -> Action {
    Action::MemorySearch {
        query: "needle".to_owned(),
        scope: MemoryScope::default().with_conv(conv),
    }
}

fn write_action(conv: &str) -> Action {
    Action::MemoryStore {
        scope: MemoryScope::default().with_conv(conv),
    }
}

fn relation_read_action(conv: &str) -> Action {
    Action::RelationExpand {
        scope: MemoryScope::default().with_conv(conv),
    }
}

fn relation_write_action(conv: &str) -> Action {
    Action::RelationStore {
        scope: MemoryScope::default().with_conv(conv),
    }
}

fn session_search_action(conv: &str) -> Action {
    Action::SessionSearch {
        query: "needle".to_owned(),
        scope: MemoryScope::default().with_conv(conv),
    }
}

fn missing_conv_action() -> Action {
    Action::MemorySearch {
        query: "needle".to_owned(),
        scope: MemoryScope::default(),
    }
}

async fn allow(subject: &Subject, action: &Action, visibility: &[(&str, RoomVisibility)]) -> bool {
    let policy = crabgent_runtime::build_with_channel_scope(
        &crabgent_runtime::MatrixPolicyConfig::default(),
        make_visibility_map(visibility),
    );
    matches!(policy.allow(subject, action).await, PolicyDecision::Allow)
}

#[tokio::test]
async fn dm_can_read_own_dm() {
    let conv = "matrix:!dm:example.org";
    let subject = make_subject("direct", Some("private"), conv, None);

    assert!(allow(&subject, &read_action(conv), &[]).await);
}

#[tokio::test]
async fn dm_can_read_public() {
    let own = "matrix:!dm:example.org";
    let public = "matrix:!public:example.org";
    let subject = make_subject("direct", Some("private"), own, None);

    assert!(
        allow(
            &subject,
            &read_action(public),
            &[(public, RoomVisibility::Public)]
        )
        .await
    );
}

#[tokio::test]
async fn dm_can_read_shared_private() {
    let own = "matrix:!dm:example.org";
    let shared = "matrix:!shared:example.org";
    let subject = make_subject("direct", Some("private"), own, Some(shared));

    assert!(
        allow(
            &subject,
            &read_action(shared),
            &[(shared, RoomVisibility::Private)]
        )
        .await
    );
}

#[tokio::test]
async fn dm_cannot_read_different_private() {
    let own = "matrix:!dm:example.org";
    let other = "matrix:!private:example.org";
    let subject = make_subject("direct", Some("private"), own, None);

    assert!(
        !allow(
            &subject,
            &read_action(other),
            &[(other, RoomVisibility::Private)]
        )
        .await
    );
}

#[tokio::test]
async fn public_can_read_other_public() {
    let own = "matrix:!public-a:example.org";
    let other = "matrix:!public-b:example.org";
    let subject = make_subject("group", Some("public"), own, None);

    assert!(
        allow(
            &subject,
            &read_action(other),
            &[(other, RoomVisibility::Public)]
        )
        .await
    );
}

#[tokio::test]
async fn public_cannot_read_private() {
    let own = "matrix:!public:example.org";
    let private = "matrix:!private:example.org";
    let subject = make_subject("group", Some("public"), own, None);

    assert!(
        !allow(
            &subject,
            &read_action(private),
            &[(private, RoomVisibility::Private)]
        )
        .await
    );
}

#[tokio::test]
async fn public_cannot_read_dm() {
    let own = "matrix:!public:example.org";
    let dm = "matrix:!dm:example.org";
    let subject = make_subject("group", Some("public"), own, None);

    assert!(!allow(&subject, &read_action(dm), &[(dm, RoomVisibility::Private)]).await);
}

#[tokio::test]
async fn private_can_read_same_channel() {
    let conv = "matrix:!private:example.org";
    let subject = make_subject("group", Some("private"), conv, None);

    assert!(allow(&subject, &read_action(conv), &[]).await);
}

#[tokio::test]
async fn private_cannot_read_other_private() {
    let own = "matrix:!private-a:example.org";
    let other = "matrix:!private-b:example.org";
    let subject = make_subject("group", Some("private"), own, None);

    assert!(
        !allow(
            &subject,
            &read_action(other),
            &[(other, RoomVisibility::Private)]
        )
        .await
    );
}

#[tokio::test]
async fn dm_cannot_write_to_public() {
    let own = "matrix:!dm:example.org";
    let public = "matrix:!public:example.org";
    let subject = make_subject("direct", Some("private"), own, None);

    assert!(
        !allow(
            &subject,
            &write_action(public),
            &[(public, RoomVisibility::Public)]
        )
        .await
    );
}

#[tokio::test]
async fn dm_can_write_own_conv() {
    let conv = "matrix:!dm:example.org";
    let subject = make_subject("direct", Some("private"), conv, None);

    assert!(allow(&subject, &write_action(conv), &[]).await);
}

#[tokio::test]
async fn private_can_write_own_conv() {
    let conv = "matrix:!private:example.org";
    let subject = make_subject("group", Some("private"), conv, None);

    assert!(allow(&subject, &write_action(conv), &[]).await);
}

#[tokio::test]
async fn dm_cannot_write_to_shared_private() {
    let own = "matrix:!dm:example.org";
    let shared = "matrix:!shared:example.org";
    let subject = make_subject("direct", Some("private"), own, Some(shared));

    assert!(
        !allow(
            &subject,
            &write_action(shared),
            &[(shared, RoomVisibility::Private)]
        )
        .await
    );
}

#[tokio::test]
async fn relation_expand_uses_same_scope_rules_as_memory_read() {
    let own = "matrix:!dm:example.org";
    let public = "matrix:!public:example.org";
    let subject = make_subject("direct", Some("private"), own, None);

    assert!(
        allow(
            &subject,
            &relation_read_action(public),
            &[(public, RoomVisibility::Public)]
        )
        .await
    );
}

#[tokio::test]
async fn relation_store_uses_same_scope_rules_as_memory_write() {
    let own = "matrix:!dm:example.org";
    let public = "matrix:!public:example.org";
    let subject = make_subject("direct", Some("private"), own, None);

    assert!(
        !allow(
            &subject,
            &relation_write_action(public),
            &[(public, RoomVisibility::Public)]
        )
        .await
    );
    assert!(allow(&subject, &relation_write_action(own), &[]).await);
}

#[tokio::test]
async fn session_search_uses_same_scope_rules_as_memory() {
    let own = "matrix:!dm:example.org";
    let public = "matrix:!public:example.org";
    let subject = make_subject("direct", Some("private"), own, None);

    assert!(
        allow(
            &subject,
            &session_search_action(public),
            &[(public, RoomVisibility::Public)]
        )
        .await
    );
}

#[tokio::test]
async fn public_cannot_write_to_other_public() {
    let own = "matrix:!public-a:example.org";
    let other = "matrix:!public-b:example.org";
    let subject = make_subject("group", Some("public"), own, None);

    assert!(
        !allow(
            &subject,
            &write_action(other),
            &[(other, RoomVisibility::Public)]
        )
        .await
    );
}

#[tokio::test]
async fn missing_scope_conv_denied() {
    let subject = make_subject("direct", Some("private"), "matrix:!dm:example.org", None);

    assert!(!allow(&subject, &missing_conv_action(), &[]).await);
}
