#[path = "support/mod.rs"]
mod support;

use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn sync_poller_delivers_inbound_thread_root() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };
    let root = support::send_from(&fixture.alice, &fixture.room_id, "inbound thread root")
        .await
        .expect("thread root message should send");
    let alice_room = fixture
        .alice
        .get_room(&fixture.room_id)
        .expect("alice room should exist");
    let mut content = matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(
        "inbound thread reply",
    );
    content.relates_to = Some(matrix_sdk::ruma::events::room::message::Relation::Thread(
        matrix_sdk::ruma::events::relation::Thread::reply(root.clone(), root.clone()),
    ));
    alice_room
        .send(content)
        .await
        .expect("thread reply should send");
    let cancel = CancellationToken::new();
    let event = support::collect_one_inbound(fixture.channel, cancel)
        .await
        .expect("thread reply inbound event should be collected");
    assert_eq!(event.body, "inbound thread reply");
    assert_eq!(event.message.thread_root.as_deref(), Some(root.as_str()));
}
