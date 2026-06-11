use crabgent_channel::image_store::file_system::{
    FileSystemImageStore, FileSystemImageStoreConfig,
};
use crabgent_channel::{AudioValidator, ImageValidator, MAX_AUDIO_BYTES};
use crabgent_channel_slack::events::{SlackEvent, SlackFileMetadata};
use crabgent_channel_slack::ids::SlackWorkspaceId;
use crabgent_channel_slack::inbound::audio::build_audio_attachment;
use crabgent_channel_slack::inbound::{
    new_channel_kind_cache, new_channel_type_cache, slack_event_to_inbound_with_channel_type_cache,
};
use crabgent_core::message::{AudioPayload, ContentBlock};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_TOKEN: &str = "secret-test-token-12345";

#[tokio::test]
async fn audio_mpeg_inbound_produces_audio_block() {
    let server = MockServer::start().await;
    let bytes = audio_mpeg_bytes(100_000);
    mount_audio_download(&server, "/download/audio.mp3", bytes.clone(), 1).await;

    let inbound = run_audio_inbound(slack_file_event(
        "F123",
        "audio/mpeg",
        100_000,
        &format!("{}/download/audio.mp3", server.uri()),
    ))
    .await;

    let payload =
        audio_payload(&inbound.attachments).expect("expected one audio attachment from Slack file");
    assert_eq!(payload.bytes().as_ref(), bytes.as_slice());
    assert_eq!(payload.mime(), "audio/mpeg");
    assert_eq!(payload.filename.as_deref(), Some("F123"));
}

#[tokio::test]
async fn audio_too_large_pre_check() {
    let server = MockServer::start().await;
    mount_audio_download(&server, "/download/too-large.mp3", audio_mpeg_bytes(16), 0).await;

    let inbound = run_audio_inbound(slack_file_event(
        "F124",
        "audio/mpeg",
        26_000_000,
        &format!("{}/download/too-large.mp3", server.uri()),
    ))
    .await;

    assert_audio_rejected(&inbound.attachments);
}

#[tokio::test]
async fn audio_download_rejects_oversize_content_length() {
    let server = MockServer::start().await;
    mount_audio_download(
        &server,
        "/download/content-length-too-large.mp3",
        audio_mpeg_bytes(usize::try_from(MAX_AUDIO_BYTES + 1).expect("audio cap fits usize")),
        1,
    )
    .await;

    let inbound = run_audio_inbound(slack_file_event(
        "F124B",
        "audio/mpeg",
        16,
        &format!("{}/download/content-length-too-large.mp3", server.uri()),
    ))
    .await;

    assert_audio_rejected(&inbound.attachments);
}

#[tokio::test]
async fn audio_invalid_mime_produces_text_fallback() {
    let server = MockServer::start().await;
    mount_audio_download(&server, "/download/audio.bin", audio_mpeg_bytes(64), 1).await;

    let inbound = run_audio_inbound(slack_file_event(
        "F125",
        "audio/exotic",
        64,
        &format!("{}/download/audio.bin", server.uri()),
    ))
    .await;

    assert_audio_rejected(&inbound.attachments);
}

#[tokio::test]
async fn audio_rejected_via_validator() {
    let server = MockServer::start().await;
    mount_audio_download(&server, "/download/not-audio.mp3", png_bytes(), 1).await;

    let inbound = run_audio_inbound(slack_file_event(
        "F126",
        "audio/mpeg",
        16,
        &format!("{}/download/not-audio.mp3", server.uri()),
    ))
    .await;

    assert_audio_rejected(&inbound.attachments);
}

#[tokio::test]
async fn audio_auth_failure_no_token_leak() {
    let server = MockServer::start().await;
    mount_audio_auth_failure(&server, "/download/auth-fail.mp3").await;

    let inbound = run_audio_inbound(slack_file_event(
        "F127",
        "audio/mpeg",
        64,
        &format!("{}/download/auth-fail.mp3", server.uri()),
    ))
    .await;

    let text = assert_audio_rejected_text(&inbound.attachments);
    assert!(
        !text.contains(TEST_TOKEN),
        "audio fallback leaked Slack token: {text}"
    );
}

