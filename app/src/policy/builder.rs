use crabgent_channel::{CHANNEL_RECEIVE, ChannelPolicyExt, attr_keys};
use crabgent_core::policy::TargetPredicate;
use crabgent_core::policy::strict::{ActionMatcher, Rule, StrictPolicy};
use crabgent_tool_goal::{GOAL_ORIGIN_ATTR, GOAL_ORIGIN_SYSTEM, GOAL_ORIGIN_USER};

use super::MatrixPolicyConfig;

const MATRIX_CHANNEL: &str = "matrix";
const TELEGRAM_CHANNEL: &str = "telegram";
const SAFE_TOOLS: [&str; 21] = [
    "read_file",
    "cache_read",
    "memory",
    "session_search",
    "channel_send",
    "channel_react",
    "channel_edit",
    "channel_delete",
    "channel_read",
    "channel_upload",
    "vision_file",
    "notify_user",
    "task",
    "cron",
    "models",
    "calendar",
    "goal",
    "consolidate_memory",
    "agent_message",
    "voice_reply",
    "speak",
];

#[must_use]
pub fn build(config: &MatrixPolicyConfig) -> StrictPolicy {
    build_with_channels(config, true, false)
}

pub(super) fn build_strict_inner_for_scope(config: &MatrixPolicyConfig) -> StrictPolicy {
    build_strict_inner_for_scope_with_channels(config, true, false)
}

#[must_use]
pub fn build_with_channels(
    config: &MatrixPolicyConfig,
    has_matrix: bool,
    has_telegram: bool,
) -> StrictPolicy {
    let mut builder = build_base(config, has_matrix, has_telegram);
    builder = builder
        .allow_memory_any()
        .allow_relation_any()
        .allow_session_search()
        .allow_goal_get()
        .allow_goal_update()
        .allow_goal_manage()
        .allow_goal_create_for(GOAL_ORIGIN_ATTR, GOAL_ORIGIN_USER)
        .allow_goal_create_for(GOAL_ORIGIN_ATTR, GOAL_ORIGIN_SYSTEM);
    builder.deny_by_default().build()
}

pub(super) fn build_strict_inner_for_scope_with_channels(
    config: &MatrixPolicyConfig,
    has_matrix: bool,
    has_telegram: bool,
) -> StrictPolicy {
    build_base(config, has_matrix, has_telegram)
        .deny_by_default()
        .build()
}

fn build_base(
    config: &MatrixPolicyConfig,
    has_matrix: bool,
    has_telegram: bool,
) -> crabgent_core::policy::strict::StrictPolicyBuilder {
    let mut builder = StrictPolicy::builder().allow_llm_call();

    if has_matrix && !config.allowed_users.is_empty() {
        builder = builder.rule(
            Rule::allow(ActionMatcher::Targeted {
                name: CHANNEL_RECEIVE.to_owned(),
                qualifier: Some(MATRIX_CHANNEL.into()),
                target: TargetPredicate::Any,
            })
            .requires_attr_in(attr_keys::PARTICIPANT_ID, config.allowed_users.clone()),
        );
    }

    if has_telegram {
        // Telegram pairing gates user access at the inbox level (via
        // PairingInbox + pair_token), so we don't apply an allowed_users
        // attr filter on channel-receive here.
        builder = builder
            .allow_channel_receive(TELEGRAM_CHANNEL)
            .for_any_conv();
    }

    for tool in &config.restricted_tools {
        builder = builder.allow_tool_for(tool, attr_keys::CHANNEL_KIND, "direct");
    }
    for tool in SAFE_TOOLS {
        builder = builder.allow_tool(tool);
    }

    if has_matrix {
        builder = builder.allow_channel_send(MATRIX_CHANNEL).for_any_conv();
    }
    if has_telegram {
        builder = builder.allow_channel_send(TELEGRAM_CHANNEL).for_any_conv();
    }
    builder
}
