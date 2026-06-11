mod support;

use crabgent_channel::{Channel, OutboundMessage, ParticipantId};
use crabgent_channel_matrix::MatrixChannel;
use crabgent_core::subject::Subject;
use matrix_sdk::ruma::OwnedRoomId;

const MATRIX_OWNER_PREFIX: &str = "matrix:";

fn room_id_from_owner(owner: &crabgent_core::owner::Owner) -> OwnedRoomId {
    let raw = owner
        .as_str()
        .strip_prefix(MATRIX_OWNER_PREFIX)
        .expect("notify_user MessageRef must carry matrix-prefixed owner");
    OwnedRoomId::try_from(raw.to_owned()).expect("conv suffix must be a valid Matrix room id")
}

#[tokio::test]
async fn notify_user_creates_dm_when_none_exists() {
    let Some(ctx) = support::matrix_test_ctx()
        .await
        .expect("Matrix test context should initialize")
    else {
        return;
    };
    let bot = ctx
        .register_client("bot")
        .await
        .expect("bot client should register");
    let alice = ctx
        .register_client("alice")
        .await
        .expect("alice client should register");
    let _ = bot
        .sync_once(support::short_sync())
        .await
        .expect("bot sync should complete");
    let _ = alice
        .sync_once(support::short_sync())
        .await
        .expect("alice sync should complete");

    let channel = MatrixChannel::from_client(
        bot.clone(),
        bot.user_id().expect("bot logged in").to_owned(),
        None,
    );

    let alice_user_id = alice.user_id().expect("alice logged in").to_owned();
    let recipient = ParticipantId::new(alice_user_id.to_string());
    let result = channel
        .notify_user(
            &Subject::new("agent"),
            &recipient,
            &OutboundMessage::new("first ping"),
        )
        .await
        .expect("notify_user should create DM");
    assert_eq!(result.channel, "matrix");

    let room_id = room_id_from_owner(&result.conv);
    alice
        .join_room_by_id(&room_id)
        .await
        .expect("alice should join created DM");
    let event = support::wait_for_room_message(&alice, &room_id, "first ping")
        .await
        .expect("alice should receive first ping");
    assert_eq!(event.sender, bot.user_id().expect("bot logged in"));
}

#[tokio::test]
async fn notify_user_reuses_existing_dm_room() {
    let Some(ctx) = support::matrix_test_ctx()
        .await
        .expect("Matrix test context should initialize")
    else {
        return;
    };
    let bot = ctx
        .register_client("bot")
        .await
        .expect("bot client should register");
    let alice = ctx
        .register_client("alice")
        .await
        .expect("alice client should register");
    let _ = bot
        .sync_once(support::short_sync())
        .await
        .expect("bot sync should complete");

    let channel = MatrixChannel::from_client(
        bot.clone(),
        bot.user_id().expect("bot logged in").to_owned(),
        None,
    );
    let alice_user_id = alice.user_id().expect("alice logged in").to_owned();
    let recipient = ParticipantId::new(alice_user_id.to_string());

    let first = channel
        .notify_user(
            &Subject::new("agent"),
            &recipient,
            &OutboundMessage::new("first ping"),
        )
        .await
        .expect("first notify_user should create DM");
    let first_room = room_id_from_owner(&first.conv);
    alice
        .join_room_by_id(&first_room)
        .await
        .expect("alice should join first DM");
    let _ = support::wait_for_room_message(&alice, &first_room, "first ping")
        .await
        .expect("alice should receive first ping");
    let _ = bot
        .sync_once(support::short_sync())
        .await
        .expect("bot sync should see first DM");

    let second = channel
        .notify_user(
            &Subject::new("agent"),
            &recipient,
            &OutboundMessage::new("second ping"),
        )
        .await
        .expect("second notify_user should reuse DM");
    let second_room = room_id_from_owner(&second.conv);
    assert_eq!(
        first_room, second_room,
        "second notify_user must reuse the DM created by the first call"
    );
    let _ = support::wait_for_room_message(&alice, &second_room, "second ping")
        .await
        .expect("alice should receive second ping");
}

#[tokio::test]
async fn notify_user_reuses_room_created_by_other_party() {
    let Some(ctx) = support::matrix_test_ctx()
        .await
        .expect("Matrix test context should initialize")
    else {
        return;
    };
    let bot = ctx
        .register_client("bot")
        .await
        .expect("bot client should register");
    let alice = ctx
        .register_client("alice")
        .await
        .expect("alice client should register");
    let _ = bot
        .sync_once(support::short_sync())
        .await
        .expect("bot sync should complete");
    let _ = alice
        .sync_once(support::short_sync())
        .await
        .expect("alice sync should complete");

    // Alice (the user) creates the DM with the bot. Bot accepts the
    // invite. The bot's own `m.direct` account-data is NOT updated by
    // this flow, so `Client::get_dm_room` on the bot side would miss
    // this room. The heuristic fallback in `notify_user` must still
    // find it via the 2-member scan and reuse it.
    let alice_dm = alice
        .create_dm(bot.user_id().expect("bot logged in"))
        .await
        .expect("alice should create DM with bot");
    let dm_room_id = alice_dm.room_id().to_owned();
    bot.join_room_by_id(&dm_room_id)
        .await
        .expect("bot should join alice-created DM");
    let _ = bot
        .sync_once(support::short_sync())
        .await
        .expect("bot sync should see alice-created DM");

    let channel = MatrixChannel::from_client(
        bot.clone(),
        bot.user_id().expect("bot logged in").to_owned(),
        None,
    );
    let alice_user_id = alice.user_id().expect("alice logged in").to_owned();
    let recipient = ParticipantId::new(alice_user_id.to_string());

    let result = channel
        .notify_user(
            &Subject::new("agent"),
            &recipient,
            &OutboundMessage::new("hello in existing dm"),
        )
        .await
        .expect("notify_user should reuse alice-created DM");
    let delivered = room_id_from_owner(&result.conv);
    assert_eq!(
        delivered, dm_room_id,
        "notify_user must reuse the DM alice created, not spawn a new one"
    );
    let event = support::wait_for_room_message(&alice, &dm_room_id, "hello in existing dm")
        .await
        .expect("alice should receive message in existing DM");
    assert_eq!(event.sender, bot.user_id().expect("bot logged in"));
}

#[tokio::test]
async fn notify_user_rejects_invalid_recipient_format() {
    let Some(ctx) = support::matrix_test_ctx()
        .await
        .expect("Matrix test context should initialize")
    else {
        return;
    };
    let bot = ctx
        .register_client("bot")
        .await
        .expect("bot client should register");
    let channel = MatrixChannel::from_client(
        bot.clone(),
        bot.user_id().expect("bot logged in").to_owned(),
        None,
    );
    let result = channel
        .notify_user(
            &Subject::new("agent"),
            &ParticipantId::new("not-a-user-id"),
            &OutboundMessage::new("body"),
        )
        .await;
    assert!(matches!(
        result,
        Err(crabgent_channel::ChannelError::InvalidEnvelope(_))
    ));
}
