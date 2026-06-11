use std::sync::Arc;

use chrono::Utc;
use crabgent_channel::{
    ChannelKind, ChannelSubjectExt, InboundEvent, MessageRef, Participant, ParticipantRole,
    attr_keys, parse_channel_subject_id,
};
use crabgent_channel_matrix::{MatrixChannel, build_subject_resolver};
use crabgent_core::owner::Owner;
use matrix_sdk::{Client, ruma::owned_user_id};
use url::Url;

async fn build_channel() -> Arc<MatrixChannel> {
    let client = Client::new(Url::parse("https://example.org").expect("test result"))
        .await
        .expect("test result");
    Arc::new(MatrixChannel::from_client(
        client,
        owned_user_id!("@bot:example.org"),
        Some("Nova".into()),
    ))
}

fn inbound_event(room_id: &str, participant_id: &str) -> InboundEvent {
    let conv = Owner::new(format!("matrix:{room_id}"));
    InboundEvent {
        channel: "matrix".into(),
        conv: conv.clone(),
        kind: None,
        from: Participant::new(participant_id, ParticipantRole::Human),
        message: MessageRef::top_level("matrix", conv, "$event:example.org"),
        body: "hello".into(),
        attachments: vec![],
        timestamp: Utc::now(),
    }
}

fn inbound_event_with_kind(room_id: &str, participant_id: &str, kind: ChannelKind) -> InboundEvent {
    let mut event = inbound_event(room_id, participant_id);
    event.kind = Some(kind);
    event
}

#[tokio::test]
async fn resolver_stamps_direct_room_subject_attrs() {
    let channel = build_channel().await;
    let room_id = "!direct:example.org".try_into().expect("test result");
    channel
        .kind_cache()
        .lock()
        .expect("matrix room-kind cache lock should not be poisoned")
        .insert(room_id, ChannelKind::Direct);

    let resolver = build_subject_resolver(Arc::clone(&channel), "nova".into());
    let subject = resolver(&inbound_event("!direct:example.org", "@alice:example.org"));
    let channel_attrs = subject.channel().expect("channel attrs present");
    let (_, participant_id) =
        parse_channel_subject_id(subject.id()).expect("subject id should parse");

    assert_eq!(channel_attrs.channel, "matrix");
    assert_eq!(channel_attrs.conv, "matrix:!direct:example.org");
    assert_eq!(channel_attrs.kind, ChannelKind::Direct);
    assert_eq!(participant_id, "@alice:example.org");
    assert_eq!(subject.participant_role(), Some("human"));
    assert_eq!(
        subject.attr(attr_keys::PARTICIPANT_ID),
        Some("@alice:example.org")
    );
    assert_eq!(subject.attr("agent"), Some("nova"));
}

#[tokio::test]
async fn resolver_stamps_group_room_subject_attrs() {
    let channel = build_channel().await;
    let room_id = "!group:example.org".try_into().expect("test result");
    channel
        .kind_cache()
        .lock()
        .expect("matrix room-kind cache lock should not be poisoned")
        .insert(room_id, ChannelKind::Group);

    let resolver = build_subject_resolver(Arc::clone(&channel), "nova".into());
    let subject = resolver(&inbound_event("!group:example.org", "@bob:example.org"));

    assert_eq!(
        subject.channel().expect("channel attrs present").kind,
        ChannelKind::Group
    );
    assert_eq!(
        subject.attr(attr_keys::PARTICIPANT_ID),
        Some("@bob:example.org")
    );
}

#[tokio::test]
async fn resolver_defaults_cache_miss_to_group() {
    let channel = build_channel().await;
    let resolver = build_subject_resolver(Arc::clone(&channel), "nova".into());
    let subject = resolver(&inbound_event("!missing:example.org", "@carol:example.org"));

    assert_eq!(
        subject.channel().expect("channel attrs present").kind,
        ChannelKind::Group
    );
}

#[tokio::test]
async fn resolver_prefers_event_kind_on_cache_miss() {
    let channel = build_channel().await;
    let resolver = build_subject_resolver(Arc::clone(&channel), "nova".into());
    let subject = resolver(&inbound_event_with_kind(
        "!missing:example.org",
        "@carol:example.org",
        ChannelKind::Direct,
    ));

    assert_eq!(
        subject.channel().expect("channel attrs present").kind,
        ChannelKind::Direct
    );
}

#[tokio::test]
async fn resolver_prefers_event_kind_over_cache() {
    let channel = build_channel().await;
    let room_id = "!conflict:example.org".try_into().expect("test result");
    channel
        .kind_cache()
        .lock()
        .expect("matrix room-kind cache lock should not be poisoned")
        .insert(room_id, ChannelKind::Group);
    let resolver = build_subject_resolver(Arc::clone(&channel), "nova".into());
    let subject = resolver(&inbound_event_with_kind(
        "!conflict:example.org",
        "@carol:example.org",
        ChannelKind::Direct,
    ));

    assert_eq!(
        subject.channel().expect("channel attrs present").kind,
        ChannelKind::Direct
    );
}

#[tokio::test]
async fn resolver_stamps_inbound_message_ref() {
    let channel = build_channel().await;
    let resolver = build_subject_resolver(Arc::clone(&channel), "nova".into());
    let event = inbound_event("!room:example.org", "@dave:example.org");
    let subject = resolver(&event);

    assert_eq!(subject.inbound_message_ref(), Some(event.message));
}
