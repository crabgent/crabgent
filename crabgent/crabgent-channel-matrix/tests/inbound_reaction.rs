#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use matrix_sdk::ruma::events::{reaction::ReactionEventContent, relation::Annotation};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn alice_reaction_to_bot_message_dispatches_added_event() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };
    let target_id = support::send_from(&fixture.bot, &fixture.room_id, "react to me")
        .await
        .expect("bot message should send");
    // Alice must see the bot message before she can react to it.
    let _ = support::wait_for_room_message(&fixture.alice, &fixture.room_id, "react to me")
        .await
        .expect("alice should see bot message before reacting");
    let alice_room = fixture
        .alice
        .get_room(&fixture.room_id)
        .expect("alice should be joined to reaction room");
    let content = ReactionEventContent::new(Annotation::new(target_id.clone(), "👍".to_owned()));
    alice_room
        .send(content)
        .await
        .expect("alice reaction should send");

    let cancel = CancellationToken::new();
    let reaction = support::collect_one_inbound_reaction(Arc::clone(&fixture.channel), cancel)
        .await
        .expect("reaction inbound event should be collected");

    assert_eq!(reaction.channel, "matrix");
    assert_eq!(reaction.emoji, "👍");
    assert!(reaction.added);
    assert_eq!(reaction.parent.id, target_id.to_string());
    assert_eq!(
        reaction.conv.as_str(),
        format!("matrix:{}", fixture.room_id)
    );
}
