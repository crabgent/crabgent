use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel_matrix::outbound::parse_owner_to_room_id;
use crabgent_core::{
    Action,
    memory::MemoryScope,
    owner::Owner,
    policy::{PolicyDecision, PolicyHook, StrictPolicy},
    subject::Subject,
};
use matrix_sdk::ruma::OwnedRoomId;

use super::{
    MatrixPolicyConfig,
    builder::{build_strict_inner_for_scope, build_strict_inner_for_scope_with_channels},
    visibility::{RoomVisibility, VisibilityResolver},
};

pub struct ChannelScopePolicy {
    inner: StrictPolicy,
    visibility: Arc<dyn VisibilityResolver + Send + Sync>,
}

#[must_use]
pub fn build_with_channel_scope(
    config: &MatrixPolicyConfig,
    visibility: Arc<dyn VisibilityResolver + Send + Sync>,
) -> ChannelScopePolicy {
    ChannelScopePolicy {
        inner: build_strict_inner_for_scope(config),
        visibility,
    }
}

#[must_use]
pub fn build_with_channel_scope_multi(
    config: &MatrixPolicyConfig,
    visibility: Arc<dyn VisibilityResolver + Send + Sync>,
    has_telegram: bool,
) -> ChannelScopePolicy {
    ChannelScopePolicy {
        inner: build_strict_inner_for_scope_with_channels(config, true, has_telegram),
        visibility,
    }
}

#[async_trait]
impl PolicyHook for ChannelScopePolicy {
    async fn allow(&self, subject: &Subject, action: &Action) -> PolicyDecision {
        match action {
            Action::MemorySearch { scope, .. }
            | Action::MemoryStore { scope }
            | Action::MemoryGet { scope, .. }
            | Action::MemoryDelete { scope, .. }
            | Action::MemoryConsolidate { scope }
            | Action::RelationStore { scope }
            | Action::RelationDelete { scope }
            | Action::RelationExpand { scope }
            | Action::SessionSearch { scope, .. }
            | Action::CronCreate { scope }
            | Action::CronGet { scope, .. }
            | Action::CronList { scope }
            | Action::CronUpdate { scope, .. }
            | Action::CronDelete { scope, .. } => {
                if self.scope_allowed(subject, action, scope) {
                    PolicyDecision::Allow
                } else {
                    PolicyDecision::Deny(
                        "scope.conv outside channel-scope-policy allow set".to_owned(),
                    )
                }
            }
            Action::TaskCreate { .. }
            | Action::TaskList { .. }
            | Action::TaskGet { .. }
            | Action::TaskCancel { .. }
            | Action::ModelList
            | Action::ModelGet { .. }
            | Action::ModelsCurrent { .. }
            | Action::ModelsSetSessionOverride { .. }
            | Action::ModelsClearSessionOverride { .. }
            | Action::ModelsSetGlobalOverride { .. }
            | Action::ModelsClearGlobalOverride
            | Action::CalendarHolidaysList
            | Action::CalendarHolidaysNext
            | Action::CalendarHolidayCheck
            | Action::CalendarDaysBetween
            | Action::CalendarDateArith
            | Action::CalendarWeekdayInfo => PolicyDecision::Allow,
            _ => self.inner.allow(subject, action).await,
        }
    }
}

impl ChannelScopePolicy {
    fn scope_allowed(&self, subject: &Subject, action: &Action, scope: &MemoryScope) -> bool {
        let Some(scope_conv) = scope.conv.as_deref() else {
            return false;
        };
        if Some(scope_conv) == subject.attr("conv") {
            return true;
        }
        // Telegram-channel subjects: every conversation is a DM (1:1 chat
        // owner-by-chat-id). No public/group ambient like Matrix → no
        // cross-conv reads worth scoping. Allow.
        if subject.attr("channel") == Some("telegram") {
            return !is_write(action);
        }
        if is_write(action) {
            return false;
        }

        match (
            subject.attr("channel_kind"),
            subject.attr("channel_visibility"),
        ) {
            (Some("direct"), _) => {
                shared_room_ids(subject).any(|shared| shared == scope_conv)
                    || self.is_public_room(scope_conv)
            }
            (Some("group"), Some("public")) => self.is_public_room(scope_conv),
            _ => false,
        }
    }

    fn is_public_room(&self, scope_conv: &str) -> bool {
        parse_room_id_from_owner_str(scope_conv).is_ok_and(|room_id| {
            matches!(self.visibility.resolve(&room_id), RoomVisibility::Public)
        })
    }
}

const fn is_write(action: &Action) -> bool {
    matches!(
        action,
        Action::MemoryStore { .. }
            | Action::MemoryDelete { .. }
            | Action::RelationStore { .. }
            | Action::RelationDelete { .. }
            | Action::CronCreate { .. }
            | Action::CronUpdate { .. }
            | Action::CronDelete { .. }
    )
}

fn shared_room_ids(subject: &Subject) -> impl Iterator<Item = &str> {
    subject
        .attr("shared_room_ids")
        .unwrap_or("")
        .split(',')
        .filter(|room_id| !room_id.is_empty())
}

fn parse_room_id_from_owner_str(
    scope_conv: &str,
) -> Result<OwnedRoomId, crabgent_channel::ChannelError> {
    parse_owner_to_room_id(&Owner::new(scope_conv))
}
