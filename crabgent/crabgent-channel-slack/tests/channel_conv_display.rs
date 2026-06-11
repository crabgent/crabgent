use std::collections::HashMap;
use std::sync::Arc;

use crabgent_channel::Channel;
use crabgent_channel_slack::{
    SlackChannel, SlackChannelId, SlackChannelNames, SlackConfig, SlackHttpClient,
};
use crabgent_core::owner::Owner;
use secrecy::SecretString;

fn test_channel(names: SlackChannelNames) -> SlackChannel {
    let config = SlackConfig::new(
        SecretString::from("app-token"),
        SecretString::from("bot-token"),
    )
    .expect("config");
    let client = Arc::new(SlackHttpClient::new(config).expect("client"));
    SlackChannel::new(client).with_channel_names(names)
}

#[tokio::test]
async fn conv_display_resolves_name_and_workspace_on_hit() {
    let mut map = HashMap::new();
    map.insert(
        SlackChannelId::new("C1").expect("id"),
        "platform-ops".to_owned(),
    );
    let channel = test_channel(SlackChannelNames::new(map, Some("example".to_owned())));

    let label = channel
        .conv_display(&Owner::new("slack:T1/C1"))
        .await
        .expect("hit yields a label");
    assert_eq!(label.name.as_deref(), Some("platform-ops"));
    assert_eq!(label.workspace.as_deref(), Some("example"));
}

#[tokio::test]
async fn conv_display_omits_name_on_dm_miss_but_keeps_workspace() {
    let channel = test_channel(SlackChannelNames::new(
        HashMap::new(),
        Some("example".to_owned()),
    ));

    // A DM channel id is absent from the pre-warm map: name is None, but the
    // constant workspace still resolves. The DM partner is surfaced via the
    // sender display, not here.
    let label = channel
        .conv_display(&Owner::new("slack:T1/D9"))
        .await
        .expect("workspace-only label");
    assert_eq!(label.name, None);
    assert_eq!(label.workspace.as_deref(), Some("example"));
}

#[tokio::test]
async fn conv_display_returns_none_when_nothing_resolves() {
    let channel = test_channel(SlackChannelNames::default());
    assert!(
        channel
            .conv_display(&Owner::new("slack:T1/C1"))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn conv_display_returns_none_for_malformed_owner() {
    let channel = test_channel(SlackChannelNames::new(
        HashMap::new(),
        Some("example".to_owned()),
    ));
    assert!(
        channel
            .conv_display(&Owner::new("not-a-slack-owner"))
            .await
            .is_none()
    );
}
