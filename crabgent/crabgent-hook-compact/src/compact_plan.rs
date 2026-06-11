//! Message-window planning for semantic compaction.

use crabgent_core::Message;
use crabgent_core::message::tail::tool_result_group_boundary;

use crate::render::is_summary_message;

pub struct CompactionPlan<'a> {
    pub(crate) leading_system: &'a [Message],
    pub(crate) prior_summary: Option<&'a Message>,
    pub(crate) compacted: &'a [Message],
    pub(crate) recent: &'a [Message],
}

impl<'a> CompactionPlan<'a> {
    pub(crate) fn new(messages: &'a [Message], keep_recent_messages: usize) -> Option<Self> {
        let leading_system_count = leading_system_count(messages);
        let compactable_len = messages.len().saturating_sub(leading_system_count);
        let recent_len = keep_recent_messages.min(compactable_len);
        let compacted_end =
            tool_result_group_boundary(messages, messages.len().checked_sub(recent_len)?);
        let (prior_summary, compacted_start) = match messages.get(leading_system_count) {
            Some(message) if is_summary_message(message) => {
                (Some(message), leading_system_count + 1)
            }
            _ => (None, leading_system_count),
        };
        if compacted_end <= compacted_start {
            return None;
        }
        Some(Self {
            leading_system: messages.get(..leading_system_count)?,
            prior_summary,
            compacted: messages.get(compacted_start..compacted_end)?,
            recent: messages.get(compacted_end..)?,
        })
    }
}

pub fn leading_system_count(messages: &[Message]) -> usize {
    messages
        .iter()
        .take_while(|message| matches!(message, Message::System { .. }))
        .count()
}
