#[path = "support/mod.rs"]
mod support;

use crabgent_channel::{Channel, MessageRef};
use crabgent_core::{owner::Owner, subject::Subject};
use matrix_sdk::ruma::{
    OwnedEventId,
    events::{OriginalSyncMessageLikeEvent, reaction::ReactionEventContent},
};

#[tokio::test]
async fn react_sends_matrix_annotation_event() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };
    let conv = Owner::new(format!("matrix:{}", fixture.room_id));
    let target_id = support::send_from(&fixture.alice, &fixture.room_id, "react target")
        .await
        .expect("reaction target message should send");
    let parent = MessageRef::top_level("matrix", conv.clone(), target_id.to_string());

    let sent = fixture
        .channel
        .react(&Subject::new("agent"), &conv, &parent, "👀")
        .await
        .expect("matrix reaction should send");

    assert_eq!(sent.channel, "matrix");
    assert_eq!(sent.conv, conv);
    assert!(sent.thread_root.is_none());
    assert_ne!(sent.id, parent.id);

    let reaction = wait_for_reaction(&fixture.alice, &fixture.room_id, &target_id, "👀")
        .await
        .expect("reaction event should be visible to alice");
    assert_eq!(reaction.event_id.to_string(), sent.id);
}

async fn wait_for_reaction(
    client: &matrix_sdk::Client,
    room_id: &matrix_sdk::ruma::RoomId,
    target_id: &OwnedEventId,
    key: &str,
) -> support::TestResult<OriginalSyncMessageLikeEvent<ReactionEventContent>> {
    for _ in 0..20 {
        let response = client.sync_once(support::short_sync()).await?;
        let Some(joined) = response.rooms.joined.get(room_id) else {
            continue;
        };
        for event in &joined.timeline.events {
            let Ok(event) = event
                .raw()
                .deserialize_as_unchecked::<OriginalSyncMessageLikeEvent<ReactionEventContent>>()
            else {
                continue;
            };
            if event.content.relates_to.event_id == *target_id
                && event.content.relates_to.key == key
            {
                return Ok(event);
            }
        }
    }
    Err(format!("timed out waiting for reaction in {room_id}").into())
}
