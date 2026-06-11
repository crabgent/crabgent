//! Channel-action constants and helpers for `PolicyHook` integration.

use crabgent_core::action::{Action, ActionTarget};
use crabgent_core::owner::Owner;
use crabgent_core::policy::TargetPredicate;
use crabgent_core::policy::{ActionMatcher, Rule, StrictPolicy, StrictPolicyBuilder};

/// Action name for `Channel::send` calls.
pub const CHANNEL_SEND: &str = "channel.send";

/// Action name for inbound message dispatch.
pub const CHANNEL_RECEIVE: &str = "channel.receive";

/// Action name for `Channel::participants` lookups.
pub const CHANNEL_LIST_PARTICIPANTS: &str = "channel.list_participants";

/// Build the `Action` for a `Channel::send` call.
#[must_use]
pub fn channel_send_action(channel: Option<&str>, conv: &Owner) -> Action {
    channel_action(CHANNEL_SEND, channel, conv)
}

/// Build the `Action` for an inbound message dispatch.
#[must_use]
pub fn channel_receive_action(channel: &str, conv: &Owner) -> Action {
    channel_action(CHANNEL_RECEIVE, Some(channel), conv)
}

/// Build the `Action` for a `Channel::participants` lookup.
#[must_use]
pub fn channel_list_participants_action(channel: &str, conv: &Owner) -> Action {
    channel_action(CHANNEL_LIST_PARTICIPANTS, Some(channel), conv)
}

fn channel_action(name: &str, channel: Option<&str>, conv: &Owner) -> Action {
    let target = channel.map_or_else(
        || ActionTarget::new(conv.clone()),
        |channel| ActionTarget::new(conv.clone()).with_qualifier(channel),
    );
    Action::targeted(name, target)
}

/// Converts a channel filter argument into an optional exact channel name.
pub trait IntoChannelFilter {
    fn into_channel_filter(self) -> Option<String>;
}

impl IntoChannelFilter for &str {
    fn into_channel_filter(self) -> Option<String> {
        Some(self.to_owned())
    }
}

impl IntoChannelFilter for String {
    fn into_channel_filter(self) -> Option<String> {
        Some(self)
    }
}

impl IntoChannelFilter for Option<&str> {
    fn into_channel_filter(self) -> Option<String> {
        self.map(str::to_owned)
    }
}

impl IntoChannelFilter for Option<String> {
    fn into_channel_filter(self) -> Option<String> {
        self
    }
}

#[derive(Debug, Clone, Copy)]
enum ChannelRuleKind {
    Send,
    Receive,
    ListParticipants,
}

impl ChannelRuleKind {
    const fn action_name(self) -> &'static str {
        match self {
            Self::Send => CHANNEL_SEND,
            Self::Receive => CHANNEL_RECEIVE,
            Self::ListParticipants => CHANNEL_LIST_PARTICIPANTS,
        }
    }

    fn matcher(self, channel: Option<String>, target: TargetPredicate) -> ActionMatcher {
        ActionMatcher::Targeted {
            name: self.action_name().to_owned(),
            qualifier: channel,
            target,
        }
    }
}

/// Builder returned by channel allow methods so callers can scope `conv`.
pub struct ChannelRuleBuilder {
    builder: StrictPolicyBuilder,
    kind: ChannelRuleKind,
    channel: Option<String>,
}

impl ChannelRuleBuilder {
    const fn new(
        builder: StrictPolicyBuilder,
        kind: ChannelRuleKind,
        channel: Option<String>,
    ) -> Self {
        Self {
            builder,
            kind,
            channel,
        }
    }

    /// Allow any conversation that matches the optional channel filter.
    pub fn for_any_conv(self) -> StrictPolicyBuilder {
        self.finish(TargetPredicate::Any)
    }

    /// Allow exactly one conversation owner.
    pub fn for_conv_exact(self, conv: impl Into<Owner>) -> StrictPolicyBuilder {
        self.finish(TargetPredicate::Exact(conv.into()))
    }

