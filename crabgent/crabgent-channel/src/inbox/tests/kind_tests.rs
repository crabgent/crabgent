use super::*;

#[test]
fn inferred_kind_stamps_channel_kind_attr() {
    let inbox = allow_inbox("m").with_inferred_kind(ChannelKind::Direct);
    let ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    let req = inbox.build_request(&ev).expect("valid subject");
    assert_eq!(req.subject.attr("channel_kind"), Some("direct"));
}

#[test]
fn inbound_event_kind_stamps_channel_kind_attr() {
    let inbox = allow_inbox("m");
    let mut ev = build_event("slack", "slack:T1/C1", ParticipantRole::Human, "hi");
    ev.kind = Some(ChannelKind::Group);
    let req = inbox.build_request(&ev).expect("valid subject");
    assert_eq!(req.subject.attr("channel_kind"), Some("group"));
}

#[test]
fn inbound_event_kind_overrides_static_inferred_kind() {
    let inbox = allow_inbox("m").with_inferred_kind(ChannelKind::Group);
    let mut ev = build_event("slack", "slack:T1/D1", ParticipantRole::Human, "hi");
    ev.kind = Some(ChannelKind::Direct);

    let req = inbox.build_request(&ev).expect("valid subject");

    assert_eq!(req.subject.attr("channel_kind"), Some("direct"));
    let prompt = req.system_prompt.as_deref().expect("hint default-on");
    assert!(prompt.contains("direct conversation"), "{prompt:?}");
    match &req.messages[0] {
        Message::User { content, .. } => {
            assert!(matches!(
                &content[0],
                ContentBlock::Text { text } if text == "<inbound source=\"direct\" channel=\"slack\">hi</inbound>"
            ));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn inbound_event_kind_feeds_conversation_hint_without_static_kind() {
    let inbox = allow_inbox("m");
    let mut ev = build_event("slack", "slack:T1/C1", ParticipantRole::Human, "hi");
    ev.kind = Some(ChannelKind::Group);

    let req = inbox.build_request(&ev).expect("valid subject");

    let prompt = req.system_prompt.as_deref().expect("hint default-on");
    assert!(prompt.contains("group conversation"), "{prompt:?}");
}
