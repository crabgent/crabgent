use std::sync::Arc;

use crabgent_core::subject::{InvalidSubjectError, Subject};

use crate::channel::{ChannelKind, ConvLabel};
use crate::envelope::InboundEvent;
use crate::subject::{ChannelSubjectExt, channel_subject_id};

use super::SubjectResolver;

pub(super) fn default_subject_resolver(kind: Option<ChannelKind>) -> SubjectResolver {
    Arc::new(move |event: &InboundEvent| subject_from_inbound_event(event, kind))
}

/// Build the canonical channel subject for an inbound message event.
pub fn subject_from_inbound_event(
    event: &InboundEvent,
    kind: Option<ChannelKind>,
) -> Result<Subject, InvalidSubjectError> {
    if event.channel.trim().is_empty() || event.from.id.as_str().trim().is_empty() {
        return Err(InvalidSubjectError);
    }
    let id = channel_subject_id(&event.channel, event.from.id.as_str());
    let mut subject = Subject::try_new(id)?
        .with_participant_role(event.from.role.as_str())
        .with_sender_display(event.from.display_name.as_deref());
    let kind = event.kind.or(kind);
    if let Some(kind) = kind {
        subject = subject.with_channel(&event.channel, &event.conv, kind);
    } else {
        subject = subject
            .with_attr(crate::subject::attr_keys::CHANNEL, event.channel.as_str())
            .with_attr(crate::subject::attr_keys::CONV, event.conv.as_str());
    }
    Ok(subject.with_inbound_message_ref(&event.message))
}

/// Stamp the async-resolved `conv_display` labels onto an already-resolved
/// subject.
///
/// `label` carries the `Channel::conv_display` result (channel/workspace
/// names). Each attr is written only when present, so an empty label leaves
/// the subject untouched. The sender label is stamped earlier by
/// [`subject_from_inbound_event`] (it is sync, straight from `event.from`),
/// so both the message and reaction paths inherit it without an extra call.
pub(super) fn stamp_conv_display(subject: Subject, label: &ConvLabel) -> Subject {
    subject.with_conv_display(label)
}
