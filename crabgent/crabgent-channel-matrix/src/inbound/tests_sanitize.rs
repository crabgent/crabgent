//! Hardening design8 prompt-injection: inbound sanitize coverage for the three
//! Matrix message types (text, image caption, audio body). Sibling of
//! `mod tests` so `inbound/tests.rs` stays under the 500-line cap.

use super::test_helpers::{media_clients, test_client, test_client_at};
use super::*;
use crabgent_test_support::minimal_ogg_bytes;
use httpmock::{Method::GET, MockServer};
use matrix_sdk::ruma::{owned_room_id, owned_user_id};
use serde_json::json;

fn timeline_json(sender: &str, content: &serde_json::Value) -> TimelineEvent {
    let raw = matrix_sdk::ruma::serde::Raw::new(&json!({
        "type": "m.room.message",
        "event_id": "$event:example.org",
        "sender": sender,
        "origin_server_ts": 1_700_000_000_000_u64,
        "content": content.clone(),
    }))
    .expect("test result")
    .cast_unchecked();
    TimelineEvent::from_plaintext(raw)
}

#[tokio::test]
async fn matrix_text_strips_control_chars() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = timeline_json(
        "@alice:example.org",
        &json!({"msgtype": "m.text", "body": "a\u{0000}b\u{200B}c\u{202E}d"}),
    );
    let matrix_client = test_client().await;
    let image_http_client = reqwest::Client::new();
    let audio_http_client = reqwest::Client::new();
    let media = media_clients(
        &matrix_client,
        &image_http_client,
        &audio_http_client,
        None,
        None,
    );
    let inbound = timeline_event_to_inbound(&room, &event, &bot, None, &media)
        .await
        .expect("test result");
    assert_eq!(inbound.body, "abcd");
}

#[tokio::test]
async fn matrix_image_caption_strips_control_chars() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = timeline_json(
        "@alice:example.org",
        &json!({
            "msgtype": "m.image",
            "body": "cap\u{0000}tion\u{200B}done",
            "filename": "pic.png",
            "url": "mxc://localhost/img-sanitize-1",
        }),
    );
    let matrix_client = test_client().await;
    let image_http_client = reqwest::Client::new();
    let audio_http_client = reqwest::Client::new();
    let media = media_clients(
        &matrix_client,
        &image_http_client,
        &audio_http_client,
        None,
        None,
    );
    let inbound = timeline_event_to_inbound(&room, &event, &bot, None, &media)
        .await
        .expect("test result");
    assert_eq!(inbound.body, "captiondone");
}

#[tokio::test]
async fn matrix_audio_filename_strips_control_chars_in_body_field_only() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET)
            .path("/_matrix/client/v1/media/download/localhost/audio-sanitize-1")
            .header("authorization", "Bearer test-token");
        then.status(200)
            .header("content-type", "audio/ogg")
            .body(minimal_ogg_bytes());
    });

    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = timeline_json(
        "@alice:example.org",
        &json!({
            "msgtype": "m.audio",
            "body": "voice\u{0000}\u{200B}.ogg",
            "url": "mxc://localhost/audio-sanitize-1",
        }),
    );
    let matrix_client = test_client_at(&server.base_url()).await;
    let image_http_client = reqwest::Client::new();
    let audio_http_client = reqwest::Client::new();
    let audio_validator = AudioValidator::new();
    let media = media_clients(
        &matrix_client,
        &image_http_client,
        &audio_http_client,
        Some(&audio_validator),
        Some("test-token"),
    );

    let inbound = timeline_event_to_inbound(&room, &event, &bot, None, &media)
        .await
        .expect("test result");

    // InboundEvent.body is sanitized: it is LLM-bound.
    assert_eq!(inbound.body, "voice.ogg");
    // AudioPayload.filename keeps the raw body: it is metadata, not LLM-bound.
    let [ContentBlock::Audio(payload)] = inbound.attachments.as_slice() else {
        panic!(
            "expected one audio attachment, got {:?}",
            inbound.attachments
        );
    };
    assert_eq!(
        payload.filename.as_deref(),
        Some("voice\u{0000}\u{200B}.ogg")
    );
}

#[tokio::test]
async fn matrix_oversize_text_returns_none() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let big = "a".repeat(9000);
    let event = timeline_json(
        "@alice:example.org",
        &json!({"msgtype": "m.text", "body": big}),
    );
    let matrix_client = test_client().await;
    let image_http_client = reqwest::Client::new();
    let audio_http_client = reqwest::Client::new();
    let media = media_clients(
        &matrix_client,
        &image_http_client,
        &audio_http_client,
        None,
        None,
    );
    assert!(
        timeline_event_to_inbound(&room, &event, &bot, None, &media)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn matrix_oversize_image_caption_returns_none() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let big = "a".repeat(9000);
    let event = timeline_json(
        "@alice:example.org",
        &json!({
            "msgtype": "m.image",
            "body": big,
            "filename": "pic.png",
            "url": "mxc://localhost/img-oversize",
        }),
    );
    let matrix_client = test_client().await;
    let image_http_client = reqwest::Client::new();
    let audio_http_client = reqwest::Client::new();
    let media = media_clients(
        &matrix_client,
        &image_http_client,
        &audio_http_client,
        None,
        None,
    );
    assert!(
        timeline_event_to_inbound(&room, &event, &bot, None, &media)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn matrix_oversize_audio_returns_none() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let big = "a".repeat(9000);
    let event = timeline_json(
        "@alice:example.org",
        &json!({
            "msgtype": "m.audio",
            "body": big,
            "url": "mxc://localhost/audio-oversize",
        }),
    );
    let matrix_client = test_client().await;
    let image_http_client = reqwest::Client::new();
    let audio_http_client = reqwest::Client::new();
    let audio_validator = AudioValidator::new();
    let media = media_clients(
        &matrix_client,
        &image_http_client,
        &audio_http_client,
        Some(&audio_validator),
        Some("test-token"),
    );
    assert!(
        timeline_event_to_inbound(&room, &event, &bot, None, &media)
            .await
            .is_none()
    );
}
