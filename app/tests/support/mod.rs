#![allow(dead_code)]

use std::{
    error::Error,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use matrix_sdk::{
    Client,
    config::SyncSettings,
    ruma::{
        RoomId, UserId,
        api::client::{
            account::register,
            room::{
                Visibility,
                create_room::{self, v3::RoomPreset},
            },
            uiaa::{AuthData, Dummy},
        },
        room::JoinRule,
    },
};
use testcontainers::{
    ContainerAsync, GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use url::Url;

pub const CONDUIT_IMAGE: &str = "matrixconduit/matrix-conduit";
pub const CONDUIT_TAG: &str = "v0.9.0";
pub const CONDUIT_PORT: u16 = 6167;

const PASSWORD: &str = "correct-horse-battery-staple";
static NEXT_USER: AtomicU64 = AtomicU64::new(1);

pub type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

pub struct MatrixTestCtx {
    pub homeserver: Url,
    real_bot: Option<(String, String)>,
    _container: Option<ContainerAsync<GenericImage>>,
}

impl MatrixTestCtx {
    pub async fn new() -> TestResult<Option<Self>> {
        if let (Ok(homeserver), Ok(user), Ok(password)) = (
            std::env::var("MATRIX_HOMESERVER"),
            std::env::var("MATRIX_USER"),
            std::env::var("MATRIX_PASSWORD"),
        ) {
            return Ok(Some(Self {
                homeserver: Url::parse(&homeserver)?,
                real_bot: Some((user, password)),
                _container: None,
            }));
        }

        let image = GenericImage::new(CONDUIT_IMAGE, CONDUIT_TAG)
            .with_exposed_port(CONDUIT_PORT.tcp())
            .with_wait_for(WaitFor::seconds(5))
            .with_env_var("CONDUIT_SERVER_NAME", "localhost")
            .with_env_var("CONDUIT_ALLOW_REGISTRATION", "true")
            .with_env_var("CONDUIT_DATABASE_BACKEND", "rocksdb")
            .with_env_var("CONDUIT_ALLOW_FEDERATION", "false");
        let container = match image.start().await {
            Ok(container) => container,
            Err(err) => {
                crabgent_log::warn!(
                    "matrix conduit container unavailable; skipping integration test: {err}"
                );
                return Ok(None);
            }
        };
        let port = match container.get_host_port_ipv4(CONDUIT_PORT.tcp()).await {
            Ok(port) => port,
            Err(err) => {
                crabgent_log::warn!(
                    "matrix conduit port unavailable; skipping integration test: {err}"
                );
                return Ok(None);
            }
        };
        Ok(Some(Self {
            homeserver: Url::parse(&format!("http://127.0.0.1:{port}"))?,
            real_bot: None,
            _container: Some(container),
        }))
    }

    pub async fn register_client(&self, prefix: &str) -> TestResult<Client> {
        let localpart = unique_localpart(prefix);
        let client = Client::new(self.homeserver.clone()).await?;
        if prefix == "bot"
            && let Some((user, password)) = self.real_bot.as_ref()
        {
            client
                .matrix_auth()
                .login_username(user, password)
                .send()
                .await?;
            return Ok(client);
        }
        let mut request = register::v3::Request::new();
        request.username = Some(localpart.clone());
        request.password = Some(PASSWORD.to_owned());
        request.auth = Some(AuthData::Dummy(Dummy::new()));
        client.matrix_auth().register(request).await?;
        if client.user_id().is_none() {
            client
                .matrix_auth()
                .login_username(&localpart, PASSWORD)
                .send()
                .await?;
        }
        Ok(client)
    }
}

pub async fn create_public_room(
    bot: &Client,
    name: &str,
) -> TestResult<matrix_sdk::ruma::OwnedRoomId> {
    let mut request = create_room::v3::Request::new();
    request.preset = Some(RoomPreset::PublicChat);
    request.visibility = Visibility::Public;
    request.name = Some(name.to_owned());
    let room = bot.create_room(request).await?;
    let room_id = room.room_id().to_owned();
    if let Some(joined) = bot.get_room(&room_id) {
        joined
            .privacy_settings()
            .update_join_rule(JoinRule::Public)
            .await?;
    }
    Ok(room_id)
}

pub async fn create_private_room(
    bot: &Client,
    name: &str,
) -> TestResult<matrix_sdk::ruma::OwnedRoomId> {
    let mut request = create_room::v3::Request::new();
    request.preset = Some(RoomPreset::PrivateChat);
    request.visibility = Visibility::Private;
    request.name = Some(name.to_owned());
    Ok(bot.create_room(request).await?.room_id().to_owned())
}

pub async fn create_dm_room(
    bot: &Client,
    user_id: &UserId,
) -> TestResult<matrix_sdk::ruma::OwnedRoomId> {
    Ok(bot.create_dm(user_id).await?.room_id().to_owned())
}

pub async fn invite_and_join(bot: &Client, user: &Client, room_id: &RoomId) -> TestResult {
    let room = bot
        .get_room(room_id)
        .ok_or_else(|| format!("bot is not in room {room_id}"))?;
    room.invite_user_by_id(user.user_id().ok_or("user is not logged in")?)
        .await?;
    user.join_room_by_id(room_id).await?;
    Ok(())
}

pub async fn warmup_visibility(bot: &Client, room_id: &RoomId) -> TestResult {
    for _ in 0..5 {
        bot.sync_once(short_sync()).await?;
        if bot
            .get_room(room_id)
            .and_then(|room| room.join_rule())
            .is_some()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(format!("timed out waiting for visibility in {room_id}").into())
}

pub fn short_sync() -> SyncSettings {
    SyncSettings::new().timeout(Duration::from_millis(500))
}

pub fn unique_localpart(prefix: &str) -> String {
    let next = NEXT_USER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{}{}", std::process::id(), next)
}
