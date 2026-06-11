//! Matrix subject construction for channel policy decisions.

use std::sync::Arc;

use crabgent_channel::{
    ChannelKind, ChannelSubjectExt, InboundEvent, attr_keys, channel_subject_id,
};
use crabgent_core::subject::Subject;

use crate::{
    MatrixChannel,
    outbound::{CHANNEL_NAME, parse_owner_to_room_id},
};

/// Build a subject resolver that stamps Matrix participant identity.
pub fn build_subject_resolver(
    channel: Arc<MatrixChannel>,
    agent_name: String,
) -> impl Fn(&InboundEvent) -> Subject + Send + Sync + 'static {
    move |event| {
        let kind = event
            .kind
            .or_else(|| room_kind_from_cache(&channel, event))
            .unwrap_or(ChannelKind::Group);
        Subject::new(channel_subject_id(CHANNEL_NAME, event.from.id.as_str()))
            .with_participant_role(event.from.role.as_str())
            .with_channel(CHANNEL_NAME, &event.conv, kind)
            .with_attr(attr_keys::PARTICIPANT_ID, event.from.id.as_str())
            .with_attr("agent", agent_name.as_str())
            .with_inbound_message_ref(&event.message)
    }
}

fn room_kind_from_cache(channel: &MatrixChannel, event: &InboundEvent) -> Option<ChannelKind> {
    let room_id = parse_owner_to_room_id(&event.conv).ok()?;
    let cache = channel.kind_cache();
    cache.lock().ok()?.get(&room_id).copied()
}
