#[path = "support/mod.rs"]
mod support;

use crabgent_channel::{Channel, MessageRef, OutboundMessage};
use crabgent_core::owner::Owner;

#[tokio::test]
async fn send_thread_reply_sets_matrix_thread_relation() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };
    let root = support::send_from(&fixture.alice, &fixture.room_id, "thread root")
        .await
        .expect("thread root message should send");
    let conv = Owner::new(format!("matrix:{}", fixture.room_id));
    let parent = MessageRef::top_level("matrix", conv.clone(), root.to_string());
    let sent = fixture
        .channel
        .send(
            &crabgent_core::subject::Subject::new("agent"),
            &conv,
            &OutboundMessage::new("thread reply from bot").in_thread(parent),
        )
        .await
        .expect("thread reply should send");
    assert_eq!(sent.thread_root.as_deref(), Some(root.as_str()));
    let event =
        support::wait_for_room_message(&fixture.alice, &fixture.room_id, "thread reply from bot")
            .await
            .expect("alice should receive thread reply");
    assert!(support::is_thread_reply(&event, &root));
}
