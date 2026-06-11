mod support;

use std::sync::Arc;

use crabgent_channel::{ChannelKind, InboundEvent, MessageRef, Participant, ParticipantRole};
use crabgent_channel_matrix::MatrixChannel;
use crabgent_core::{
    Action,
    memory::MemoryScope,
    owner::Owner,
    policy::{PolicyDecision, PolicyHook},
    subject::Subject,
};
use crabgent_runtime::{
    ChannelScopePolicy, MatrixPolicyConfig, MatrixVisibilityResolver, MembershipIndex,
    VisibilityResolver, build_scoped_subject_resolver, build_with_channel_scope,
    new_visibility_cache,
};
use matrix_sdk::ruma::OwnedRoomId;

type SharedVisibilityResolver = Arc<dyn VisibilityResolver + Send + Sync>;

struct ScopeSetup {
    policy: ChannelScopePolicy,
    channel: Arc<MatrixChannel>,
    visibility: SharedVisibilityResolver,
    membership: Arc<MembershipIndex>,
    alice: String,
    dm_room: OwnedRoomId,
    public_room: OwnedRoomId,
    other_public_room: OwnedRoomId,
    private_room: OwnedRoomId,
    shared_private_room: OwnedRoomId,
    other_private_room: OwnedRoomId,
}

impl ScopeSetup {
    async fn new() -> support::TestResult<Option<Self>> {
        let Some(ctx) = support::MatrixTestCtx::new().await? else {
            return Ok(None);
        };
        let bot = ctx.register_client("bot").await?;
        let alice = ctx.register_client("alice").await?;

        let dm_room =
            support::create_dm_room(&bot, alice.user_id().ok_or("alice is not logged in")?).await?;
        alice.join_room_by_id(&dm_room).await?;
        let public_room =
            support::create_public_room(&bot, &support::unique_localpart("pub")).await?;
        let other_public_room =
            support::create_public_room(&bot, &support::unique_localpart("pub")).await?;
        let private_room =
            support::create_private_room(&bot, &support::unique_localpart("priv")).await?;
        let shared_private_room =
            support::create_private_room(&bot, &support::unique_localpart("shared")).await?;
        let other_private_room =
            support::create_private_room(&bot, &support::unique_localpart("other")).await?;

        support::invite_and_join(&bot, &alice, &public_room).await?;
        support::invite_and_join(&bot, &alice, &private_room).await?;
        support::invite_and_join(&bot, &alice, &shared_private_room).await?;
        bot.sync_once(support::short_sync()).await?;
        support::warmup_visibility(&bot, &public_room).await?;
        support::warmup_visibility(&bot, &other_public_room).await?;

        let bot_user_id = bot.user_id().ok_or("bot is not logged in")?.to_owned();
        let channel = Arc::new(MatrixChannel::from_client(
            bot.clone(),
            bot_user_id.clone(),
            None,
        ));
        cache_kind(&channel, &dm_room, ChannelKind::Direct);
        for room_id in [
            &public_room,
            &other_public_room,
            &private_room,
            &shared_private_room,
            &other_private_room,
        ] {
            cache_kind(&channel, room_id, ChannelKind::Group);
        }

        let visibility: SharedVisibilityResolver = Arc::new(MatrixVisibilityResolver::new(
            bot.clone(),
            new_visibility_cache(),
        ));
        let membership = Arc::new(MembershipIndex::new(bot_user_id));
        membership.refresh(&bot).await?;
        let policy =
            build_with_channel_scope(&MatrixPolicyConfig::default(), Arc::clone(&visibility));

        Ok(Some(Self {
            policy,
            channel,
            visibility,
            membership,
            alice: alice.user_id().ok_or("alice is not logged in")?.to_string(),
            dm_room,
            public_room,
            other_public_room,
            private_room,
            shared_private_room,
            other_private_room,
        }))
    }

    fn subject(&self, room_id: &OwnedRoomId) -> Subject {
        let resolver = build_scoped_subject_resolver(
            Arc::clone(&self.channel),
            "agent".to_owned(),
            Arc::clone(&self.visibility),
            Arc::clone(&self.membership),
        );
        resolver(&inbound_event(room_id, &self.alice))
    }
}

fn cache_kind(channel: &MatrixChannel, room_id: &OwnedRoomId, kind: ChannelKind) {
    channel
        .kind_cache()
        .lock()
        .expect("matrix room-kind cache lock")
        .insert(room_id.clone(), kind);
}

