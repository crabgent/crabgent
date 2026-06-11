use super::*;
use crate::channel::ChannelKind;
use crate::participant::ParticipantId;
use crate::test_support::RecordingChannel;

fn build_router(channels: Vec<Arc<dyn Channel>>) -> ChannelRouter {
    channels
        .into_iter()
        .fold(ChannelRouter::new(), ChannelRouter::with_channel)
}

#[test]
fn empty_router_reports_empty() {
    let r = ChannelRouter::new();
    assert!(r.is_empty());
    assert_eq!(r.len(), 0);
}

#[test]
fn register_channel_increases_len() {
    let stub: Arc<dyn Channel> =
        Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "id-1"));
    let r = ChannelRouter::new().with_channel(stub);
    assert_eq!(r.len(), 1);
    assert!(!r.is_empty());
}

#[test]
fn register_replaces_same_name() {
    let s1: Arc<dyn Channel> =
        Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "id-1"));
    let s2: Arc<dyn Channel> =
        Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "id-2"));
    let r = ChannelRouter::new().with_channel(s1).with_channel(s2);
    assert_eq!(r.len(), 1);
}

#[test]
fn get_returns_arc() {
    let stub: Arc<dyn Channel> = Arc::new(RecordingChannel::new("tg", ChannelKind::Direct, "id-1"));
    let r = ChannelRouter::new().with_channel(stub);
    assert!(r.get("tg").is_some());
    assert!(r.get("missing").is_none());
}

#[tokio::test]
async fn send_dispatches_via_metadata() {
    let stub = Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "id-1"));
    let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
    let r = build_router(vec![trait_obj]);
    let s = Subject::new("agent");
    let msg = OutboundMessage::new("hi").with_metadata("channel", "slack");
    let resp = r
        .send(&s, &Owner::new("slack:T1/C1"), &msg)
        .await
        .expect("ok");
    assert_eq!(resp.channel, "slack");
    assert_eq!(stub.sent_count(), 1);
}

#[tokio::test]
async fn send_dispatches_via_owner_prefix() {
    let stub = Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "id-1"));
    let trait_obj: Arc<dyn Channel> = Arc::clone(&stub) as _;
    let r = build_router(vec![trait_obj]);
    let s = Subject::new("agent");
    let msg = OutboundMessage::new("hi");
    let resp = r
        .send(&s, &Owner::new("slack:T1/C1"), &msg)
        .await
        .expect("ok");
    assert_eq!(resp.channel, "slack");
    assert_eq!(stub.sent_count(), 1);
}

#[tokio::test]
async fn send_returns_not_registered_for_unknown_channel() {
    let stub: Arc<dyn Channel> =
        Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "id-1"));
    let r = ChannelRouter::new().with_channel(stub);
    let s = Subject::new("agent");
    let msg = OutboundMessage::new("hi");
    let err = r
        .send(&s, &Owner::new("telegram:42"), &msg)
        .await
        .expect_err("should fail");
    match err {
        ChannelError::NotRegistered(name) => assert_eq!(name, "telegram"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn send_metadata_overrides_owner_prefix() {
    let slack = Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "id-1"));
    let tg = Arc::new(RecordingChannel::new(
        "telegram",
        ChannelKind::Direct,
        "id-1",
    ));
    let r = build_router(vec![
        Arc::clone(&slack) as Arc<dyn Channel>,
        Arc::clone(&tg) as Arc<dyn Channel>,
    ]);
    let s = Subject::new("agent");
    // Owner says slack, metadata says telegram. Metadata wins.
    let msg = OutboundMessage::new("hi").with_metadata("channel", "telegram");
    let _ = r
        .send(&s, &Owner::new("slack:T1/C1"), &msg)
        .await
        .expect("ok");
    assert_eq!(tg.sent_count(), 1);
    assert_eq!(slack.sent_count(), 0);
}

