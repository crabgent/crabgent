#![expect(
    dead_code,
    reason = "shared integration-test helpers are used per test target"
)]

use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use crabgent_channel::{ChannelInbox, InboundEvent, InboundReaction};
use crabgent_channel_matrix::{MatrixChannel, MatrixSyncPoller};
use matrix_sdk::{
    Client,
    config::SyncSettings,
    ruma::{
        OwnedEventId, OwnedRoomId, RoomId,
        api::client::{
            account::register,
            room::create_room::{self, v3::RoomPreset},
            uiaa::{AuthData, Dummy},
        },
        events::room::message::{
            MessageType, OriginalSyncRoomMessageEvent, Relation, RoomMessageEventContent,
        },
    },
};
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use url::Url;

mod conduit_config;

use conduit_config::{CONDUIT_IMAGE, CONDUIT_PORT, CONDUIT_TAG};

const PASSWORD: &str = "correct-horse-battery-staple";
static NEXT_USER: AtomicU64 = AtomicU64::new(1);
static SHARED_CONDUIT: tokio::sync::OnceCell<Arc<SharedConduit>> =
    tokio::sync::OnceCell::const_new();

pub type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

struct SharedConduit {
    _container: ContainerAsync<GenericImage>,
    homeserver_url: String,
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "integration tests keep this helper crate-visible for the locked matrix_test_ctx fixture API"
)]
pub(crate) struct MatrixTestCtx {
    pub(crate) homeserver_url: String,
    pub(crate) user: String,
    pub(crate) password: String,
    _conduit: Option<Arc<SharedConduit>>,
}

async fn shared_conduit() -> TestResult<Arc<SharedConduit>> {
    let conduit = SHARED_CONDUIT
        .get_or_try_init(|| async {
            let image = GenericImage::new(CONDUIT_IMAGE, CONDUIT_TAG)
                .with_exposed_port(CONDUIT_PORT.tcp())
                .with_wait_for(WaitFor::seconds(5))
                .with_env_var("CONDUIT_SERVER_NAME", "localhost")
                .with_env_var("CONDUIT_ALLOW_REGISTRATION", "true")
                .with_env_var("CONDUIT_DATABASE_BACKEND", "rocksdb")
                .with_env_var("CONDUIT_ALLOW_FEDERATION", "false");
            let container = image.start().await?;
            let port = container.get_host_port_ipv4(CONDUIT_PORT.tcp()).await?;
            Ok::<_, Box<dyn Error + Send + Sync>>(Arc::new(SharedConduit {
                _container: container,
                homeserver_url: format!("http://127.0.0.1:{port}"),
            }))
        })
        .await?;
    Ok(Arc::clone(conduit))
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "integration tests keep this helper crate-visible for the locked matrix_test_ctx fixture API"
)]
pub(crate) async fn matrix_test_ctx() -> TestResult<Option<MatrixTestCtx>> {
    if let (Ok(homeserver_url), Ok(user), Ok(password)) = (
        std::env::var("MATRIX_HOMESERVER"),
        std::env::var("MATRIX_USER"),
        std::env::var("MATRIX_PASSWORD"),
    ) {
        return Ok(Some(MatrixTestCtx {
            homeserver_url,
            user,
            password,
            _conduit: None,
        }));
    }

    let conduit = match shared_conduit().await {
        Ok(conduit) => conduit,
        Err(err) => {
            crabgent_log::warn!(
                "matrix conduit container unavailable; skipping integration test: {err}"
            );
            return Ok(None);
        }
    };

    Ok(Some(MatrixTestCtx {
        homeserver_url: conduit.homeserver_url.clone(),
        user: unique_localpart("bot"),
        password: PASSWORD.to_owned(),
        _conduit: Some(conduit),
    }))
}

