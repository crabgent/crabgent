#[path = "support/vision_provider.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use crabgent_channel::{
    ChannelInbox, InboundEvent, KernelChannelInbox, MessageRef, Participant, ParticipantId,
    ParticipantRole,
};
use crabgent_core::{
    ContentBlock, ImagePayload, Kernel, owner::Owner, policy::AllowAllPolicy, types::LlmRequest,
};
use serde_json::{Value, json};
use tokio::time::timeout;

#[tokio::test]
async fn vision_inbound_to_user_message_with_image() {
    let provider =
        support::RecordingProvider::with_caps("test-vision-model", "recording", true, false);
    let kernel = Kernel::builder()
        .provider(provider.clone())
        .policy(AllowAllPolicy)
        .build();
    let inbox = Arc::new(KernelChannelInbox::new(
        Arc::new(kernel),
        "test-vision-model",
        Arc::new(AllowAllPolicy),
    ));

    inbox
        .receive(InboundEvent {
            channel: "slack".to_owned(),
            conv: Owner::new("slack:vision-room"),
            kind: None,
            from: Participant::new(ParticipantId::new("U1"), ParticipantRole::Human),
            message: MessageRef::top_level("slack", Owner::new("slack:vision-room"), "ts:1"),
            body: "describe this image".to_owned(),
            attachments: vec![ContentBlock::Image(
                ImagePayload::new(minimal_png_bytes(), "image/png").expect("valid image payload"),
            )],
            timestamp: Utc::now(),
        })
        .await
        .expect("receive should accept inbound vision event");

    let captured = wait_for_request(&provider).await;
    assert_eq!(captured.len(), 1);
    let message = captured
        .first()
        .expect("kernel should capture one request")
        .messages
        .first()
        .expect("kernel request has first message");
    let role = message.get("role").and_then(Value::as_str);
    assert_eq!(role, Some("user"));
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .expect("user message has content array");
    assert_eq!(content.len(), 2);
    let text_block = content.first().expect("content should include text block");
    assert_eq!(text_block.get("type"), Some(&json!("text")));
    assert_eq!(
        text_block.get("text"),
        Some(&json!(
            "<inbound source=\"unknown\" channel=\"slack\">describe this image</inbound>"
        ))
    );
    let image_block = content.get(1).expect("content should include image block");
    assert_eq!(image_block.get("type"), Some(&json!("image")));
    assert_eq!(image_block.get("mime"), Some(&json!("image/png")));
    assert!(image_block.get("data").is_some_and(Value::is_string));
    assert!(image_block.get("source").is_none());
}

#[tokio::test]
async fn vision_attachments_default_empty_unchanged() {
    let provider =
        support::RecordingProvider::with_caps("test-vision-model", "recording", true, false);
    let kernel = Kernel::builder()
        .provider(provider.clone())
        .policy(AllowAllPolicy)
        .build();
    let inbox = Arc::new(KernelChannelInbox::new(
        Arc::new(kernel),
        "test-vision-model",
        Arc::new(AllowAllPolicy),
    ));

    inbox
        .receive(InboundEvent {
            channel: "slack".to_owned(),
            conv: Owner::new("slack:vision-room"),
            kind: None,
            from: Participant::new(ParticipantId::new("U1"), ParticipantRole::Human),
            message: MessageRef::top_level("slack", Owner::new("slack:vision-room"), "ts:2"),
            body: "plain message".to_owned(),
            attachments: vec![],
            timestamp: Utc::now(),
        })
        .await
        .expect("receive should accept inbound non-vision event");

    let captured = wait_for_request(&provider).await;
    assert_eq!(captured.len(), 1);
    let message = captured
        .first()
        .expect("kernel should capture one request")
        .messages
        .first()
        .expect("kernel request has first message");
    let content = message
        .get("content")
        .and_then(Value::as_array)
        .expect("user message has content array");
    assert_eq!(content.len(), 1);
    let text_block = content.first().expect("content should include text block");
    assert_eq!(text_block.get("type"), Some(&json!("text")));
    assert_eq!(
        text_block.get("text"),
        Some(&json!(
            "<inbound source=\"unknown\" channel=\"slack\">plain message</inbound>"
        ))
    );
}

async fn wait_for_request(provider: &support::RecordingProvider) -> Vec<LlmRequest> {
    timeout(Duration::from_secs(5), async {
        loop {
            if !provider.captured().is_empty() {
                return provider.captured();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("kernel run should capture one request within timeout")
}

fn minimal_png_bytes() -> Vec<u8> {
    let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(
        b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00",
    );
    png
}
