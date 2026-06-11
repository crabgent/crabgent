use crabgent_channel::image_store::file_system::{
    FileSystemImageStore, FileSystemImageStoreConfig,
};
use crabgent_channel::{AudioValidator, ImageValidator};
use crabgent_channel_slack::events::SlackEvent;
use crabgent_channel_slack::ids::SlackWorkspaceId;
use crabgent_channel_slack::inbound::{
    new_channel_kind_cache, new_channel_type_cache, slack_event_to_inbound_with_channel_type_cache,
};
use secrecy::SecretString;
use serde_json::json;

#[tokio::test]
async fn dm_message_maps_to_thread_session() {
    let event: SlackEvent = serde_json::from_value(json!({
        "type": "message",
        "channel": "D123",
        "channel_type": "im",
        "user": "U123",
        "text": "hello",
        "ts": "1.2",
        "thread_ts": "1.1"
    }))
    .expect("event");
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
        cache_root: dir.path().to_owned(),
    });
    let validator = ImageValidator::new();
    let audio_validator = AudioValidator::new();
    let client = reqwest::Client::new();
    let token = SecretString::new("dummy".into());
    let inbound = slack_event_to_inbound_with_channel_type_cache(
        &event,
        &workspace,
        &new_channel_kind_cache(),
        &new_channel_type_cache(),
        None,
        &client,
        &token,
        &store,
        &validator,
        &audio_validator,
    )
    .await
    .expect("inbound");

    assert_eq!(inbound.conv.as_str(), "slack:T123/D123");
    assert_eq!(inbound.body, "hello");
    assert_eq!(inbound.message.thread_root(), Some("1.1"));
    assert!(!inbound.message.broadcast());
}