impl MatrixTestCtx {
    #[expect(
        clippy::used_underscore_binding,
        reason = "_conduit is the locked lifetime guard field and also marks the real-server path"
    )]
    pub async fn register_client(&self, prefix: &str) -> TestResult<Client> {
        let client = Client::new(Url::parse(&self.homeserver_url)?).await?;
        if prefix == "bot" && self._conduit.is_none() {
            client
                .matrix_auth()
                .login_username(&self.user, &self.password)
                .send()
                .await?;
            return Ok(client);
        }
        let localpart = if prefix == "bot" {
            self.user.clone()
        } else {
            unique_localpart(prefix)
        };
        let mut request = register::v3::Request::new();
        request.username = Some(localpart.clone());
        request.password = Some(self.password.clone());
        request.auth = Some(AuthData::Dummy(Dummy::new()));
        client.matrix_auth().register(request).await?;
        if client.user_id().is_none() {
            client
                .matrix_auth()
                .login_username(&localpart, &self.password)
                .send()
                .await?;
        }
        Ok(client)
    }
}

pub struct JoinedRoomFixture {
    pub channel: Arc<MatrixChannel>,
    pub bot: Client,
    pub alice: Client,
    pub bob: Option<Client>,
    pub room_id: OwnedRoomId,
}

pub async fn joined_room(member_count: usize) -> TestResult<Option<JoinedRoomFixture>> {
    let Some(ctx) = matrix_test_ctx().await? else {
        return Ok(None);
    };
    let bot = ctx.register_client("bot").await?;
    let alice = ctx.register_client("alice").await?;
    let bob = if member_count > 2 {
        Some(ctx.register_client("bob").await?)
    } else {
        None
    };

    let mut request = create_room::v3::Request::new();
    request.preset = Some(RoomPreset::PrivateChat);
    request.name = Some(unique_localpart("room"));
    let room = bot.create_room(request).await?;
    room.invite_user_by_id(alice.user_id().expect("alice logged in"))
        .await?;
    alice.join_room_by_id(room.room_id()).await?;
    if let Some(bob_client) = bob.as_ref() {
        room.invite_user_by_id(bob_client.user_id().expect("bob logged in"))
            .await?;
        bob_client.join_room_by_id(room.room_id()).await?;
    }
    let _ = bot.sync_once(short_sync()).await?;
    let channel = Arc::new(MatrixChannel::from_client(
        bot.clone(),
        bot.user_id().expect("bot logged in").to_owned(),
        Some("Nova".into()),
    ));
    Ok(Some(JoinedRoomFixture {
        channel,
        bot,
        alice,
        bob,
        room_id: room.room_id().to_owned(),
    }))
}

pub async fn dm_room() -> TestResult<Option<JoinedRoomFixture>> {
    let Some(ctx) = matrix_test_ctx().await? else {
        return Ok(None);
    };
    let bot = ctx.register_client("bot").await?;
    let alice = ctx.register_client("alice").await?;
    let room = bot
        .create_dm(alice.user_id().expect("alice logged in"))
        .await?;
    alice.join_room_by_id(room.room_id()).await?;
    let _ = bot.sync_once(short_sync()).await?;
    let channel = Arc::new(MatrixChannel::from_client(
        bot.clone(),
        bot.user_id().expect("bot logged in").to_owned(),
        None,
    ));
    Ok(Some(JoinedRoomFixture {
        channel,
        bot,
        alice,
        bob: None,
        room_id: room.room_id().to_owned(),
    }))
}

pub async fn send_from(client: &Client, room_id: &RoomId, body: &str) -> TestResult<OwnedEventId> {
    let room = client
        .get_room(room_id)
        .ok_or_else(|| format!("client is not in room {room_id}"))?;
    Ok(room
        .send(RoomMessageEventContent::text_plain(body))
        .await?
        .response
        .event_id)
}

pub async fn wait_for_room_message(
    client: &Client,
    room_id: &RoomId,
    body: &str,
) -> TestResult<OriginalSyncRoomMessageEvent> {
    wait_for_room_message_matching(client, room_id, |event| match &event.content.msgtype {
        MessageType::Text(text) => text.body == body,
        _ => false,
    })
    .await
}