#[tokio::test]
async fn send_returns_invalid_owner_format_when_no_selector() {
    let r = ChannelRouter::new();
    let s = Subject::new("agent");
    let msg = OutboundMessage::new("hi");
    let err = r
        .send(&s, &Owner::new(""), &msg)
        .await
        .expect_err("should fail");
    match err {
        ChannelError::InvalidOwnerFormat(owner) => assert_eq!(owner, ""),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn send_returns_invalid_owner_format_for_bad_prefix() {
    let r = ChannelRouter::new();
    let s = Subject::new("agent");
    let msg = OutboundMessage::new("hi");
    let err = r
        .send(&s, &Owner::new(":missing"), &msg)
        .await
        .expect_err("should fail");
    match err {
        ChannelError::InvalidOwnerFormat(owner) => assert_eq!(owner, ":missing"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn react_routes_via_parent_channel() {
    let slack = Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "rx-1"));
    let telegram = Arc::new(RecordingChannel::new(
        "telegram",
        ChannelKind::Direct,
        "rx-2",
    ));
    let r = build_router(vec![
        Arc::clone(&slack) as Arc<dyn Channel>,
        Arc::clone(&telegram) as Arc<dyn Channel>,
    ]);
    let s = Subject::new("agent");
    let conv = Owner::new("telegram:42");
    let parent = MessageRef::top_level("slack", conv.clone(), "ts:1");
    let resp = r.react(&s, &conv, &parent, "👀").await.expect("ok");
    assert_eq!(resp.channel, "slack");
    assert_eq!(slack.react_count(), 1);
    assert_eq!(
        slack.last_reaction(),
        Some((parent.clone(), "👀".to_owned()))
    );
    assert_eq!(telegram.react_count(), 0);
}

#[tokio::test]
async fn react_falls_back_to_conv_prefix_when_parent_channel_empty() {
    let stub = Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "rx-1"));
    let r = build_router(vec![Arc::clone(&stub) as Arc<dyn Channel>]);
    let s = Subject::new("agent");
    let conv = Owner::new("slack:T1/C1");
    let parent = MessageRef {
        channel: String::new(),
        conv: conv.clone(),
        id: "ts:1".to_owned(),
        thread_root: None,
        broadcast: false,
    };
    let resp = r.react(&s, &conv, &parent, "👀").await.expect("ok");
    assert_eq!(resp.channel, "slack");
    assert_eq!(stub.react_count(), 1);
}

#[tokio::test]
async fn react_returns_unsupported_for_default_impl() {
    struct Bare;

    #[async_trait]
    impl Channel for Bare {
        fn name(&self) -> &'static str {
            "bare"
        }

        async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
            Ok(ChannelKind::Group)
        }

        async fn participants(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
        ) -> Result<Vec<crate::participant::Participant>, ChannelError> {
            Ok(Vec::new())
        }

        async fn send(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
            _msg: &OutboundMessage,
        ) -> Result<MessageRef, ChannelError> {
            Ok(MessageRef::top_level("bare", Owner::new("bare:1"), "id"))
        }
    }

    let r = build_router(vec![Arc::new(Bare) as Arc<dyn Channel>]);
    let s = Subject::new("agent");
    let conv = Owner::new("bare:1");
    let parent = MessageRef::top_level("bare", conv.clone(), "ts:1");
    let err = r
        .react(&s, &conv, &parent, "👀")
        .await
        .expect_err("should fail");
    assert!(matches!(err, ChannelError::Unsupported("react")));
}

#[tokio::test]
async fn react_returns_not_registered_for_unknown_channel() {
    let stub: Arc<dyn Channel> =
        Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "id-1"));
    let r = ChannelRouter::new().with_channel(stub);
    let s = Subject::new("agent");
    let conv = Owner::new("slack:T1/C1");
    let parent = MessageRef::top_level("telegram", conv.clone(), "ts:1");
    let err = r
        .react(&s, &conv, &parent, "👀")
        .await
        .expect_err("should fail");
    match err {
        ChannelError::NotRegistered(name) => assert_eq!(name, "telegram"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn edit_routes_via_target_channel() {
    let slack = Arc::new(RecordingChannel::new(
        "slack",
        ChannelKind::Direct,
        "edit-1",
    ));
    let telegram = Arc::new(RecordingChannel::new(
        "telegram",
        ChannelKind::Direct,
        "edit-2",
    ));
    let r = build_router(vec![
        Arc::clone(&slack) as Arc<dyn Channel>,
        Arc::clone(&telegram) as Arc<dyn Channel>,
    ]);
    let s = Subject::new("agent");
    let conv = Owner::new("telegram:42");
    let target = MessageRef::top_level("slack", conv.clone(), "ts:1");
    r.edit(&s, &conv, &target, "updated").await.expect("ok");
    assert_eq!(slack.edit_count(), 1);
    assert_eq!(telegram.edit_count(), 0);
}

#[tokio::test]
async fn delete_routes_via_target_channel() {
    let slack = Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "del-1"));
    let telegram = Arc::new(RecordingChannel::new(
        "telegram",
        ChannelKind::Direct,
        "del-2",
    ));
    let r = build_router(vec![
        Arc::clone(&slack) as Arc<dyn Channel>,
        Arc::clone(&telegram) as Arc<dyn Channel>,
    ]);
    let s = Subject::new("agent");
    let conv = Owner::new("telegram:42");
    let target = MessageRef::top_level("slack", conv.clone(), "ts:1");
    r.delete(&s, &conv, &target).await.expect("ok");
    assert_eq!(slack.delete_count(), 1);
    assert_eq!(telegram.delete_count(), 0);
}

