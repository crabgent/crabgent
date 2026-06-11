use crabgent_channel::{Channel, MessageRef};
use crabgent_channel_matrix::MatrixChannel;
use crabgent_core::{owner::Owner, subject::Subject};
use httpmock::{Method::GET, Method::POST, Method::PUT, MockServer};
use matrix_sdk::{
    Client, SessionMeta, SessionTokens, authentication::matrix::MatrixSession,
    config::SyncSettings, ruma::OwnedDeviceId, ruma::owned_user_id,
};
use serde_json::{Value, json};
use url::Url;

const ROOM_ID: &str = "!room:localhost";
#[tokio::test]
async fn channel_edit_sends_replacement_event() {
    let server = MockServer::start();
    let channel = build_channel(&server).await;
    let conv = matrix_conv();
    let target = MessageRef::top_level("matrix", conv.clone(), "$target:localhost");
    let send_mock = server.mock(|when, then| {
        when.method(PUT)
            .path_matches(r"^/_matrix/client/(r0|v3)/rooms/[^/]+/send/m\.room\.message/[^/]+$")
            .body_matches(r#""rel_type"\s*:\s*"m.replace""#)
            .body_matches(r#""event_id"\s*:\s*"\$target:localhost""#)
            .body_matches("updated text");
        then.status(200)
            .json_body(json!({"event_id": "$edit:localhost"}));
    });

    channel
        .edit(&Subject::new("agent"), &conv, &target, "updated text")
        .await
        .expect("edit");

    send_mock.assert();
}

#[tokio::test]
async fn channel_delete_redacts_target_event() {
    let server = MockServer::start();
    let channel = build_channel(&server).await;
    let conv = matrix_conv();
    let target = MessageRef::top_level("matrix", conv.clone(), "$target:localhost");
    let redact = server.mock(|when, then| {
        when.method(PUT)
            .path_matches(r"^/_matrix/client/(r0|v3)/rooms/[^/]+/redact/[^/]+/[^/]+$");
        then.status(200)
            .json_body(json!({"event_id": "$redact:localhost"}));
    });

    channel
        .delete(&Subject::new("agent"), &conv, &target)
        .await
        .expect("delete");

    redact.assert();
}

#[tokio::test]
async fn channel_upload_posts_media_then_sends_file_message() {
    let server = MockServer::start();
    let channel = build_channel(&server).await;
    let conv = matrix_conv();
    let parent = MessageRef::top_level("matrix", conv.clone(), "$root:localhost");
    server.mock(|when, then| {
        when.method(GET)
            .path_matches(r"^/_matrix/(media/(r0|v3)/config|client/(v1|v3)/media/config)$");
        then.status(200)
            .json_body(json!({"m.upload.size": 1_000_000_u64}));
    });
    let upload = server.mock(|when, then| {
        when.method(POST)
            .path_matches(r"^/_matrix/(media/(r0|v3)/upload|client/(v1|v3)/media/upload)$")
            .body_matches("hello");
        then.status(200)
            .json_body(json!({"content_uri": "mxc://localhost/file"}));
    });
    let send_mock = server.mock(|when, then| {
        when.method(PUT)
            .path_matches(r"^/_matrix/client/(r0|v3)/rooms/[^/]+/send/m\.room\.message/[^/]+$")
            .is_true(|request| file_message_has_formatted_markdown_caption(request.body_ref()));
        then.status(200)
            .json_body(json!({"event_id": "$file:localhost"}));
    });

    let uploaded = channel
        .upload(
            &Subject::new("agent"),
            &conv,
            "note.txt",
            b"hello".to_vec(),
            Some("**caption**"),
            Some(&parent),
        )
        .await
        .expect("upload");

    upload.assert();
    send_mock.assert();
    assert_eq!(uploaded.id, "$file:localhost");
    assert_eq!(uploaded.thread_root(), Some("$root:localhost"));
}

#[tokio::test]
async fn channel_upload_posts_image_message_for_image_mime() {
    let server = MockServer::start();
    let channel = build_channel(&server).await;
    let conv = matrix_conv();
    server.mock(|when, then| {
        when.method(GET)
            .path_matches(r"^/_matrix/(media/(r0|v3)/config|client/(v1|v3)/media/config)$");
        then.status(200)
            .json_body(json!({"m.upload.size": 1_000_000_u64}));
    });
    let upload = server.mock(|when, then| {
        when.method(POST)
            .path_matches(r"^/_matrix/(media/(r0|v3)/upload|client/(v1|v3)/media/upload)$")
            .body_matches("png-bytes");
        then.status(200)
            .json_body(json!({"content_uri": "mxc://localhost/image"}));
    });
    let send_mock = server.mock(|when, then| {
        when.method(PUT)
            .path_matches(r"^/_matrix/client/(r0|v3)/rooms/[^/]+/send/m\.room\.message/[^/]+$")
            .is_true(|request| image_message_has_formatted_markdown_caption(request.body_ref()));
        then.status(200)
            .json_body(json!({"event_id": "$image:localhost"}));
    });

    let uploaded = channel
        .upload(
            &Subject::new("agent"),
            &conv,
            "plot.png",
            b"png-bytes".to_vec(),
            Some("**caption**"),
            None,
        )
        .await
        .expect("upload");

    upload.assert();
    send_mock.assert();
    assert_eq!(uploaded.id, "$image:localhost");
    assert!(uploaded.thread_root().is_none());
}

fn file_message_has_formatted_markdown_caption(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    value.get("msgtype").and_then(Value::as_str) == Some("m.file")
        && value.get("url").and_then(Value::as_str) == Some("mxc://localhost/file")
        && value.get("filename").and_then(Value::as_str) == Some("note.txt")
        && value.get("body").and_then(Value::as_str) == Some("**caption**")
        && value.get("format").and_then(Value::as_str) == Some("org.matrix.custom.html")
        && value.get("formatted_body").and_then(Value::as_str) == Some("<strong>caption</strong>")
        && value
            .get("m.relates_to")
            .and_then(|relates_to| relates_to.get("rel_type"))
            .and_then(Value::as_str)
            == Some("m.thread")
}

fn image_message_has_formatted_markdown_caption(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    value.get("msgtype").and_then(Value::as_str) == Some("m.image")
        && value.get("url").and_then(Value::as_str) == Some("mxc://localhost/image")
        && value.get("filename").and_then(Value::as_str) == Some("plot.png")
        && value.get("body").and_then(Value::as_str) == Some("**caption**")
        && value.get("format").and_then(Value::as_str) == Some("org.matrix.custom.html")
        && value.get("formatted_body").and_then(Value::as_str) == Some("<strong>caption</strong>")
}

#[tokio::test]
async fn channel_read_maps_history_and_filters_thread() {
    let server = MockServer::start();
    let channel = build_channel(&server).await;
    let conv = matrix_conv();
    let parent = MessageRef::top_level("matrix", conv.clone(), "$root:localhost");
    let messages = server.mock(|when, then| {
        when.method(GET)
            .path_matches(r"^/_matrix/client/(r0|v3)/rooms/[^/]+/messages$");
        then.status(200).json_body(json!({
            "start": "s0",
            "end": "s1",
            "chunk": [
                {
                    "type": "m.room.message",
                    "event_id": "$reply:localhost",
                    "sender": "@alice:localhost",
                    "origin_server_ts": 1_700_000_000_123_u64,
                    "content": {
                        "msgtype": "m.text",
                        "body": "thread reply",
                        "m.relates_to": {
                            "rel_type": "m.thread",
                            "event_id": "$root:localhost"
                        }
                    }
                },
                {
                    "type": "m.room.message",
                    "event_id": "$other:localhost",
                    "sender": "@alice:localhost",
                    "origin_server_ts": 1_700_000_000_999_u64,
                    "content": {
                        "msgtype": "m.text",
                        "body": "other"
                    }
                }
            ],
            "state": []
        }));
    });

    let read = channel
        .read(&Subject::new("agent"), &conv, Some(&parent), 25)
        .await
        .expect("read");

    messages.assert();
    assert_eq!(read.len(), 1);
    let item = read
        .first()
        .expect("thread-filtered read should return one item");
    assert_eq!(item.message_ref.id, "$reply:localhost");
    assert_eq!(item.message_ref.thread_root(), Some("$root:localhost"));
    assert_eq!(item.author.as_str(), "@alice:localhost");
    assert_eq!(item.body, "thread reply");
    assert_eq!(item.timestamp_unix_ms, 1_700_000_000_123);
}

async fn build_channel(server: &MockServer) -> MatrixChannel {
    let client = Client::new(Url::parse(&server.base_url()).expect("mock url"))
        .await
        .expect("client");
    client
        .restore_session(MatrixSession {
            meta: SessionMeta {
                user_id: owned_user_id!("@bot:localhost"),
                device_id: OwnedDeviceId::from("DEVICE"),
            },
            tokens: SessionTokens {
                access_token: "1234".to_owned(),
                refresh_token: None,
            },
        })
        .await
        .expect("restore session");
    seed_joined_room(&client, server).await;
    MatrixChannel::from_client(
        client,
        owned_user_id!("@bot:localhost"),
        Some("Nova".to_owned()),
    )
}

async fn seed_joined_room(client: &Client, server: &MockServer) {
    server.mock(|when, then| {
        when.method(GET).path("/_matrix/client/versions");
        then.status(200).json_body(json!({
            "versions": ["r0.6.0", "v1.1", "v1.3"]
        }));
    });
    let sync = server.mock(|when, then| {
        when.method(GET)
            .path_matches(r"^/_matrix/client/(r0|v3)/sync$");
        then.status(200).json_body(json!({
            "next_batch": "s1",
            "rooms": {
                "join": {
                    ROOM_ID: {
                        "state": { "events": [] },
                        "timeline": {
                            "events": [],
                            "limited": false,
                            "prev_batch": "p0"
                        },
                        "ephemeral": { "events": [] },
                        "account_data": { "events": [] },
                        "unread_notifications": {}
                    }
                }
            }
        }));
    });
    client
        .sync_once(SyncSettings::default())
        .await
        .expect("seed joined room");
    sync.assert();
}

fn matrix_conv() -> Owner {
    Owner::new(format!("matrix:{ROOM_ID}"))
}
