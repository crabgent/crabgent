//! Mapping of Slack `reaction_added` / `reaction_removed` events into
//! `InboundReaction`.

use crabgent_channel_slack::events::SlackEvent;
use crabgent_channel_slack::ids::SlackWorkspaceId;
use crabgent_channel_slack::inbound::{new_channel_kind_cache, slack_event_to_inbound_reaction};
use serde_json::json;

fn workspace() -> SlackWorkspaceId {
    SlackWorkspaceId::new("T123").expect("workspace")
}

fn reaction_event(kind: &str, user: &str, channel: &str, ts: &str, emoji: &str) -> SlackEvent {
    serde_json::from_value(json!({
        "type": kind,
        "user": user,
        "reaction": emoji,
        "item": {
            "type": "message",
            "channel": channel,
            "ts": ts,
        },
        "event_ts": "100.1",
    }))
    .expect("event parse")
}

#[test]
fn reaction_added_maps_to_inbound_reaction() {
    let event = reaction_event("reaction_added", "U999", "C111", "ts:42", "+1");
    let workspace = workspace();
    let cache = new_channel_kind_cache();
    let r = slack_event_to_inbound_reaction(&event, &workspace, &cache, None).expect("mapped");
    assert_eq!(r.channel, "slack");
    assert_eq!(r.conv.as_str(), "slack:T123/C111");
    assert_eq!(r.from.id.as_str(), "U999");
    assert_eq!(r.emoji, "+1");
    assert!(r.added);
    assert_eq!(r.parent.id, "ts:42");
}

#[test]
fn reaction_removed_maps_with_added_false() {
    let event = reaction_event("reaction_removed", "U999", "C111", "ts:42", "+1");
    let workspace = workspace();
    let cache = new_channel_kind_cache();
    let r = slack_event_to_inbound_reaction(&event, &workspace, &cache, None).expect("mapped");
    assert!(!r.added);
}

#[test]
fn reaction_from_bot_self_is_dropped() {
    let event = reaction_event("reaction_added", "U_BOT", "C111", "ts:42", "+1");
    let workspace = workspace();
    let cache = new_channel_kind_cache();
    let mapped = slack_event_to_inbound_reaction(&event, &workspace, &cache, Some("U_BOT"));
    assert!(mapped.is_none(), "bot self-reaction must be filtered");
}

#[test]
fn reaction_without_user_is_dropped() {
    let event: SlackEvent = serde_json::from_value(json!({
        "type": "reaction_added",
        "reaction": "+1",
        "item": {"type": "message", "channel": "C111", "ts": "ts:42"},
    }))
    .expect("event parse");
    let workspace = workspace();
    let cache = new_channel_kind_cache();
    let mapped = slack_event_to_inbound_reaction(&event, &workspace, &cache, None);
    assert!(mapped.is_none(), "anonymous reaction must be filtered");
}

#[test]
fn reaction_caches_channel_kind_to_group_when_unknown() {
    let event = reaction_event("reaction_added", "U999", "C222", "ts:1", "eyes");
    let workspace = workspace();
    let cache = new_channel_kind_cache();
    let _ = slack_event_to_inbound_reaction(&event, &workspace, &cache, None);
    assert_eq!(
        cache.lock().expect("cache").get("C222"),
        Some(&crabgent_channel::ChannelKind::Group)
    );
}

#[test]
fn reaction_does_not_overwrite_existing_kind_cache() {
    let event = reaction_event("reaction_added", "U999", "C333", "ts:1", "eyes");
    let workspace = workspace();
    let cache = new_channel_kind_cache();
    cache
        .lock()
        .expect("cache")
        .insert("C333".into(), crabgent_channel::ChannelKind::Direct);
    let _ = slack_event_to_inbound_reaction(&event, &workspace, &cache, None);
    assert_eq!(
        cache.lock().expect("cache").get("C333"),
        Some(&crabgent_channel::ChannelKind::Direct),
        "existing kind must not be downgraded to Group"
    );
}

#[test]
fn non_reaction_event_returns_none() {
    let event: SlackEvent = serde_json::from_value(json!({
        "type": "app_mention",
        "channel": "C123",
        "user": "U123",
        "text": "hi",
        "ts": "1.0",
    }))
    .expect("event");
    let workspace = workspace();
    let cache = new_channel_kind_cache();
    let mapped = slack_event_to_inbound_reaction(&event, &workspace, &cache, None);
    assert!(mapped.is_none());
}
