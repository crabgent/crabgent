//! Outbound Matrix reaction helpers.

use crabgent_channel::{ChannelError, MessageRef};
use matrix_sdk::ruma::{
    OwnedEventId,
    events::{reaction::ReactionEventContent, relation::Annotation},
};

/// Convert a crabgent parent message reference to Matrix `m.reaction` content.
pub fn build_reaction_content(
    parent: &MessageRef,
    emoji: &str,
) -> Result<ReactionEventContent, ChannelError> {
    if emoji.is_empty() {
        return Err(ChannelError::InvalidEnvelope("empty emoji".to_owned()));
    }
    let event_id = OwnedEventId::try_from(parent.id.clone()).map_err(|err| {
        ChannelError::InvalidEnvelope(format!(
            "invalid matrix reaction target '{}': {err}",
            parent.id
        ))
    })?;
    Ok(ReactionEventContent::new(Annotation::new(
        event_id,
        emoji.to_owned(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::owner::Owner;

    #[test]
    fn reaction_content_targets_parent_event() {
        let conv = Owner::new("matrix:!room:example.org");
        let parent = MessageRef::top_level("matrix", conv, "$event:example.org");
        let Ok(content) = build_reaction_content(&parent, "👀") else {
            panic!("valid matrix event id should build reaction content");
        };

        assert_eq!(content.relates_to.event_id.as_str(), "$event:example.org");
        assert_eq!(content.relates_to.key, "👀");
    }

    #[test]
    fn invalid_parent_id_is_rejected() {
        let conv = Owner::new("matrix:!room:example.org");
        let parent = MessageRef::top_level("matrix", conv, "not-an-event-id");

        assert!(matches!(
            build_reaction_content(&parent, "👀"),
            Err(ChannelError::InvalidEnvelope(_))
        ));
    }

    #[test]
    fn empty_emoji_is_rejected() {
        let conv = Owner::new("matrix:!room:example.org");
        let parent = MessageRef::top_level("matrix", conv, "$event:example.org");

        assert!(matches!(
            build_reaction_content(&parent, ""),
            Err(ChannelError::InvalidEnvelope(_))
        ));
    }
}
