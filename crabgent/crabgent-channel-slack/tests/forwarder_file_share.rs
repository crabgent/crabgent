use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel::image_store::file_system::{
    FileSystemImageStore, FileSystemImageStoreConfig,
};
use crabgent_channel::{
    AudioValidator, ChannelError, ChannelInbox, ImageStore, ImageValidator, InboundEvent,
};
use crabgent_channel_slack::dispatch::{KernelInboundForwarder, SlackEventListener, SlackSelfIds};
use crabgent_channel_slack::events::SlackEvent;
use crabgent_channel_slack::ids::SlackWorkspaceId;
use crabgent_channel_slack::inbound::{new_channel_kind_cache, new_channel_type_cache};
use crabgent_core::ContentBlock;
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_TOKEN: &str = "secret-test-token-12345";

#[tokio::test]
async fn forwarder_file_share_sends_mixed_media_once() {
    let server = MockServer::start().await;
    mount_download(&server, "/image.png", minimal_png_bytes(), "image/png").await;
    mount_download(&server, "/audio.mp3", audio_mpeg_bytes(), "audio/mpeg").await;
    let inbox = Arc::new(RecordingInbox::default());
    let forwarder = forwarder(Arc::clone(&inbox) as Arc<dyn ChannelInbox>);

    forwarder
        .on_event(mixed_file_share_event(&server))
        .await
        .expect("mixed file share forwards");

    let events = inbox.events.lock().expect("recorded events");
    assert_eq!(events.len(), 1);
    let attachments = &events[0].attachments;
    assert_eq!(image_count(attachments), 1);
    assert_eq!(audio_count(attachments), 1);
    assert_eq!(text_count(attachments), 0);
}

#[tokio::test]
async fn forwarder_media_download_does_not_follow_redirect() {
    let server = MockServer::start().await;
    // `url_private` answers with a 302 to a secondary endpoint. A client
    // that follows redirects would re-attach the bot token to the
    // redirect target (SSRF credential leak). The hardened client must
    // not follow it: the redirect target stays untouched and the image
    // attachment collapses to a text fallback.
    Mock::given(method("GET"))
        .and(path("/private/image.png"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/leak", server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/leak"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(minimal_png_bytes())
                .insert_header("content-type", "image/png"),
        )
        .expect(0)
        .mount(&server)
        .await;
    let inbox = Arc::new(RecordingInbox::default());
    let forwarder = forwarder(Arc::clone(&inbox) as Arc<dyn ChannelInbox>);

    forwarder
        .on_event(redirect_image_event(&server))
        .await
        .expect("redirected file share forwards");

    let events = inbox.events.lock().expect("recorded events");
    assert_eq!(events.len(), 1);
    let attachments = &events[0].attachments;
    // Download was refused at the redirect, so no image materialized; the
    // attachment falls back to a text block instead.
    assert_eq!(image_count(attachments), 0);
    assert_eq!(text_count(attachments), 1);
    // `.expect(0)` on the redirect target is verified on `server` drop.
}

fn redirect_image_event(server: &MockServer) -> SlackEvent {
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
                "id": "FIMG",
                "mimetype": "image/png",
                "url_private": format!("{}/private/image.png", server.uri()),
                "size": 32
            }
        ]
    }))
    .expect("redirect image event")
}

fn forwarder(inbox: Arc<dyn ChannelInbox>) -> KernelInboundForwarder {
    let dir = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn ImageStore> =
        Arc::new(FileSystemImageStore::new(FileSystemImageStoreConfig {
            cache_root: dir.keep(),
        }));
    KernelInboundForwarder::with_hardened_client(
        inbox,
        SlackWorkspaceId::new("T123").expect("workspace"),
        new_channel_kind_cache(),
        new_channel_type_cache(),
        SlackSelfIds::default(),
        Duration::from_secs(30),
        SecretString::new(TEST_TOKEN.into()),
        store,
        ImageValidator::new(),
        AudioValidator::new(),
    )
    .expect("hardened media client builds")
}

async fn mount_download(server: &MockServer, url_path: &str, body: Vec<u8>, mime: &str) {
    Mock::given(method("GET"))
        .and(path(url_path))
        .and(header("Authorization", format!("Bearer {TEST_TOKEN}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(body)
                .insert_header("content-type", mime),
        )
        .expect(1)
        .mount(server)
        .await;
}

fn mixed_file_share_event(server: &MockServer) -> SlackEvent {
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
                "id": "FIMG",
                "mimetype": "image/png",
                "url_private": format!("{}/image.png", server.uri()),
                "size": 32
            },
            {
                "id": "FAUD",
                "mimetype": "audio/mpeg",
                "url_private_download": format!("{}/audio.mp3", server.uri()),
                "size": 64
            },
            {
                "id": "FTXT",
                "mimetype": "text/plain",
                "size": 0
            }
        ]
    }))
    .expect("file share event")
}

fn minimal_png_bytes() -> Vec<u8> {
    let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(
        b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00",
    );
    png
}

fn audio_mpeg_bytes() -> Vec<u8> {
    let mut bytes = b"ID3".to_vec();
    bytes.resize(64, 0);
    bytes
}

fn image_count(attachments: &[ContentBlock]) -> usize {
    attachments
        .iter()
        .filter(|block| matches!(block, ContentBlock::Image(_)))
        .count()
}

fn audio_count(attachments: &[ContentBlock]) -> usize {
    attachments
        .iter()
        .filter(|block| matches!(block, ContentBlock::Audio(_)))
        .count()
}

fn text_count(attachments: &[ContentBlock]) -> usize {
    attachments
        .iter()
        .filter(|block| matches!(block, ContentBlock::Text { .. }))
        .count()
}

#[derive(Default)]
struct RecordingInbox {
    events: Mutex<Vec<InboundEvent>>,
}

#[async_trait]
impl ChannelInbox for RecordingInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        self.events.lock().expect("recorded events").push(event);
        Ok(())
    }
}
