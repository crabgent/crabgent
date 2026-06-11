use super::test_helpers::{media_clients, test_client, test_client_at};
use super::*;
use crate::reaction_tracker::{ReactionTracker, TrackedReaction};
use crabgent_test_support::minimal_ogg_bytes;
use httpmock::{Method::GET, MockServer};
use matrix_sdk::ruma::{owned_event_id, owned_room_id, owned_user_id};
use serde_json::json;

fn timeline_json(
    sender: &str,
    content: serde_json::Value,
    relates_to: serde_json::Value,
) -> TimelineEvent {
    let mut content = content;
    if !relates_to.is_null() {
        content["m.relates_to"] = relates_to;
    }
    let raw = matrix_sdk::ruma::serde::Raw::new(&json!({
        "type": "m.room.message",
        "event_id": "$event:example.org",
        "sender": sender,
        "origin_server_ts": 1_700_000_000_000_u64,
        "content": content,
    }))
    .expect("test result")
    .cast_unchecked();
    TimelineEvent::from_plaintext(raw)
}

fn reaction_event_json(
    event_id: &str,
    sender: &str,
    target_event_id: &str,
    key: &str,
) -> TimelineEvent {
    let raw = matrix_sdk::ruma::serde::Raw::new(&json!({
        "type": "m.reaction",
        "event_id": event_id,
        "sender": sender,
        "origin_server_ts": 1_700_000_000_000_u64,
        "content": {
            "m.relates_to": {
                "rel_type": "m.annotation",
                "event_id": target_event_id,
                "key": key,
            }
        },
    }))
    .expect("test result")
    .cast_unchecked();
    TimelineEvent::from_plaintext(raw)
}

fn redaction_event_json(event_id: &str, sender: &str, redacts: &str) -> TimelineEvent {
    let raw = matrix_sdk::ruma::serde::Raw::new(&json!({
        "type": "m.room.redaction",
        "event_id": event_id,
        "sender": sender,
        "origin_server_ts": 1_700_000_000_001_u64,
        "redacts": redacts,
        "content": {},
    }))
    .expect("test result")
    .cast_unchecked();
    TimelineEvent::from_plaintext(raw)
}

#[tokio::test]
async fn maps_text_event() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = timeline_json(
        "@alice:example.org",
        json!({"msgtype": "m.text", "body": "hello"}),
        serde_json::Value::Null,
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
    assert_eq!(inbound.body, "hello");
    assert_eq!(inbound.message.thread_root, None);
}

#[tokio::test]
async fn maps_thread_root() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = timeline_json(
        "@alice:example.org",
        json!({"msgtype": "m.text", "body": "reply"}),
        json!({
            "rel_type": "m.thread",
            "event_id": "$root:example.org",
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
    assert_eq!(
        inbound.message.thread_root.as_deref(),
        Some("$root:example.org")
    );
}

#[tokio::test]
async fn skips_bot_self() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = timeline_json(
        "@bot:example.org",
        json!({"msgtype": "m.text", "body": "own"}),
        serde_json::Value::Null,
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
async fn maps_audio_event() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET)
            .path("/_matrix/client/v1/media/download/localhost/audio-id-1")
            .header("authorization", "Bearer test-token");
        then.status(200)
            .header("content-type", "audio/ogg")
            .body(minimal_ogg_bytes());
    });

    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = timeline_json(
        "@alice:example.org",
        json!({
            "msgtype": "m.audio",
            "body": "voice.ogg",
            "url": "mxc://localhost/audio-id-1",
        }),
        serde_json::Value::Null,
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

    assert_eq!(inbound.attachments.len(), 1);
    assert!(matches!(inbound.attachments[0], ContentBlock::Audio(_)));
}

