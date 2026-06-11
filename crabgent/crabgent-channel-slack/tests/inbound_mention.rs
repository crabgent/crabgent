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
async fn app_mention_maps_to_group_conversation() {
    let event: SlackEvent = serde_json::from_value(json!({
        "type": "app_mention",
        "channel": "C123",
        "user": "U123",
        "text": "<@BOT> hello",
        "ts": "2.1"
    }))
    .expect("event");
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let cache = new_channel_kind_cache();
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
        &cache,
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

    assert_eq!(inbound.conv.as_str(), "slack:T123/C123");
    assert_eq!(inbound.message.thread_root(), None);
    assert_eq!(
        cache.lock().expect("cache").get("C123"),
        Some(&crabgent_channel::ChannelKind::Group)
    );
}