pub async fn wait_for_room_message_matching(
    client: &Client,
    room_id: &RoomId,
    predicate: impl Fn(&OriginalSyncRoomMessageEvent) -> bool,
) -> TestResult<OriginalSyncRoomMessageEvent> {
    for _ in 0..20 {
        let response = client.sync_once(short_sync()).await?;
        if let Some(joined) = response.rooms.joined.get(room_id) {
            for event in &joined.timeline.events {
                let Ok(event) = event
                    .raw()
                    .deserialize_as_unchecked::<OriginalSyncRoomMessageEvent>()
                else {
                    continue;
                };
                if predicate(&event) {
                    return Ok(event);
                }
            }
        }
    }
    Err(format!("timed out waiting for room message in {room_id}").into())
}

pub fn is_thread_reply(event: &OriginalSyncRoomMessageEvent, root: &OwnedEventId) -> bool {
    matches!(
        event.content.relates_to.as_ref(),
        Some(Relation::Thread(thread)) if thread.event_id == *root
    )
}

pub async fn collect_one_inbound(
    channel: Arc<MatrixChannel>,
    cancel: CancellationToken,
) -> TestResult<InboundEvent> {
    let (tx, mut rx) = mpsc::channel(4);
    let (tx_reaction, _rx_reaction) = mpsc::channel::<InboundReaction>(4);
    let poller = MatrixSyncPoller::new(channel, Arc::new(RecordingInbox::new(tx, tx_reaction)))
        .with_sync_timeout(Duration::from_millis(500));
    let handle = poller.start(cancel.clone());
    let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await?
        .ok_or("matrix poller did not produce inbound event")?;
    cancel.cancel();
    handle.await??;
    Ok(event)
}

pub async fn collect_one_inbound_reaction(
    channel: Arc<MatrixChannel>,
    cancel: CancellationToken,
) -> TestResult<InboundReaction> {
    let (tx, _rx) = mpsc::channel::<InboundEvent>(8);
    let (tx_reaction, mut rx_reaction) = mpsc::channel(4);
    let poller = MatrixSyncPoller::new(channel, Arc::new(RecordingInbox::new(tx, tx_reaction)))
        .with_sync_timeout(Duration::from_millis(500));
    let handle = poller.start(cancel.clone());
    let reaction = tokio::time::timeout(Duration::from_secs(10), rx_reaction.recv())
        .await?
        .ok_or("matrix poller did not produce inbound reaction")?;
    cancel.cancel();
    handle.await??;
    Ok(reaction)
}

pub struct RecordingInbox {
    tx: Mutex<mpsc::Sender<InboundEvent>>,
    tx_reaction: Mutex<mpsc::Sender<InboundReaction>>,
}

impl RecordingInbox {
    fn new(tx: mpsc::Sender<InboundEvent>, tx_reaction: mpsc::Sender<InboundReaction>) -> Self {
        Self {
            tx: Mutex::new(tx),
            tx_reaction: Mutex::new(tx_reaction),
        }
    }
}

#[async_trait::async_trait]
impl ChannelInbox for RecordingInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), crabgent_channel::ChannelError> {
        self.tx
            .lock()
            .await
            .send(event)
            .await
            .map_err(crabgent_channel::ChannelError::adapter)
    }

    async fn receive_reaction(
        &self,
        reaction: InboundReaction,
    ) -> Result<(), crabgent_channel::ChannelError> {
        self.tx_reaction
            .lock()
            .await
            .send(reaction)
            .await
            .map_err(crabgent_channel::ChannelError::adapter)
    }
}

pub fn short_sync() -> SyncSettings {
    SyncSettings::new().timeout(Duration::from_millis(500))
}

fn unique_localpart(prefix: &str) -> String {
    let next = NEXT_USER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{}{}", std::process::id(), next)
}
