mod common;

use crabgent_channel::image_store::file_system::{
    FileSystemImageStore, FileSystemImageStoreConfig,
};
use crabgent_channel::{AudioValidator, ImageValidator};
use crabgent_channel_slack::SlackChannel;
use crabgent_channel_slack::events::SlackEvent;
use crabgent_channel_slack::ids::SlackWorkspaceId;
use crabgent_channel_slack::inbound::{
    new_channel_kind_cache, new_channel_type_cache, slack_event_to_inbound_with_channel_type_cache,
};
use secrecy::SecretString;
use serde_json::json;

use common::{slack_client, slack_test_ctx};

#[tokio::test]
async fn channel_type_cache_roundtrips_raw_message_types() {
    let ctx = slack_test_ctx().await;
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let kind_cache = new_channel_kind_cache();
    let type_cache = new_channel_type_cache();
    let channel = SlackChannel::new(slack_client(&ctx)).with_channel_type_cache(type_cache.clone());

    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileSystemImageStore::new(FileSystemImageStoreConfig {
        cache_root: dir.path().to_owned(),
    });
    let validator = ImageValidator::new();
    let client = reqwest::Client::new();
    let token = SecretString::new("dummy".into());
    let audio_validator = AudioValidator::new();

    for (channel_id, channel_type) in [
        ("D123", "im"),
        ("G123", "mpim"),
        ("C123", "channel"),
        ("G456", "group"),
    ] {
        let event: SlackEvent = serde_json::from_value(json!({
            "type": "message",
            "channel": channel_id,
            "channel_type": channel_type,
            "user": "U123",
            "text": "hello",
            "ts": "1.2"
        }))
        .expect("event");

        let inbound = slack_event_to_inbound_with_channel_type_cache(
            &event,
            &workspace,
            &kind_cache,
            &type_cache,
            None,
            &client,
            &token,
            &store,
            &validator,
            &audio_validator,
        )
        .await
        .expect("inbound");

        assert_eq!(inbound.conv.as_str(), format!("slack:T123/{channel_id}"));
        assert_eq!(
            channel.channel_type(channel_id).as_deref(),
            Some(channel_type)
        );
    }
}
