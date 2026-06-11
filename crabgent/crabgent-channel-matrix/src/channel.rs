//! Matrix `Channel` implementation.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use crabgent_channel::{ChannelError, ChannelKind, Participant, ParticipantId, ParticipantRole};
use matrix_sdk::ruma::{OwnedDeviceId, OwnedUserId};
use matrix_sdk::{
    Client, SessionMeta, SessionTokens, authentication::matrix::MatrixSession, ruma::OwnedRoomId,
};
use secrecy::ExposeSecret;

use crate::{
    config::{DEFAULT_BODY_CAP_BYTES, MatrixAuth, MatrixChannelConfig},
    error::MatrixChannelError,
};

mod ops_existing;

/// Shared room-kind cache populated by send/sync paths.
pub type RoomKindCache = Arc<Mutex<HashMap<OwnedRoomId, ChannelKind>>>;

/// Matrix adapter for group and direct rooms.
pub struct MatrixChannel {
    client: Client,
    bot_user_id: OwnedUserId,
    bot_display_name: Option<String>,
    body_cap_bytes: usize,
    kind_cache: RoomKindCache,
}

impl MatrixChannel {
    /// Build and authenticate a Matrix SDK client from config.
    #[crabgent_log::instrument(
        level = "debug",
        skip(config),
        fields(homeserver = %config.homeserver, user = %config.user)
    )]
    pub async fn new(config: MatrixChannelConfig) -> Result<Self, MatrixChannelError> {
        let client = Client::new(config.homeserver.clone())
            .await
            .map_err(|err| MatrixChannelError::Config(err.to_string()))?;
        match &config.auth {
            MatrixAuth::Password { password } => {
                client
                    .matrix_auth()
                    .login_username(config.user.as_str(), password.expose_secret())
                    .send()
                    .await
                    .map_err(|err| MatrixChannelError::Login(err.to_string()))?;
            }
            MatrixAuth::AccessToken {
                access_token,
                device_id,
            } => {
                let session = MatrixSession {
                    meta: SessionMeta {
                        user_id: config.user.clone(),
                        device_id: OwnedDeviceId::from(device_id.as_str()),
                    },
                    tokens: SessionTokens {
                        access_token: access_token.expose_secret().to_owned(),
                        refresh_token: None,
                    },
                };
                client
                    .restore_session(session)
                    .await
                    .map_err(|err| MatrixChannelError::Login(err.to_string()))?;
            }
        }
        Ok(Self::from_client_with_body_cap_bytes(
            client,
            config.user,
            config.bot_display_name,
            config.body_cap_bytes,
        ))
    }

    /// Build from an already-authenticated SDK client.
    pub fn from_client(
        client: Client,
        bot_user_id: OwnedUserId,
        bot_display_name: Option<String>,
    ) -> Self {
        Self::from_client_with_body_cap_bytes(
            client,
            bot_user_id,
            bot_display_name,
            DEFAULT_BODY_CAP_BYTES,
        )
    }

    /// Build from an already-authenticated SDK client with an outbound body cap.
    pub fn from_client_with_body_cap_bytes(
        client: Client,
        bot_user_id: OwnedUserId,
        bot_display_name: Option<String>,
        body_cap_bytes: usize,
    ) -> Self {
        Self {
            client,
            bot_user_id,
            bot_display_name,
            body_cap_bytes,
            kind_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Borrow the SDK client for poller setup and consumer integrations.
    #[must_use]
    pub const fn client(&self) -> &Client {
        &self.client
    }

    /// Borrow the bot user id.
    #[must_use]
    pub const fn bot_user_id(&self) -> &OwnedUserId {
        &self.bot_user_id
    }

    /// Clone the room-kind cache handle for subject resolution.
    #[must_use]
    pub fn kind_cache(&self) -> RoomKindCache {
        Arc::clone(&self.kind_cache)
    }

    /// Prefetch and cache the kind for a room.
    #[crabgent_log::instrument(level = "debug", skip(self), fields(room_id = %room_id))]
    pub async fn prefetch_kind(&self, room_id: &OwnedRoomId) -> Result<ChannelKind, ChannelError> {
        let cached_kind = {
            let cache = self
                .kind_cache
                .lock()
                .map_err(|err| ChannelError::adapter(err.to_string()))?;
            cache.get(room_id).copied()
        };
        if let Some(kind) = cached_kind {
            return Ok(kind);
        }
        let room = self
            .client
            .get_room(room_id)
            .ok_or_else(|| ChannelError::ConversationNotFound(room_id.to_string()))?;
        let is_direct = room.is_direct().await.map_err(ChannelError::adapter)?;
        let kind = if is_direct || room.active_members_count() <= 2 {
            ChannelKind::Direct
        } else {
            ChannelKind::Group
        };
        self.kind_cache
            .lock()
            .map_err(|err| ChannelError::adapter(err.to_string()))?
            .insert(room_id.clone(), kind);
        Ok(kind)
    }

    pub(super) fn bot_participant(&self) -> Participant {
        let participant = Participant::new(
            ParticipantId::new(self.bot_user_id.to_string()),
            ParticipantRole::Bot,
        );
        match self.bot_display_name.as_ref() {
            Some(name) => participant.with_display_name(name),
            None => participant,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbound::CHANNEL_NAME;
    use crabgent_channel::{Channel, MessageRef, OutboundMessage};
    use crabgent_core::{owner::Owner, subject::Subject};
    use matrix_sdk::ruma::owned_user_id;
    use url::Url;

    async fn build_channel() -> MatrixChannel {
        let client = Client::new(Url::parse("https://example.org").expect("test result"))
            .await
            .expect("test result");
        MatrixChannel::from_client(
            client,
            owned_user_id!("@bot:example.org"),
            Some("Nova".into()),
        )
    }

    #[tokio::test]
    async fn from_client_preserves_bot_identity() {
        let channel = build_channel().await;
        assert_eq!(channel.name(), CHANNEL_NAME);
        assert_eq!(channel.bot_user_id().as_str(), "@bot:example.org");
    }

    #[tokio::test]
    async fn invalid_owner_fails_before_network_paths() {
        let channel = build_channel().await;
        let ctx = Subject::new("agent");
        let conv = Owner::new("matrix:not-a-room-id");
        assert!(matches!(
            channel.kind(&conv).await,
            Err(ChannelError::InvalidOwnerFormat(_))
        ));
        assert!(matches!(
            channel.participants(&ctx, &conv).await,
            Err(ChannelError::InvalidOwnerFormat(_))
        ));
        assert!(matches!(
            channel.send(&ctx, &conv, &OutboundMessage::new("hi")).await,
            Err(ChannelError::InvalidOwnerFormat(_))
        ));
        let parent = MessageRef::top_level(
            CHANNEL_NAME,
            Owner::new("matrix:!room:example.org"),
            "$event:example.org",
        );
        assert!(matches!(
            channel.react(&ctx, &conv, &parent, "👀").await,
            Err(ChannelError::InvalidOwnerFormat(_))
        ));
        assert!(matches!(
            channel.direct_role(&conv).await,
            Err(ChannelError::InvalidOwnerFormat(_))
        ));
    }

    #[tokio::test]
    async fn known_room_missing_is_conversation_not_found() {
        let channel = build_channel().await;
        let conv = Owner::new("matrix:!missing:example.org");
        assert!(matches!(
            channel.kind(&conv).await,
            Err(ChannelError::ConversationNotFound(_))
        ));
    }

    #[tokio::test]
    async fn kind_cache_handle_is_shared() {
        let channel = build_channel().await;
        let room = OwnedRoomId::try_from("!cached:example.org").expect("test result");
        channel
            .kind_cache()
            .lock()
            .expect("matrix room-kind cache lock should not be poisoned")
            .insert(room.clone(), ChannelKind::Group);
        assert_eq!(
            channel
                .kind_cache()
                .lock()
                .expect("matrix room-kind cache lock should not be poisoned")
                .get(&room)
                .copied(),
            Some(ChannelKind::Group)
        );
    }

    #[tokio::test]
    async fn access_token_config_restores_client_session() {
        let config = MatrixChannelConfig {
            homeserver: Url::parse("https://example.org").expect("test result"),
            user: owned_user_id!("@bot:example.org"),
            auth: MatrixAuth::AccessToken {
                access_token: secrecy::SecretString::from("test-token".to_owned()),
                device_id: "DEVICEID".into(),
            },
            bot_display_name: None,
            body_cap_bytes: DEFAULT_BODY_CAP_BYTES,
        };
        let channel = MatrixChannel::new(config).await.expect("test result");
        assert_eq!(channel.bot_user_id().as_str(), "@bot:example.org");
    }

    #[tokio::test]
    async fn conv_display_yields_homeserver_when_room_state_absent() {
        // The test client carries no room state, so `room.name()` is
        // unreachable and `name` stays `None`. The homeserver still flows
        // from the server part of the parsed room id, proving the label is
        // best-effort: a missing name never suppresses the workspace.
        let channel = build_channel().await;
        let conv = Owner::new("matrix:!room:example.org");
        let label = channel
            .conv_display(&conv)
            .await
            .expect("homeserver label present even without room state");
        assert_eq!(label.name, None);
        assert_eq!(label.workspace.as_deref(), Some("example.org"));
    }

    #[tokio::test]
    async fn conv_display_none_for_malformed_owner() {
        let channel = build_channel().await;
        let conv = Owner::new("matrix:not-a-room-id");
        assert!(channel.conv_display(&conv).await.is_none());
    }

    #[tokio::test]
    async fn valid_room_missing_fails_send_and_participants() {
        let channel = build_channel().await;
        let ctx = Subject::new("agent");
        let conv = Owner::new("matrix:!missing:example.org");
        assert!(matches!(
            channel.participants(&ctx, &conv).await,
            Err(ChannelError::ConversationNotFound(_))
        ));
        assert!(matches!(
            channel.send(&ctx, &conv, &OutboundMessage::new("hi")).await,
            Err(ChannelError::ConversationNotFound(_))
        ));
        let parent = MessageRef::top_level(CHANNEL_NAME, conv.clone(), "$event:example.org");
        assert!(matches!(
            channel.react(&ctx, &conv, &parent, "👀").await,
            Err(ChannelError::ConversationNotFound(_))
        ));
    }
}
