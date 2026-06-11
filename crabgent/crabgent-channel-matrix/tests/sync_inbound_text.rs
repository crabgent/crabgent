#[path = "support/mod.rs"]
mod support;

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use crabgent_channel::{
    ChannelInbox, ImageRef, ImageStore, ImageStoreError, ImageValidator, InboundEvent,
};
use crabgent_channel_matrix::{MatrixChannel, MatrixSyncPoller};
use crabgent_core::message::ContentBlock;
use matrix_sdk::attachment::AttachmentConfig;
use mime::IMAGE_PNG;
use tokio::{
    sync::{Mutex, mpsc},
    time::timeout,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn sync_poller_delivers_inbound_text() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };
    support::send_from(&fixture.alice, &fixture.room_id, "inbound text")
        .await
        .expect("inbound text fixture should send");
    let cancel = CancellationToken::new();
    let event = support::collect_one_inbound(fixture.channel, cancel)
        .await
        .expect("inbound text event should be collected");
    assert_eq!(event.body, "inbound text");
    assert_eq!(event.message.thread_root, None);
    assert_eq!(event.conv.as_str(), format!("matrix:{}", fixture.room_id));
}

#[tokio::test]
async fn sync_poller_delivers_inbound_image() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };

    send_image(&fixture, "vision", png_bytes())
        .await
        .expect("matrix image fixture should send");

    let store: Arc<dyn ImageStore> = Arc::new(TestImageStore::default()) as _;
    let validator = ImageValidator::new();
    let cancel = CancellationToken::new();
    let (event, handle) =
        collect_one_inbound_with_image_support(fixture.channel, store, validator, cancel.clone())
            .await
            .expect("image inbound event should be collected");
    cancel.cancel();
    handle
        .await
        .expect("matrix image poller task should join")
        .expect("matrix image poller should stop cleanly");

    assert_eq!(event.body, "vision");
    assert_eq!(event.attachments.len(), 1, "expected one attachment block");
    let payload = image_payload(&event.attachments).expect("expected one image block");
    assert_eq!(payload.mime(), "image/png");
    assert!(
        !payload.bytes().is_empty(),
        "vision payload should contain non-empty bytes",
    );
}

#[tokio::test]
async fn sync_poller_maps_invalid_image_attachment_to_text_fallback() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };

    send_image(&fixture, "bad vision", b"not an image".to_vec())
        .await
        .expect("invalid matrix image fixture should send");

    let store: Arc<dyn ImageStore> = Arc::new(TestImageStore::default()) as _;
    let validator = ImageValidator::new();
    let cancel = CancellationToken::new();
    let (event, handle) =
        collect_one_inbound_with_image_support(fixture.channel, store, validator, cancel.clone())
            .await
            .expect("invalid image inbound event should be collected");
    cancel.cancel();
    handle
        .await
        .expect("matrix invalid-image poller task should join")
        .expect("matrix invalid-image poller should stop cleanly");

    assert_eq!(event.body, "bad vision");
    let [ContentBlock::Text { text }] = event.attachments.as_slice() else {
        panic!(
            "invalid image bytes should produce one text fallback, got {:?}",
            event.attachments
        );
    };
    assert_eq!(
        text,
        "[image rejected: image bytes not recognized as a valid format]"
    );
}

async fn send_image(
    fixture: &support::JoinedRoomFixture,
    body: &str,
    bytes: Vec<u8>,
) -> support::TestResult {
    let room = fixture
        .alice
        .get_room(&fixture.room_id)
        .ok_or("alice not in room")?;
    room.send_attachment(body, &IMAGE_PNG, bytes, AttachmentConfig::new())
        .await?;
    Ok(())
}

fn png_bytes() -> Vec<u8> {
    let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(
        b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00",
    );
    png
}

fn image_payload(attachments: &[ContentBlock]) -> Option<&crabgent_core::message::ImagePayload> {
    let [ContentBlock::Image(payload)] = attachments else {
        return None;
    };
    Some(payload)
}

#[derive(Default)]
struct TestImageStore {
    next_id: Mutex<u64>,
}

#[async_trait]
impl ImageStore for TestImageStore {
    async fn put(&self, _bytes: bytes::Bytes, _mime: &str) -> Result<ImageRef, ImageStoreError> {
        let mut next_id = self.next_id.lock().await;
        let id = *next_id;
        *next_id += 1;
        Ok(ImageRef::new(id.to_string()))
    }

    async fn get(&self, _image_ref: &ImageRef) -> Result<(bytes::Bytes, String), ImageStoreError> {
        Err(ImageStoreError::NotFound)
    }
}

struct RecordingInbox {
    tx: Mutex<mpsc::Sender<InboundEvent>>,
}

impl RecordingInbox {
    fn new(tx: mpsc::Sender<InboundEvent>) -> Self {
        Self { tx: Mutex::new(tx) }
    }
}

#[async_trait]
impl ChannelInbox for RecordingInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), crabgent_channel::ChannelError> {
        self.tx
            .lock()
            .await
            .send(event)
            .await
            .map_err(crabgent_channel::ChannelError::adapter)
    }
}

async fn collect_one_inbound_with_image_support(
    channel: Arc<MatrixChannel>,
    image_store: Arc<dyn ImageStore>,
    image_validator: ImageValidator,
    cancel: CancellationToken,
) -> support::TestResult<(
    InboundEvent,
    tokio::task::JoinHandle<Result<(), crabgent_channel::ChannelError>>,
)> {
    let (tx, mut rx) = mpsc::channel(4);
    let _ = &mut rx;
    let poller = MatrixSyncPoller::new(channel, Arc::new(RecordingInbox::new(tx)))
        .with_sync_timeout(Duration::from_millis(500))
        .with_image_support(reqwest::Client::new(), image_store, image_validator);
    let handle = poller.start(cancel.clone());

    let event = timeout(Duration::from_secs(10), rx.recv())
        .await?
        .ok_or("matrix poller did not produce inbound event")?;

    Ok((event, handle))
}
