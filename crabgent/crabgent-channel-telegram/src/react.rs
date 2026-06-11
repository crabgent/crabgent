//! Telegram reaction dispatch.

use crabgent_channel::{ChannelError, MessageRef};
use crabgent_core::owner::Owner;
use crabgent_log::{debug, instrument};

use crate::{
    channel::{CHANNEL_NAME, TelegramChannel},
    outbound,
};

#[instrument(level = "debug", skip(channel, parent, emoji), fields(conv = %conv))]
pub async fn react(
    channel: &TelegramChannel,
    conv: &Owner,
    parent: &MessageRef,
    emoji: &str,
) -> Result<MessageRef, ChannelError> {
    let chat_id = outbound::parse_chat_id(conv)?;
    let message_id = outbound::parse_message_id(&parent.id)?;
    let body = outbound::build_set_message_reaction_body(chat_id, message_id, emoji);
    debug!(method = "setMessageReaction", "telegram react dispatch");
    channel.post_json("setMessageReaction", &body).await?;
    Ok(MessageRef::top_level(
        CHANNEL_NAME,
        conv.clone(),
        parent.id.clone(),
    ))
}