#[tokio::test]
async fn rejects_oversize_audio() {
    let mut bytes = Vec::from(b"OggS");
    bytes.resize(26_000_000, 0);
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET)
            .path("/_matrix/client/v1/media/download/localhost/audio-id-oversize");
        then.status(200)
            .header("content-type", "audio/ogg")
            .body(bytes);
    });

    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = timeline_json(
        "@alice:example.org",
        json!({
            "msgtype": "m.audio",
            "body": "voice.ogg",
            "url": "mxc://localhost/audio-id-oversize",
        }),
        serde_json::Value::Null,
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

    assert!(inbound.attachments.is_empty());
    assert!(
        !inbound
            .attachments
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if text.contains("rejected")))
    );
}

#[test]
fn reaction_event_maps_to_inbound_reaction() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = reaction_event_json(
        "$react:example.org",
        "@alice:example.org",
        "$target:example.org",
        "👍",
    );
    let tracker = ReactionTracker::default();
    let r = timeline_event_to_inbound_reaction(&room, &event, &bot, &tracker).expect("mapped");
    assert_eq!(r.channel, "matrix");
    assert_eq!(r.conv.as_str(), "matrix:!room:example.org");
    assert_eq!(r.from.id.as_str(), "@alice:example.org");
    assert_eq!(r.emoji, "👍");
    assert!(r.added);
    assert_eq!(r.parent.id, "$target:example.org");
    assert!(r.parent.thread_root.is_none());
}

#[test]
fn oversized_reaction_key_is_byte_capped() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    // A hostile homeserver sends a multi-kilobyte key built from a
    // multi-byte scalar to also exercise char-boundary truncation.
    let huge_key = "🦀".repeat(2000);
    let event = reaction_event_json(
        "$flood:example.org",
        "@mallory:example.org",
        "$target:example.org",
        &huge_key,
    );
    let cache = ReactionTracker::default();
    let r = timeline_event_to_inbound_reaction(&room, &event, &bot, &cache).expect("mapped");
    // Both producers (reaction emoji + tracked key for later redaction)
    // are bounded because the cap runs once before recording.
    assert!(
        r.emoji.len() <= 256,
        "emoji must be capped to 256 bytes, got {}",
        r.emoji.len()
    );
    assert!(
        r.emoji.is_char_boundary(r.emoji.len()),
        "cap must land on a char boundary"
    );
    let entry = cache
        .take(&owned_event_id!("$flood:example.org"))
        .expect("recorded");
    assert!(
        entry.key.len() <= 256,
        "tracked key must be capped to 256 bytes, got {}",
        entry.key.len()
    );
}

#[test]
fn reaction_event_records_into_tracker() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = reaction_event_json(
        "$tracked:example.org",
        "@alice:example.org",
        "$target:example.org",
        "👍",
    );
    let cache = ReactionTracker::default();
    assert!(timeline_event_to_inbound_reaction(&room, &event, &bot, &cache).is_some());
    let entry = cache
        .take(&owned_event_id!("$tracked:example.org"))
        .expect("recorded");
    assert_eq!(entry.target_event_id.as_str(), "$target:example.org");
    assert_eq!(entry.key, "👍");
}

#[test]
fn reaction_event_from_bot_self_is_dropped() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = reaction_event_json(
        "$react:example.org",
        "@bot:example.org",
        "$target:example.org",
        "👍",
    );
    let tracker = ReactionTracker::default();
    assert!(timeline_event_to_inbound_reaction(&room, &event, &bot, &tracker).is_none());
}

#[test]
fn non_reaction_message_returns_none_from_reaction_mapper() {
    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = timeline_json(
        "@alice:example.org",
        json!({"msgtype": "m.text", "body": "hello"}),
        serde_json::Value::Null,
    );
    let tracker = ReactionTracker::default();
    assert!(timeline_event_to_inbound_reaction(&room, &event, &bot, &tracker).is_none());
}