#[tokio::test]
async fn upload_routes_via_conv_prefix_without_thread_parent() {
    let stub = Arc::new(RecordingChannel::new(
        "slack",
        ChannelKind::Direct,
        "file-1",
    ));
    let r = build_router(vec![Arc::clone(&stub) as Arc<dyn Channel>]);
    let s = Subject::new("agent");
    let conv = Owner::new("slack:T1/C1");
    let resp = r
        .upload(
            &s,
            &conv,
            "report.txt",
            b"hello".to_vec(),
            Some("caption"),
            None,
        )
        .await
        .expect("ok");
    assert_eq!(resp.id, "file-1");
    assert_eq!(stub.upload_count(), 1);
}

#[tokio::test]
async fn read_routes_via_thread_parent_channel() {
    let slack = Arc::new(RecordingChannel::new(
        "slack",
        ChannelKind::Direct,
        "read-1",
    ));
    let telegram = Arc::new(RecordingChannel::new(
        "telegram",
        ChannelKind::Direct,
        "read-2",
    ));
    let r = build_router(vec![
        Arc::clone(&slack) as Arc<dyn Channel>,
        Arc::clone(&telegram) as Arc<dyn Channel>,
    ]);
    let s = Subject::new("agent");
    let conv = Owner::new("telegram:42");
    let parent = MessageRef::top_level("slack", conv.clone(), "ts:1");
    let messages = r.read(&s, &conv, Some(&parent), 10).await.expect("ok");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message_ref.channel, "slack");
    assert_eq!(slack.read_count(), 1);
    assert_eq!(telegram.read_count(), 0);
}

#[tokio::test]
async fn notify_user_routes_via_metadata_channel() {
    let slack = Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "n-1"));
    let telegram = Arc::new(RecordingChannel::new(
        "telegram",
        ChannelKind::Direct,
        "n-2",
    ));
    let r = build_router(vec![
        Arc::clone(&slack) as Arc<dyn Channel>,
        Arc::clone(&telegram) as Arc<dyn Channel>,
    ]);
    let s = Subject::new("agent");
    let recipient = ParticipantId::new("U-alice");
    let msg = OutboundMessage::new("ping").with_metadata("channel", "slack");
    let resp = r.notify_user(&s, &recipient, &msg).await.expect("ok");
    assert_eq!(resp.channel, "slack");
    assert_eq!(slack.notify_user_count(), 1);
    assert_eq!(telegram.notify_user_count(), 0);
}

#[tokio::test]
async fn notify_user_returns_unsupported_for_default_impl() {
    struct Bare;

    #[async_trait]
    impl Channel for Bare {
        fn name(&self) -> &'static str {
            "bare"
        }

        async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
            Ok(ChannelKind::Direct)
        }

        async fn participants(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
        ) -> Result<Vec<crate::participant::Participant>, ChannelError> {
            Ok(Vec::new())
        }

        async fn send(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
            _msg: &OutboundMessage,
        ) -> Result<MessageRef, ChannelError> {
            Ok(MessageRef::top_level("bare", Owner::new("bare:1"), "id"))
        }
    }

    let r = build_router(vec![Arc::new(Bare) as Arc<dyn Channel>]);
    let s = Subject::new("agent");
    let recipient = ParticipantId::new("U-bare-target");
    let msg = OutboundMessage::new("ping").with_metadata("channel", "bare");
    let err = r
        .notify_user(&s, &recipient, &msg)
        .await
        .expect_err("should fail");
    assert!(matches!(err, ChannelError::Unsupported("notify_user")));
}

#[tokio::test]
async fn notify_user_returns_not_registered_for_unknown_channel() {
    let r = ChannelRouter::new();
    let s = Subject::new("agent");
    let recipient = ParticipantId::new("U-alice");
    let msg = OutboundMessage::new("ping").with_metadata("channel", "telegram");
    let err = r
        .notify_user(&s, &recipient, &msg)
        .await
        .expect_err("should fail");
    match err {
        ChannelError::NotRegistered(name) => assert_eq!(name, "telegram"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn notify_user_returns_invalid_envelope_when_metadata_missing() {
    let r = ChannelRouter::new();
    let s = Subject::new("agent");
    let recipient = ParticipantId::new("U-alice");
    let msg = OutboundMessage::new("ping");
    let err = r
        .notify_user(&s, &recipient, &msg)
        .await
        .expect_err("should fail");
    match err {
        ChannelError::InvalidEnvelope(reason) => {
            assert!(
                reason.contains("metadata.channel"),
                "reason should mention metadata.channel: {reason}"
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn notify_user_returns_invalid_envelope_when_metadata_empty() {
    let stub: Arc<dyn Channel> =
        Arc::new(RecordingChannel::new("slack", ChannelKind::Direct, "n-1"));
    let r = ChannelRouter::new().with_channel(stub);
    let s = Subject::new("agent");
    let recipient = ParticipantId::new("U-alice");
    let msg = OutboundMessage::new("ping").with_metadata("channel", "");
    let err = r
        .notify_user(&s, &recipient, &msg)
        .await
        .expect_err("should fail");
    assert!(matches!(err, ChannelError::InvalidEnvelope(_)));
}