fn inbound_event(room_id: &OwnedRoomId, user_id: &str) -> InboundEvent {
    let conv = Owner::new(owner_for_room(room_id));
    InboundEvent {
        channel: "matrix".to_owned(),
        conv: conv.clone(),
        kind: None,
        from: Participant::new(user_id, ParticipantRole::Human),
        message: MessageRef::top_level("matrix", conv, "$event:example.org"),
        body: "hello".to_owned(),
        timestamp: "1970-01-01T00:00:00Z".parse().expect("valid timestamp"),
        attachments: vec![],
    }
}

fn owner_for_room(room_id: &OwnedRoomId) -> String {
    format!("matrix:{room_id}")
}

fn read_action(room_id: &OwnedRoomId) -> Action {
    Action::MemorySearch {
        query: "needle".to_owned(),
        scope: MemoryScope::default().with_conv(owner_for_room(room_id)),
    }
}

fn write_action(room_id: &OwnedRoomId) -> Action {
    Action::MemoryStore {
        scope: MemoryScope::default().with_conv(owner_for_room(room_id)),
    }
}

async fn allowed(setup: &ScopeSetup, subject: &Subject, action: &Action) -> bool {
    matches!(
        setup.policy.allow(subject, action).await,
        PolicyDecision::Allow
    )
}

#[tokio::test]
async fn dm_subject_can_read_own_dm() -> support::TestResult {
    let Some(setup) = ScopeSetup::new().await? else {
        return Ok(());
    };
    let subject = setup.subject(&setup.dm_room);

    assert!(allowed(&setup, &subject, &read_action(&setup.dm_room)).await);
    Ok(())
}

#[tokio::test]
async fn dm_subject_can_read_shared_private() -> support::TestResult {
    let Some(setup) = ScopeSetup::new().await? else {
        return Ok(());
    };
    let subject = setup.subject(&setup.dm_room);

    assert!(allowed(&setup, &subject, &read_action(&setup.shared_private_room)).await);
    Ok(())
}

#[tokio::test]
async fn dm_subject_can_read_public() -> support::TestResult {
    let Some(setup) = ScopeSetup::new().await? else {
        return Ok(());
    };
    let subject = setup.subject(&setup.dm_room);

    assert!(allowed(&setup, &subject, &read_action(&setup.public_room)).await);
    Ok(())
}

#[tokio::test]
async fn dm_subject_cannot_read_different_private() -> support::TestResult {
    let Some(setup) = ScopeSetup::new().await? else {
        return Ok(());
    };
    let subject = setup.subject(&setup.dm_room);

    assert!(!allowed(&setup, &subject, &read_action(&setup.other_private_room)).await);
    Ok(())
}

#[tokio::test]
async fn public_subject_can_read_other_public() -> support::TestResult {
    let Some(setup) = ScopeSetup::new().await? else {
        return Ok(());
    };
    let subject = setup.subject(&setup.public_room);

    assert!(allowed(&setup, &subject, &read_action(&setup.other_public_room)).await);
    Ok(())
}

#[tokio::test]
async fn public_subject_cannot_read_private() -> support::TestResult {
    let Some(setup) = ScopeSetup::new().await? else {
        return Ok(());
    };
    let subject = setup.subject(&setup.public_room);

    assert!(!allowed(&setup, &subject, &read_action(&setup.private_room)).await);
    Ok(())
}

#[tokio::test]
async fn public_subject_cannot_read_dm() -> support::TestResult {
    let Some(setup) = ScopeSetup::new().await? else {
        return Ok(());
    };
    let subject = setup.subject(&setup.public_room);

    assert!(!allowed(&setup, &subject, &read_action(&setup.dm_room)).await);
    Ok(())
}

#[tokio::test]
async fn private_subject_can_read_same_channel() -> support::TestResult {
    let Some(setup) = ScopeSetup::new().await? else {
        return Ok(());
    };
    let subject = setup.subject(&setup.private_room);

    assert!(allowed(&setup, &subject, &read_action(&setup.private_room)).await);
    Ok(())
}

#[tokio::test]
async fn private_subject_cannot_read_other_private() -> support::TestResult {
    let Some(setup) = ScopeSetup::new().await? else {
        return Ok(());
    };
    let subject = setup.subject(&setup.private_room);

    assert!(!allowed(&setup, &subject, &read_action(&setup.other_private_room)).await);
    Ok(())
}

#[tokio::test]
async fn dm_subject_cannot_write_to_public() -> support::TestResult {
    let Some(setup) = ScopeSetup::new().await? else {
        return Ok(());
    };
    let subject = setup.subject(&setup.dm_room);

    assert!(!allowed(&setup, &subject, &write_action(&setup.public_room)).await);
    Ok(())
}
