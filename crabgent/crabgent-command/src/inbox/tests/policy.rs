//! Policy-gate and subject-stamping tests for `CommandDispatchInbox`.

use std::sync::{Arc, Mutex};

use crabgent_channel::{ChannelInbox, MessageRef, attr_keys};
use crabgent_core::Owner;

use super::helpers::{OrderedPolicy, SubjectCapturePolicy, event, inbox};

#[tokio::test]
async fn dispatch_subject_stamps_channel_and_inbound_message_attrs() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let subjects = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(SubjectCapturePolicy {
        subjects: Arc::clone(&subjects),
    });
    let (inbox, _inner, _sink, _store) = inbox(policy, order);
    let mut ev = event("/stub arg");
    ev.message =
        MessageRef::thread_reply_broadcast("test", Owner::new("test:conv"), "in2", "root1", true);

    inbox.receive(ev).await.expect("dispatch ok");

    let subjects = subjects.lock().expect("test mutex must not be poisoned");
    let (_, subject) = subjects.first().expect("command should capture subject");
    assert_eq!(subject.attr(attr_keys::CHANNEL), Some("test"));
    assert_eq!(subject.attr(attr_keys::CONV), Some("test:conv"));
    assert_eq!(subject.attr("agent"), Some("worker"));
    assert_eq!(subject.attr(attr_keys::PARTICIPANT_ROLE), Some("human"));
    assert_eq!(subject.attr(attr_keys::PARTICIPANT_ID), Some("u1"));
    assert_eq!(subject.attr(attr_keys::INBOUND_MSG_ID), Some("in2"));
    assert_eq!(
        subject.attr(attr_keys::INBOUND_MSG_THREAD_ROOT),
        Some("root1"),
    );
    assert_eq!(subject.attr(attr_keys::INBOUND_MSG_BROADCAST), Some("true"));
    assert_eq!(subject.attr(attr_keys::CHANNEL_KIND), Some("group"));
}

#[tokio::test]
async fn dispatch_policy_gates_see_agent_attr() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let subjects = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(SubjectCapturePolicy {
        subjects: Arc::clone(&subjects),
    });
    let (inbox, _inner, _sink, _store) = inbox(policy, order);

    inbox
        .receive(event("/stub arg"))
        .await
        .expect("dispatch ok");

    let subjects = subjects.lock().expect("test mutex must not be poisoned");
    assert_eq!(subjects.len(), 2);
    assert_eq!(subjects[0].0, "stub");
    assert_eq!(subjects[1].0, "stub.inner");
    assert!(
        subjects
            .iter()
            .all(|(_, subject)| subject.attr("agent") == Some("worker")),
        "both command policy gates must see the command agent identity"
    );
}

#[tokio::test]
async fn dispatch_policy_deny_returns_safe_error_reply() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order: Arc::clone(&order),
        deny_outer: true,
    });
    let (inbox, inner, sink, _store) = inbox(policy, order);

    inbox
        .receive(event("/stub arg"))
        .await
        .expect("deny handled");

    assert_eq!(
        *inner.calls.lock().expect("test mutex must not be poisoned"),
        0
    );
    assert_eq!(
        sink.replies
            .lock()
            .expect("test mutex must not be poisoned")
            .as_slice(),
        &["blocked by test policy".to_owned()]
    );
}
