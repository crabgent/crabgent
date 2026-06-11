use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    CHANNEL_RECEIVE, ChannelKind, InboundEvent, MessageRef, Participant, ParticipantRole,
    attr_keys, channel_receive_action,
};
use crabgent_channel_matrix::{MatrixChannel, build_subject_resolver};
use crabgent_core::{
    action::Action,
    owner::Owner,
    policy::{PolicyDecision, PolicyHook},
    subject::Subject,
};
use matrix_sdk::{Client, ruma::owned_user_id};
use url::Url;

struct ParticipantAllowlistPolicy {
    allowed: HashSet<String>,
}

#[async_trait]
impl PolicyHook for ParticipantAllowlistPolicy {
    async fn allow(&self, subject: &Subject, action: &Action) -> PolicyDecision {
        match action {
            Action::Targeted { name, target }
                if name == CHANNEL_RECEIVE && target.qualifier() == Some("matrix") =>
            {
                subject
                    .attr(attr_keys::PARTICIPANT_ID)
                    .filter(|participant_id| self.allowed.contains(*participant_id))
                    .map_or_else(
                        || PolicyDecision::Deny("matrix participant is not allowed".into()),
                        |_| PolicyDecision::Allow,
                    )
            }
            _ => PolicyDecision::Allow,
        }
    }
}

async fn build_channel() -> Arc<MatrixChannel> {
    let client = Client::new(Url::parse("https://example.org").expect("test result"))
        .await
        .expect("test result");
    let channel = Arc::new(MatrixChannel::from_client(
        client,
        owned_user_id!("@bot:example.org"),
        None,
    ));
    channel
        .kind_cache()
        .lock()
        .expect("matrix room-kind cache lock should not be poisoned")
        .insert(
            "!room:example.org".try_into().expect("test result"),
            ChannelKind::Direct,
        );
    channel
}

fn inbound_event(participant_id: &str) -> InboundEvent {
    let conv = Owner::new("matrix:!room:example.org");
    InboundEvent {
        channel: "matrix".into(),
        conv: conv.clone(),
        kind: None,
        from: Participant::new(participant_id, ParticipantRole::Human),
        message: MessageRef::top_level("matrix", conv, "$event:example.org"),
        body: "hello".into(),
        attachments: vec![],
        timestamp: Utc::now(),
    }
}

#[tokio::test]
async fn policy_hook_allows_allowlisted_matrix_participant() {
    let channel = build_channel().await;
    let resolver = build_subject_resolver(channel, "nova".into());
    let subject = resolver(&inbound_event("@alice:example.org"));
    let policy = ParticipantAllowlistPolicy {
        allowed: HashSet::from(["@alice:example.org".to_owned()]),
    };
    let action = channel_receive_action("matrix", &Owner::new("matrix:!room:example.org"));

    assert!(matches!(
        policy.allow(&subject, &action).await,
        PolicyDecision::Allow
    ));
}

#[tokio::test]
async fn policy_hook_denies_non_allowlisted_matrix_participant() {
    let channel = build_channel().await;
    let resolver = build_subject_resolver(channel, "nova".into());
    let subject = resolver(&inbound_event("@mallory:example.org"));
    let policy = ParticipantAllowlistPolicy {
        allowed: HashSet::from(["@alice:example.org".to_owned()]),
    };
    let action = channel_receive_action("matrix", &Owner::new("matrix:!room:example.org"));

    assert!(matches!(
        policy.allow(&subject, &action).await,
        PolicyDecision::Deny(reason) if reason == "matrix participant is not allowed"
    ));
}
