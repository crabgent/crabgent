use super::*;
use crabgent_channel::image_store::file_system::{
    FileSystemImageStore, FileSystemImageStoreConfig,
};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_token() -> SecretString {
    SecretString::new("secret-test-token-12345".into())
}

fn minimal_png_bytes() -> Vec<u8> {
    let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(
        b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00",
    );
    png
}

#[tokio::test]
async fn message_file_share_with_image_attaches_and_threads() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/image.png"))
        .and(header("Authorization", "Bearer secret-test-token-12345"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(minimal_png_bytes())
                .insert_header("content-type", "image/png"),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
        cache_root: dir.path().to_owned(),
    });
    let validator = ImageValidator::new();
    let audio_validator = AudioValidator::new();
    let client = reqwest::Client::new();
    let token = test_token();
    let url = format!("{}/download/image.png", server.uri());

    let message = file_share_message(
        Some("thread text"),
        Some("1234.5678"),
        Some(vec![SlackFileMetadata {
            id: "F123".to_owned(),
            mimetype: Some("image/png".to_owned()),
            url_private: Some(url),
            url_private_download: None,
            size: None,
        }]),
    );
    let services = AttachmentServices {
        client: &client,
        token: &token,
        store: &store,
        image_validator: &validator,
        audio_validator: &audio_validator,
    };

    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let result = message_to_inbound(
        &message,
        &workspace,
        &new_channel_kind_cache(),
        &new_channel_type_cache(),
        None,
        &services,
    )
    .await;

    let inbound = result.expect("inbound event");
    assert_eq!(inbound.attachments.len(), 1, "attachments empty");
    assert!(
        matches!(&inbound.attachments[0], ContentBlock::Image(_)),
        "expected ContentBlock::Image, got {:?}",
        inbound.attachments[0]
    );
    assert_eq!(inbound.message.id, "1111.2222");
    assert_eq!(inbound.message.thread_root(), Some("1234.5678"));
}

#[tokio::test]
async fn message_file_share_with_audio_attaches() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/download/audio.mp3"))
        .and(header("Authorization", "Bearer secret-test-token-12345"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(audio_mpeg_bytes())
                .insert_header("content-type", "audio/mpeg"),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
        cache_root: dir.path().to_owned(),
    });
    let validator = ImageValidator::new();
    let audio_validator = AudioValidator::new();
    let client = reqwest::Client::new();
    let token = test_token();
    let url = format!("{}/download/audio.mp3", server.uri());

    let message = file_share_message(
        Some("audio"),
        None,
        Some(vec![SlackFileMetadata {
            id: "F124".to_owned(),
            mimetype: Some("audio/mpeg".to_owned()),
            url_private: None,
            url_private_download: Some(url),
            size: Some(64),
        }]),
    );
    let services = AttachmentServices {
        client: &client,
        token: &token,
        store: &store,
        image_validator: &validator,
        audio_validator: &audio_validator,
    };

    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let result = message_to_inbound(
        &message,
        &workspace,
        &new_channel_kind_cache(),
        &new_channel_type_cache(),
        None,
        &services,
    )
    .await;

    let inbound = result.expect("inbound event");
    let [ContentBlock::Audio(payload)] = inbound.attachments.as_slice() else {
        panic!(
            "expected one audio attachment, got {:?}",
            inbound.attachments
        );
    };
    assert_eq!(payload.mime(), "audio/mpeg");
    assert_eq!(payload.filename.as_deref(), Some("F124"));
}

#[tokio::test]
async fn message_file_share_without_files_has_no_attachments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
        cache_root: dir.path().to_owned(),
    });
    let validator = ImageValidator::new();
    let audio_validator = AudioValidator::new();
    let client = reqwest::Client::new();
    let token = test_token();
    let services = AttachmentServices {
        client: &client,
        token: &token,
        store: &store,
        image_validator: &validator,
        audio_validator: &audio_validator,
    };
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");

    for files in [None, Some(vec![])] {
        let message = file_share_message(Some("file share"), None, files);
        let inbound = message_to_inbound(
            &message,
            &workspace,
            &new_channel_kind_cache(),
            &new_channel_type_cache(),
            None,
            &services,
        )
        .await
        .expect("inbound event");

        assert!(inbound.attachments.is_empty());
    }
}

#[tokio::test]
async fn build_image_attachment_maps_validation_failure_to_text_fallback() {
    let server = MockServer::start().await;
    let png = minimal_png_bytes();
    Mock::given(method("GET"))
        .and(path("/download/image.png"))
        .and(header("Authorization", "Bearer secret-test-token-12345"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(png)
                .insert_header("content-type", "image/gif"),
        )
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
        cache_root: dir.path().to_owned(),
    });
    let validator = ImageValidator::new();
    let client = reqwest::Client::new();
    let url = format!("{}/download/image.png", server.uri());

    let block = build_image_attachment(
        &client,
        &test_token(),
        &store,
        &validator,
        &url,
        "image/gif",
    )
    .await;

    let ContentBlock::Text { text } = block else {
        panic!("expected image fallback text");
    };
    assert_eq!(
        text,
        "[image rejected: image bytes not recognized as a valid format]"
    );
}

