#[path = "support/mod.rs"]
mod support;

use matrix_sdk::{
    attachment::AttachmentConfig,
    ruma::{
        OwnedMxcUri,
        events::{room::MediaSource, room::message::MessageType},
    },
};
use mime::IMAGE_PNG;

async fn send_image(
    fixture: &support::JoinedRoomFixture,
    body: &str,
    bytes: Vec<u8>,
) -> support::TestResult<OwnedMxcUri> {
    let room = fixture
        .alice
        .get_room(&fixture.room_id)
        .ok_or("alice not in room")?;
    room.send_attachment(body, &IMAGE_PNG, bytes, AttachmentConfig::new())
        .await?;

    let event = support::wait_for_room_message_matching(&fixture.bot, &fixture.room_id, |event| {
        matches!(event.content.msgtype, MessageType::Image(_))
    })
    .await?;

    match event.content.msgtype {
        MessageType::Image(content) => match content.source {
            MediaSource::Plain(source) => Ok(source),
            MediaSource::Encrypted(_) => Err("image source is encrypted".into()),
        },
        _ => Err("event is not image".into()),
    }
}

fn png_bytes() -> Vec<u8> {
    let mut png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(
        b"\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00",
    );
    png
}

#[tokio::test]
async fn matrix_image_download_happy_path() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };

    let source = send_image(&fixture, "happy.png", png_bytes())
        .await
        .expect("matrix image fixture should send");
    let result = crabgent_channel_matrix::image_download::download_matrix_image(
        &reqwest::Client::new(),
        &fixture.bot,
        &source,
        fixture.bot.access_token().as_deref(),
    )
    .await;

    let (bytes, mime) = result.expect("matrix image download");
    assert_eq!(bytes, png_bytes().as_slice());
    assert_eq!(mime, "image/png");
}

#[tokio::test]
async fn matrix_image_download_auth_failure_no_token_leak() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };

    let source = send_image(&fixture, "auth.png", png_bytes())
        .await
        .expect("matrix auth-failure image fixture should send");
    let result = crabgent_channel_matrix::image_download::download_matrix_image(
        &reqwest::Client::new(),
        &fixture.bot,
        &source,
        Some("wrong-secret-token"),
    )
    .await;

    let err = result.expect_err("auth failed");
    assert_eq!(format!("{err}"), "authentication failed");
    assert!(!format!("{err}").contains("wrong-secret-token"));
}

#[tokio::test]
async fn matrix_image_size_limit_graceful_fallback() {
    let Some(fixture) = support::joined_room(2)
        .await
        .expect("joined room fixture should initialize")
    else {
        return;
    };

    let huge = vec![0u8; 6_000_000];
    let source = send_image(&fixture, "large.png", huge)
        .await
        .expect("large matrix image fixture should send");

    let result = crabgent_channel_matrix::image_download::download_matrix_image(
        &reqwest::Client::new(),
        &fixture.bot,
        &source,
        fixture.bot.access_token().as_deref(),
    )
    .await;

    assert!(matches!(
        result,
        Err(crabgent_channel_matrix::image_download::ImageDownloadError::Size)
    ));
}
