//! Dispatch and routing tests for `CommandDispatchInbox`.

use std::sync::{Arc, Mutex};

use crabgent_channel::ChannelInbox;
use crabgent_core::Owner;
use crabgent_store::SessionStore;

use super::helpers::{OrderedPolicy, dispatch_scope, event, inbox};

#[tokio::test]
async fn dispatch_executes_matched_command() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order: Arc::clone(&order),
        deny_outer: false,
    });
    let (inbox, inner, sink, _store) = inbox(policy, Arc::clone(&order));

    inbox
        .receive(event("/stub arg"))
        .await
        .expect("dispatch ok");

    let order = order.lock().expect("test mutex must not be poisoned");
    assert!(order.iter().any(|entry| entry == "execute:arg"));
    assert_eq!(
        *inner.calls.lock().expect("test mutex must not be poisoned"),
        0
    );
    assert_eq!(
        sink.replies
            .lock()
            .expect("test mutex must not be poisoned")
            .as_slice(),
        &["stub ok".to_owned()]
    );
}

#[tokio::test]
async fn dispatch_falls_through_on_no_prefix() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order: Arc::clone(&order),
        deny_outer: false,
    });
    let (inbox, inner, _sink, _store) = inbox(policy, order);

    inbox.receive(event("hello")).await.expect("fallthrough ok");

    assert_eq!(
        *inner.calls.lock().expect("test mutex must not be poisoned"),
        1
    );
}

#[tokio::test]
async fn dispatch_unknown_command_falls_through_to_inner() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order: Arc::clone(&order),
        deny_outer: false,
    });
    let (inbox, inner, sink, _store) = inbox(policy, order);

    inbox
        .receive(event("/unknown arg"))
        .await
        .expect("unknown fallthrough ok");

    assert_eq!(
        *inner.calls.lock().expect("test mutex must not be poisoned"),
        1,
        "unknown command should fall through to inner kernel inbox",
    );
    assert_eq!(
        sink.replies
            .lock()
            .expect("test mutex must not be poisoned")
            .as_slice(),
        &[] as &[String],
    );
}

#[tokio::test]
async fn dispatch_calls_outer_policy_gate_before_inner() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order: Arc::clone(&order),
        deny_outer: false,
    });
    let (inbox, _inner, _sink, _store) = inbox(policy, Arc::clone(&order));

    inbox
        .receive(event("/stub arg"))
        .await
        .expect("dispatch ok");

    assert_eq!(
        order
            .lock()
            .expect("test mutex must not be poisoned")
            .as_slice(),
        &["stub", "policy_action", "stub.inner", "execute:arg"]
    );
}

#[tokio::test]
async fn dispatch_does_not_invoke_session_persist_hook() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order: Arc::clone(&order),
        deny_outer: false,
    });
    let (inbox, inner, _sink, store) = inbox(policy, order);

    inbox
        .receive(event("/stub arg"))
        .await
        .expect("dispatch ok");

    assert_eq!(
        *inner.calls.lock().expect("test mutex must not be poisoned"),
        0
    );
    let session = store
        .find_or_create(&Owner::new("test:conv"), None, &dispatch_scope())
        .await
        .expect("load session");
    assert_eq!(session.messages.len(), 2);
}

#[tokio::test]
async fn dispatch_empty_registry_falls_through() {
    use crate::handles::CommandHandles;
    use crate::inbox::CommandDispatchInbox;
    use crate::prefix::CommandPrefix;
    use crate::registry::CommandRegistry;
    use crabgent_store::memory::MemorySessionStore;

    use super::helpers::{RecordingInbox, RecordingSink, agent_name};

    let order = Arc::new(Mutex::new(Vec::new()));
    let policy = Arc::new(OrderedPolicy {
        order,
        deny_outer: false,
    });
    let store = Arc::new(MemorySessionStore::default());
    let handles =
        CommandHandles::new_unchecked(CommandRegistry::new(), store, policy, agent_name());
    let inner = Arc::new(RecordingInbox::default());
    let sink = Arc::new(RecordingSink::default());
    let inbox = CommandDispatchInbox::new(
        handles,
        CommandPrefix::default(),
        inner.clone(),
        sink.clone(),
    );

    inbox
        .receive(event("/stub arg"))
        .await
        .expect("empty registry fallthrough ok");

    assert_eq!(
        *inner.calls.lock().expect("test mutex must not be poisoned"),
        1,
        "empty registry should let prefixed text reach inner inbox",
    );
    assert!(
        sink.replies
            .lock()
            .expect("test mutex must not be poisoned")
            .is_empty()
    );
}