    /// Allow conversations whose owner string starts with `prefix`.
    pub fn for_conv_prefix(self, prefix: impl Into<String>) -> StrictPolicyBuilder {
        self.finish(TargetPredicate::Prefix(prefix.into()))
    }

    /// Finish the policy immediately. Uses any-conversation matching.
    pub fn build(self) -> StrictPolicy {
        self.for_any_conv().build()
    }

    fn finish(self, target: TargetPredicate) -> StrictPolicyBuilder {
        self.builder
            .rule(Rule::allow(self.kind.matcher(self.channel, target)))
    }
}

/// Strict-policy channel convenience methods.
pub trait ChannelPolicyExt {
    fn allow_channel_send<C>(self, channel: C) -> ChannelRuleBuilder
    where
        C: IntoChannelFilter;

    fn allow_channel_receive<C>(self, channel: C) -> ChannelRuleBuilder
    where
        C: IntoChannelFilter;

    fn allow_channel_list_participants<C>(self, channel: C) -> ChannelRuleBuilder
    where
        C: IntoChannelFilter;
}

impl ChannelPolicyExt for StrictPolicyBuilder {
    fn allow_channel_send<C>(self, channel: C) -> ChannelRuleBuilder
    where
        C: IntoChannelFilter,
    {
        ChannelRuleBuilder::new(self, ChannelRuleKind::Send, channel.into_channel_filter())
    }

    fn allow_channel_receive<C>(self, channel: C) -> ChannelRuleBuilder
    where
        C: IntoChannelFilter,
    {
        ChannelRuleBuilder::new(
            self,
            ChannelRuleKind::Receive,
            channel.into_channel_filter(),
        )
    }

    fn allow_channel_list_participants<C>(self, channel: C) -> ChannelRuleBuilder
    where
        C: IntoChannelFilter,
    {
        ChannelRuleBuilder::new(
            self,
            ChannelRuleKind::ListParticipants,
            channel.into_channel_filter(),
        )
    }
}