fn file_share_message(
    text: Option<&str>,
    thread_ts: Option<&str>,
    files: Option<Vec<SlackFileMetadata>>,
) -> SlackMessageEvent {
    SlackMessageEvent {
        channel: "C123".to_owned(),
        user: Some("U123".to_owned()),
        bot_id: None,
        text: text.map(str::to_owned),
        ts: "1111.2222".to_owned(),
        thread_ts: thread_ts.map(str::to_owned),
        channel_type: Some("channel".to_owned()),
        team_id: None,
        subtype: Some("file_share".to_owned()),
        files,
    }
}

fn audio_mpeg_bytes() -> Vec<u8> {
    let mut bytes = b"ID3".to_vec();
    bytes.resize(64, 0);
    bytes
}

#[test]
fn slack_build_message_event_strips_control_chars() {
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let event = build_message_event(
        &workspace,
        "C123",
        "U123",
        "a\u{0000}b\u{200B}c\u{202E}d",
        "1111.2222",
        None,
        ChannelKind::Group,
        ParticipantRole::Human,
        vec![],
    )
    .expect("inbound event");
    assert_eq!(event.body, "abcd");
}

#[test]
fn slack_build_message_event_xml_escapes_in_body() {
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let event = build_message_event(
        &workspace,
        "C123",
        "U123",
        "<script>alert(1)</script>",
        "1111.2222",
        None,
        ChannelKind::Group,
        ParticipantRole::Human,
        vec![],
    )
    .expect("inbound event");
    assert_eq!(event.body, "&lt;script&gt;alert(1)&lt;/script&gt;");
}

#[test]
fn slack_build_message_event_oversize_returns_none() {
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let big = "a".repeat(9000);
    let result = build_message_event(
        &workspace,
        "C123",
        "U123",
        &big,
        "1111.2222",
        None,
        ChannelKind::Group,
        ParticipantRole::Human,
        vec![],
    );
    assert!(result.is_none(), "oversize inbound text must yield None");
}

#[test]
fn parse_slack_ts_message_event_carries_platform_timestamp() {
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let event = build_message_event(
        &workspace,
        "C123",
        "U123",
        "hello",
        "1716312345.678901",
        None,
        ChannelKind::Group,
        ParticipantRole::Human,
        vec![],
    )
    .expect("event");
    assert_eq!(event.timestamp.timestamp(), 1_716_312_345);
    assert_eq!(event.timestamp.timestamp_subsec_micros(), 678_901);
}

#[test]
fn reaction_carries_platform_timestamp_not_now() {
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let event = SlackEvent::ReactionAdded(crate::events::SlackReactionEvent {
        reaction: "thumbsup".to_owned(),
        user: Some("U123".to_owned()),
        item: crate::events::SlackReactionItem {
            channel: "C123".to_owned(),
            ts: "1716312345.678901".to_owned(),
        },
        team_id: None,
    });
    let reaction =
        slack_event_to_inbound_reaction(&event, &workspace, &new_channel_kind_cache(), None)
            .expect("reaction");
    // Startup-cutoff filtering needs the reacted-to message's Slack ts, not
    // Utc::now(), so replayed reactions on reconnect read as past events.
    assert_eq!(reaction.timestamp.timestamp(), 1_716_312_345);
    assert_eq!(reaction.timestamp.timestamp_subsec_micros(), 678_901);
}

#[test]
fn assistant_thread_without_user_returns_none() {
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let thread = SlackAssistantThreadEvent {
        channel: "C123".to_owned(),
        thread_ts: "1716312345.678901".to_owned(),
        user: None,
        team_id: None,
    };
    let result = assistant_thread_to_inbound(&thread, &workspace, &new_channel_kind_cache());
    assert!(
        result.is_none(),
        "userless assistant-thread events must not collapse to slack:unknown"
    );
}

#[test]
fn assistant_thread_with_user_uses_user_subject() {
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let thread = SlackAssistantThreadEvent {
        channel: "C123".to_owned(),
        thread_ts: "1716312345.678901".to_owned(),
        user: Some("U123".to_owned()),
        team_id: None,
    };
    let event =
        assistant_thread_to_inbound(&thread, &workspace, &new_channel_kind_cache()).expect("event");
    assert_eq!(event.from.id.as_str(), "U123");
}