#[tokio::test]
async fn audio_missing_url_private_produces_text_fallback() {
    let client = reqwest::Client::new();
    let audio_validator = AudioValidator::new();
    let file_metadata = SlackFileMetadata {
        id: "F128".to_owned(),
        mimetype: Some("audio/mpeg".to_owned()),
        url_private: None,
        url_private_download: None,
        size: Some(64),
    };

    let block = build_audio_attachment(
        &client,
        TEST_TOKEN,
        &audio_validator,
        &file_metadata,
        "audio/mpeg",
    )
    .await;

    let text = text_fallback(&block).expect("expected text fallback for missing url_private");
    assert!(
        text.starts_with("[Audio rejected: "),
        "unexpected audio fallback text: {text}"
    );
}

#[tokio::test]
async fn file_shared_event_is_ignored_by_inbound() {
    let inbound = try_audio_inbound(slack_file_shared_event_without_meta("F130")).await;

    assert!(
        inbound.is_none(),
        "file_shared events must wait for the message file_share event"
    );
}

async fn mount_audio_download(server: &MockServer, url_path: &str, body: Vec<u8>, calls: u64) {
    Mock::given(method("GET"))
        .and(path(url_path))
        .and(header("Authorization", format!("Bearer {TEST_TOKEN}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body)
                .insert_header("content-type", "audio/mpeg"),
        )
        .expect(calls)
        .mount(server)
        .await;
}

async fn mount_audio_auth_failure(server: &MockServer, url_path: &str) {
    Mock::given(method("GET"))
        .and(path(url_path))
        .and(header("Authorization", format!("Bearer {TEST_TOKEN}")))
        .respond_with(ResponseTemplate::new(401).set_body_bytes(TEST_TOKEN.as_bytes().to_vec()))
        .expect(1)
        .mount(server)
        .await;
}

async fn run_audio_inbound(event: SlackEvent) -> crabgent_channel::InboundEvent {
    try_audio_inbound(event).await.expect("inbound")
}

async fn try_audio_inbound(event: SlackEvent) -> Option<crabgent_channel::InboundEvent> {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
        cache_root: dir.path().to_owned(),
    });
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let client = reqwest::Client::new();
    let token = SecretString::new(TEST_TOKEN.into());
    let image_validator = ImageValidator::new();
    let audio_validator = AudioValidator::new();

    slack_event_to_inbound_with_channel_type_cache(
        &event,
        &workspace,
        &new_channel_kind_cache(),
        &new_channel_type_cache(),
        None,
        &client,
        &token,
        &store,
        &image_validator,
        &audio_validator,
    )
    .await
}

fn slack_file_event(file_id: &str, mime: &str, size: i64, url: &str) -> SlackEvent {
    serde_json::from_value(json!({
        "type": "message",
        "channel": "C123",
        "user": "U123",
        "text": "",
        "ts": "1.1",
        "subtype": "file_share",
        "channel_type": "channel",
        "files": [
            {
                "id": file_id,
                "mimetype": mime,
                "url_private": url,
                "size": size
            }
        ]
    }))
    .expect("event")
}

fn slack_file_shared_event_without_meta(file_id: &str) -> SlackEvent {
    serde_json::from_value(json!({
        "type": "file_shared",
        "channel_id": "C123",
        "file_id": file_id,
        "user_id": "U123",
        "event_ts": "1.1"
    }))
    .expect("event")
}

fn audio_mpeg_bytes(len: usize) -> Vec<u8> {
    let mut bytes = b"ID3".to_vec();
    bytes.resize(len, 0);
    bytes
}

fn png_bytes() -> Vec<u8> {
    vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
}

fn assert_audio_rejected(attachments: &[ContentBlock]) {
    let _ = assert_audio_rejected_text(attachments);
}

fn assert_audio_rejected_text(attachments: &[ContentBlock]) -> &str {
    let text = audio_rejection_text(attachments).expect("expected one text fallback attachment");
    assert!(
        text.starts_with("[Audio rejected: "),
        "unexpected audio fallback text: {text}"
    );
    text
}

fn audio_payload(attachments: &[ContentBlock]) -> Option<&AudioPayload> {
    let [ContentBlock::Audio(payload)] = attachments else {
        return None;
    };
    Some(payload)
}

fn audio_rejection_text(attachments: &[ContentBlock]) -> Option<&str> {
    let [ContentBlock::Text { text }] = attachments else {
        return None;
    };
    Some(text)
}

fn text_fallback(block: &ContentBlock) -> Option<&str> {
    let ContentBlock::Text { text } = block else {
        return None;
    };
    Some(text)
}
