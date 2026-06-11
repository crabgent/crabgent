//! The mid-turn inject value re-renders the `<inbound>` tag from the same
//! display-stamped subject the live request uses, so both always carry the
//! same channel/name/workspace/sender context (`plan_from_request`).

use crate::channel::ConvLabel;
use crate::subject::ChannelSubjectExt;
use crabgent_core::subject::Subject;

use super::*;

fn user_text(req: &RunRequest) -> &str {
    match &req.messages[0] {
        Message::User { content, .. } => match &content[0] {
            ContentBlock::Text { text } => text,
            other => panic!("unexpected content block: {other:?}"),
        },
        other => panic!("unexpected message: {other:?}"),
    }
}

#[test]
fn plan_inject_value_matches_live_request_user_text_with_display_attrs() {
    let inbox = allow_inbox("m").with_inferred_kind(ChannelKind::Group);
    let event = build_event("slack", "slack:T1/C1", ParticipantRole::Human, "hello");
    let subject: Subject = Subject::new("u")
        .with_channel("slack", &Owner::new("slack:T1/C1"), ChannelKind::Group)
        .with_conv_display(&ConvLabel {
            name: Some("#platform-ops".to_owned()),
            workspace: Some("example".to_owned()),
        })
        .with_sender_display(Some("Alice"));

    let plan = inbox
        .plan_event_with_subject(&event, subject)
        .expect("plan ok");

    let live_text = user_text(&plan.req);
    let inject_text = plan.inject_value["content"][0]["text"]
        .as_str()
        .expect("inject value text");
    // Both renders are byte-identical: the inject value carries the same
    // display context the live request does.
    assert_eq!(inject_text, live_text);
    assert!(
        inject_text.contains("name=\"#platform-ops\"") && inject_text.contains("sender=\"Alice\""),
        "display attrs present in both renders: {inject_text:?}"
    );
}
