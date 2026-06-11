#[path = "support/mod.rs"]
mod support;

use crabgent_channel::{Channel, OutboundMessage};
use crabgent_core::owner::Owner;

#[tokio::test]
async fn send_top_level_reaches_matrix_room() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };
    let conv = Owner::new(format!("matrix:{}", fixture.room_id));
    let sent = fixture
        .channel
        .send(
            &crabgent_core::subject::Subject::new("agent"),
            &conv,
            &OutboundMessage::new("hello from matrix channel"),
        )
        .await
        .expect("top-level Matrix message should send");
    assert_eq!(sent.channel, "matrix");
    assert!(sent.thread_root.is_none());
    let event = support::wait_for_room_message(
        &fixture.alice,
        &fixture.room_id,
        "hello from matrix channel",
    )
    .await
    .expect("alice should receive top-level message");
    assert_eq!(event.event_id.to_string(), sent.id);
}
