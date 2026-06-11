//! Telegram `message_reaction` inbound mapping.
//!
//! Telegram delivers reaction edits as `MessageReactionUpdated`
//! updates (<https://core.telegram.org/bots/api#messagereactionupdated>).
//! Each update carries the full `new_reaction` and `old_reaction`
//! lists, so the adapter computes the symmetric difference and emits
//! one `InboundReaction` per emoji delta.
//!
//! Custom-emoji and paid reactions are dropped with a debug log;
//! reactions in non-private chats and anonymous reactions (no
//! `user`) are also dropped, matching the adapter's direct-only
//! message scope.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use crabgent_channel::{
    ChannelError, ChannelInbox, InboundReaction, MessageRef, Participant, ParticipantId,
    ParticipantRole,
};
use crabgent_core::owner::Owner;
use crabgent_log::{debug, error};
use serde::Deserialize;

use super::{TELEGRAM_PRIVATE_TYPE, TelegramChat, TelegramUser, display_name};

/// Stable adapter name used by the Telegram channel.
pub(super) const CHANNEL_NAME: &str = "telegram";

#[derive(Debug, Deserialize)]
pub struct TelegramMessageReactionUpdated {
    pub chat: TelegramChat,
    pub message_id: i64,
    #[serde(default)]
    pub user: Option<TelegramUser>,
    pub date: i64,
    #[serde(default)]
    pub old_reaction: Vec<TelegramReactionType>,
    #[serde(default)]
    pub new_reaction: Vec<TelegramReactionType>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TelegramReactionType {
    Emoji { emoji: String },
    CustomEmoji { custom_emoji_id: String },
    Paid {},
}

impl TelegramReactionType {
    fn as_emoji(&self) -> Option<&str> {
        if let Self::Emoji { emoji } = self {
            return Some(emoji.as_str());
        }
        log_skipped_reaction(self);
        None
    }
}

fn log_skipped_reaction(reaction: &TelegramReactionType) {
    if let TelegramReactionType::CustomEmoji { custom_emoji_id } = reaction {
        log_custom_emoji_reaction(custom_emoji_id);
        return;
    }
    if matches!(reaction, TelegramReactionType::Paid {}) {
        log_paid_reaction();
    }
}

fn log_custom_emoji_reaction(custom_emoji_id: &str) {
    debug!(
        custom_emoji_id = %custom_emoji_id,
        "skipping custom_emoji telegram reaction"
    );
}

fn log_paid_reaction() {
    debug!("skipping paid telegram reaction");
}

/// Map a `MessageReactionUpdated` into one `InboundReaction` per
/// emoji delta. Empty when the chat is not private, the reaction has
/// no `user`, or all reactions are non-emoji types.
pub(super) fn update_to_reactions(mr: &TelegramMessageReactionUpdated) -> Vec<InboundReaction> {
    if mr.chat.chat_type != TELEGRAM_PRIVATE_TYPE {
        return Vec::new();
    }
    let Some(user) = mr.user.as_ref() else {
        return Vec::new();
    };
    let new_set: HashSet<&str> = mr
        .new_reaction
        .iter()
        .filter_map(TelegramReactionType::as_emoji)
        .collect();
    let old_set: HashSet<&str> = mr
        .old_reaction
        .iter()
        .filter_map(TelegramReactionType::as_emoji)
        .collect();
    let conv = Owner::new(format!("{CHANNEL_NAME}:{}", mr.chat.id));
    let from = build_participant(user);
    let parent = MessageRef::top_level(CHANNEL_NAME, conv.clone(), mr.message_id.to_string());
    let timestamp = timestamp_to_utc(mr.date);
    let mut events = Vec::with_capacity(new_set.len() + old_set.len());
    for &emoji in new_set.difference(&old_set) {
        events.push(build_event(&conv, &from, &parent, emoji, true, timestamp));
    }
    for &emoji in old_set.difference(&new_set) {
        events.push(build_event(&conv, &from, &parent, emoji, false, timestamp));
    }
    events
}

/// Forward each reaction emitted by [`update_to_reactions`] through
/// `inbox.receive_reaction`. Stops at the first error so the poller
/// can preserve its `last_offset` retry semantics.
pub(super) async fn dispatch_reactions(
    inbox: &Arc<dyn ChannelInbox>,
    update_id: i64,
    mr: &TelegramMessageReactionUpdated,
) -> Result<(), ChannelError> {
    for reaction in update_to_reactions(mr) {
        if let Err(err) = inbox.receive_reaction(reaction).await {
            error!(
                update_id,
                error = %err,
                "telegram inbox receive_reaction failed"
            );
            return Err(err);
        }
    }
    Ok(())
}

fn build_event(
    conv: &Owner,
    from: &Participant,
    parent: &MessageRef,
    emoji: &str,
    added: bool,
    timestamp: DateTime<Utc>,
) -> InboundReaction {
    InboundReaction {
        channel: CHANNEL_NAME.into(),
        conv: conv.clone(),
        from: from.clone(),
        parent: parent.clone(),
        emoji: emoji.to_owned(),
        added,
        timestamp,
    }
}

fn build_participant(user: &TelegramUser) -> Participant {
    let mut p = Participant::new(
        ParticipantId::new(user.id.to_string()),
        ParticipantRole::Human,
    );
    if let Some(name) = display_name(user) {
        p = p.with_display_name(name);
    }
    p
}

fn timestamp_to_utc(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_user(id: i64) -> TelegramUser {
        TelegramUser {
            id,
            username: Some("alice".into()),
            first_name: None,
            last_name: None,
        }
    }

    fn build_update(
        chat_type: &str,
        user: Option<TelegramUser>,
        old: Vec<TelegramReactionType>,
        new: Vec<TelegramReactionType>,
    ) -> TelegramMessageReactionUpdated {
        TelegramMessageReactionUpdated {
            chat: TelegramChat {
                id: 42,
                chat_type: chat_type.to_owned(),
            },
            message_id: 100,
            user,
            date: 1_700_000_000,
            old_reaction: old,
            new_reaction: new,
        }
    }

    fn emoji_reaction(s: &str) -> TelegramReactionType {
        TelegramReactionType::Emoji {
            emoji: s.to_owned(),
        }
    }

    #[test]
    fn empty_delta_returns_no_events() {
        let update = build_update("private", Some(build_user(7)), vec![], vec![]);
        assert!(update_to_reactions(&update).is_empty());
    }

    #[test]
    fn single_emoji_added_emits_added_true() {
        let update = build_update(
            "private",
            Some(build_user(7)),
            vec![],
            vec![emoji_reaction("👍")],
        );
        let events = update_to_reactions(&update);
        assert_eq!(events.len(), 1);
        let r = &events[0];
        assert_eq!(r.channel, "telegram");
        assert_eq!(r.conv.as_str(), "telegram:42");
        assert_eq!(r.from.id.as_str(), "7");
        assert_eq!(r.emoji, "👍");
        assert!(r.added);
        assert_eq!(r.parent.id, "100");
    }

    #[test]
    fn single_emoji_removed_emits_added_false() {
        let update = build_update(
            "private",
            Some(build_user(7)),
            vec![emoji_reaction("👍")],
            vec![],
        );
        let events = update_to_reactions(&update);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].emoji, "👍");
        assert!(!events[0].added);
    }

    #[test]
    fn replace_emoji_emits_two_events_one_each_direction() {
        let update = build_update(
            "private",
            Some(build_user(7)),
            vec![emoji_reaction("👍")],
            vec![emoji_reaction("❤")],
        );
        let events = update_to_reactions(&update);
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|r| r.emoji == "❤" && r.added));
        assert!(events.iter().any(|r| r.emoji == "👍" && !r.added));
    }

    #[test]
    fn no_change_in_set_is_no_op() {
        let update = build_update(
            "private",
            Some(build_user(7)),
            vec![emoji_reaction("👍")],
            vec![emoji_reaction("👍")],
        );
        assert!(update_to_reactions(&update).is_empty());
    }

    #[test]
    fn group_chat_is_filtered() {
        let update = build_update(
            "group",
            Some(build_user(7)),
            vec![],
            vec![emoji_reaction("👍")],
        );
        assert!(update_to_reactions(&update).is_empty());
    }

    #[test]
    fn anonymous_no_user_is_filtered() {
        let update = build_update("private", None, vec![], vec![emoji_reaction("👍")]);
        assert!(update_to_reactions(&update).is_empty());
    }

    #[test]
    fn custom_emoji_dropped_but_emoji_passthrough_preserved() {
        let update = build_update(
            "private",
            Some(build_user(7)),
            vec![],
            vec![
                TelegramReactionType::CustomEmoji {
                    custom_emoji_id: "5".into(),
                },
                emoji_reaction("🎉"),
            ],
        );
        let events = update_to_reactions(&update);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].emoji, "🎉");
    }

    #[test]
    fn paid_reaction_is_dropped() {
        let update = build_update(
            "private",
            Some(build_user(7)),
            vec![],
            vec![TelegramReactionType::Paid {}],
        );
        assert!(update_to_reactions(&update).is_empty());
    }

    #[test]
    fn json_round_trip_emoji_variant() {
        let v = serde_json::json!({"type": "emoji", "emoji": "👍"});
        let parsed: TelegramReactionType = serde_json::from_value(v).expect("parse");
        assert!(matches!(parsed, TelegramReactionType::Emoji { emoji } if emoji == "👍"));
    }

    #[test]
    fn json_round_trip_custom_emoji_variant() {
        let v = serde_json::json!({"type": "custom_emoji", "custom_emoji_id": "5"});
        let parsed: TelegramReactionType = serde_json::from_value(v).expect("parse");
        assert!(matches!(
            parsed,
            TelegramReactionType::CustomEmoji { custom_emoji_id } if custom_emoji_id == "5"
        ));
    }

    #[test]
    fn json_round_trip_paid_variant() {
        let v = serde_json::json!({"type": "paid"});
        let parsed: TelegramReactionType = serde_json::from_value(v).expect("parse");
        assert!(matches!(parsed, TelegramReactionType::Paid {}));
    }
}