#[must_use]
pub(crate) fn channel_name_from_owner(conv: &Owner) -> Option<&str> {
    conv.as_str()
        .split_once(':')
        .and_then(|(name, rest)| (!name.is_empty() && !rest.is_empty()).then_some(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_send_action_carries_conv() {
        let conv = Owner::new("slack:T1/C1");
        let a = channel_send_action(Some("slack"), &conv);
        assert_eq!(a.name(), CHANNEL_SEND);
        assert_eq!(
            a,
            Action::targeted(
                CHANNEL_SEND,
                ActionTarget::new(conv).with_qualifier("slack")
            )
        );
    }

    #[test]
    fn channel_receive_action_carries_conv() {
        let conv = Owner::new("slack:T1/C1");
        let a = channel_receive_action("slack", &conv);
        assert_eq!(a.name(), CHANNEL_RECEIVE);
        assert_eq!(
            a,
            Action::targeted(
                CHANNEL_RECEIVE,
                ActionTarget::new(conv).with_qualifier("slack")
            )
        );
    }

    #[test]
    fn channel_list_participants_action_carries_conv() {
        let conv = Owner::new("slack:T1/C1");
        let a = channel_list_participants_action("slack", &conv);
        assert_eq!(a.name(), CHANNEL_LIST_PARTICIPANTS);
        assert_eq!(
            a,
            Action::targeted(
                CHANNEL_LIST_PARTICIPANTS,
                ActionTarget::new(conv).with_qualifier("slack")
            )
        );
    }

    #[test]
    fn action_constants_are_stable_strings() {
        assert_eq!(CHANNEL_SEND, "channel.send");
        assert_eq!(CHANNEL_RECEIVE, "channel.receive");
        assert_eq!(CHANNEL_LIST_PARTICIPANTS, "channel.list_participants");
    }

    #[test]
    fn actions_are_distinct_by_name() {
        assert_ne!(
            channel_send_action(None, &Owner::new("stub:c")).name(),
            channel_receive_action("stub", &Owner::new("stub:c")).name()
        );
        assert_ne!(
            channel_receive_action("stub", &Owner::new("stub:c")).name(),
            channel_list_participants_action("stub", &Owner::new("stub:c")).name()
        );
    }

    #[test]
    fn channel_name_from_owner_reads_non_empty_prefix() {
        assert_eq!(
            channel_name_from_owner(&Owner::new("slack:T1/C1")),
            Some("slack")
        );
        assert_eq!(channel_name_from_owner(&Owner::new(":bad")), None);
        assert_eq!(channel_name_from_owner(&Owner::new("missing")), None);
    }

    #[tokio::test]
    async fn channel_send_can_match_exact_conv() {
        use crabgent_core::policy::{PolicyDecision, PolicyHook, StrictPolicy};
        use crabgent_core::subject::Subject;

        let p = StrictPolicy::builder()
            .allow_channel_send("stub")
            .for_conv_exact("stub:c1")
            .build();
        let s = Subject::new("u");
        assert!(matches!(
            p.allow(
                &s,
                &channel_send_action(Some("stub"), &Owner::new("stub:c1"))
            )
            .await,
            PolicyDecision::Allow
        ));
        assert!(matches!(
            p.allow(
                &s,
                &channel_send_action(Some("stub"), &Owner::new("stub:c2"))
            )
            .await,
            PolicyDecision::Deny(_)
        ));
    }

    #[tokio::test]
    async fn channel_send_can_match_conv_prefix() {
        use crabgent_core::policy::{PolicyDecision, PolicyHook, StrictPolicy};
        use crabgent_core::subject::Subject;

        let p = StrictPolicy::builder()
            .allow_channel_send("stub")
            .for_conv_prefix("stub:T1/")
            .build();
        let s = Subject::new("u");
        assert!(matches!(
            p.allow(
                &s,
                &channel_send_action(Some("stub"), &Owner::new("stub:T1/C1"))
            )
            .await,
            PolicyDecision::Allow
        ));
        assert!(matches!(
            p.allow(
                &s,
                &channel_send_action(Some("stub"), &Owner::new("stub:T2/C1"))
            )
            .await,
            PolicyDecision::Deny(_)
        ));
    }

    #[tokio::test]
    async fn channel_filter_blocks_other_channels() {
        use crabgent_core::policy::{PolicyDecision, PolicyHook, StrictPolicy};
        use crabgent_core::subject::Subject;

        let p = StrictPolicy::builder()
            .allow_channel_send("stub")
            .for_any_conv()
            .build();
        let s = Subject::new("u");
        assert!(matches!(
            p.allow(
                &s,
                &channel_send_action(Some("other"), &Owner::new("other:c1"))
            )
            .await,
            PolicyDecision::Deny(_)
        ));
    }

    #[tokio::test]
    async fn channel_receive_and_participants_are_distinct() {
        use crabgent_core::policy::{PolicyDecision, PolicyHook, StrictPolicy};
        use crabgent_core::subject::Subject;

        let p = StrictPolicy::builder()
            .allow_channel_receive("stub")
            .for_conv_exact("stub:c1")
            .allow_channel_list_participants("stub")
            .for_conv_exact("stub:c2")
            .build();
        let s = Subject::new("u");
        assert!(matches!(
            p.allow(&s, &channel_receive_action("stub", &Owner::new("stub:c1")))
                .await,
            PolicyDecision::Allow
        ));
        assert!(matches!(
            p.allow(
                &s,
                &channel_list_participants_action("stub", &Owner::new("stub:c2"))
            )
            .await,
            PolicyDecision::Allow
        ));
        assert!(matches!(
            p.allow(
                &s,
                &channel_list_participants_action("stub", &Owner::new("stub:c1"))
            )
            .await,
            PolicyDecision::Deny(_)
        ));
    }
}