#[test]
fn redaction_of_tracked_reaction_emits_added_false() {
    let bot = owned_user_id!("@bot:example.org");
    let tracker = ReactionTracker::default();
    tracker.record(
        owned_event_id!("$react:example.org"),
        TrackedReaction {
            target_event_id: owned_event_id!("$target:example.org"),
            key: "👍".to_owned(),
            sender: owned_user_id!("@alice:example.org"),
            room_id: owned_room_id!("!room:example.org"),
        },
    );
    let event = redaction_event_json(
        "$redaction:example.org",
        "@alice:example.org",
        "$react:example.org",
    );
    let r = timeline_redaction_to_inbound_reaction(&event, &bot, &tracker).expect("mapped");
    assert_eq!(r.channel, "matrix");
    assert_eq!(r.conv.as_str(), "matrix:!room:example.org");
    assert_eq!(r.from.id.as_str(), "@alice:example.org");
    assert_eq!(r.emoji, "👍");
    assert!(!r.added);
    assert_eq!(r.parent.id, "$target:example.org");
}

#[test]
fn redaction_of_unknown_event_returns_none() {
    let bot = owned_user_id!("@bot:example.org");
    let tracker = ReactionTracker::default();
    let event = redaction_event_json(
        "$redaction:example.org",
        "@alice:example.org",
        "$never_seen:example.org",
    );
    assert!(timeline_redaction_to_inbound_reaction(&event, &bot, &tracker).is_none());
}

#[test]
fn redaction_from_bot_self_is_dropped() {
    let bot = owned_user_id!("@bot:example.org");
    let tracker = ReactionTracker::default();
    tracker.record(
        owned_event_id!("$react:example.org"),
        TrackedReaction {
            target_event_id: owned_event_id!("$target:example.org"),
            key: "👍".to_owned(),
            sender: owned_user_id!("@bot:example.org"),
            room_id: owned_room_id!("!room:example.org"),
        },
    );
    let event = redaction_event_json(
        "$redaction:example.org",
        "@bot:example.org",
        "$react:example.org",
    );
    assert!(timeline_redaction_to_inbound_reaction(&event, &bot, &tracker).is_none());
}

#[test]
fn redaction_with_redacts_in_content_field_works() {
    let bot = owned_user_id!("@bot:example.org");
    let tracker = ReactionTracker::default();
    tracker.record(
        owned_event_id!("$react:example.org"),
        TrackedReaction {
            target_event_id: owned_event_id!("$target:example.org"),
            key: "👍".to_owned(),
            sender: owned_user_id!("@alice:example.org"),
            room_id: owned_room_id!("!room:example.org"),
        },
    );
    // v11 room redaction places `redacts` inside `content`.
    let raw = matrix_sdk::ruma::serde::Raw::new(&json!({
        "type": "m.room.redaction",
        "event_id": "$redaction:example.org",
        "sender": "@alice:example.org",
        "origin_server_ts": 1_700_000_000_001_u64,
        "content": {"redacts": "$react:example.org"},
    }))
    .expect("test result")
    .cast_unchecked();
    let event = TimelineEvent::from_plaintext(raw);
    let r = timeline_redaction_to_inbound_reaction(&event, &bot, &tracker).expect("mapped");
    assert!(!r.added);
    assert_eq!(r.emoji, "👍");
}

#[test]
fn non_redaction_event_returns_none_from_redaction_mapper() {
    let bot = owned_user_id!("@bot:example.org");
    let tracker = ReactionTracker::default();
    let event = timeline_json(
        "@alice:example.org",
        json!({"msgtype": "m.text", "body": "hello"}),
        serde_json::Value::Null,
    );
    assert!(timeline_redaction_to_inbound_reaction(&event, &bot, &tracker).is_none());
}

#[tokio::test]
async fn rejects_disallowed_mime() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET)
            .path("/_matrix/client/v1/media/download/localhost/audio-id-midi");
        then.status(200)
            .header("content-type", "audio/midi")
            .body(minimal_ogg_bytes());
    });

    let room = owned_room_id!("!room:example.org");
    let bot = owned_user_id!("@bot:example.org");
    let event = timeline_json(
        "@alice:example.org",
        json!({
            "msgtype": "m.audio",
            "body": "voice.ogg",
            "url": "mxc://localhost/audio-id-midi",
        }),
        serde_json::Value::Null,
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

    assert!(inbound.attachments.is_empty());
    assert!(
        !inbound
            .attachments
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { text } if text.contains("rejected")))
    );
}
