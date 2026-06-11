//! Matrix sync poller.

use std::{sync::Arc, time::Duration};

use crabgent_channel::{
    AudioValidator, Channel, ChannelError, ChannelInbox, ChannelKind, ImageStore, ImageValidator,
    InboundEvent, InboundReaction,
};
use crabgent_command::{
    CommandAgentName, CommandError, CommandPrefix, CommandRegistry, CommandWiring, SessionStore,
};
use crabgent_core::PolicyHook;
use crabgent_log::{error, instrument, warn};
use matrix_sdk::{
    LoopCtrl,
    config::SyncSettings,
    deserialized_responses::TimelineEvent,
    ruma::{OwnedRoomId, OwnedUserId},
};
use tokio::{task::JoinHandle, time::sleep};
use tokio_util::sync::CancellationToken;

use crate::{
    channel::MatrixChannel,
    inbound::{
        InboundMediaClients, timeline_event_to_inbound, timeline_event_to_inbound_reaction,
        timeline_redaction_to_inbound_reaction,
    },
    reaction_tracker::ReactionTracker,
    subject::build_subject_resolver,
};

const DEFAULT_SYNC_TIMEOUT_SECS: u64 = 5;
const DEFAULT_RETRY_DELAY_SECS: u64 = 2;
/// Total deadline for a single media download. Matrix media is byte-capped
/// (`MAX_IMAGE_BYTES` / `MAX_AUDIO_BYTES`), so a finite total timeout is safe
/// here and bounds slow-loris responses from a hostile homeserver.
const MEDIA_DOWNLOAD_TIMEOUT_SECS: u64 = 30;
pub const DEFAULT_COMMAND_PREFIX: &str = "!";

/// Build a hardened HTTP client for inbound media downloads.
///
/// Media fetch URLs are derived from homeserver API responses, so the client
/// refuses redirects (a federated or compromised homeserver could otherwise
/// 3xx-redirect the authenticated fetch toward internal hosts, a blind SSRF)
/// and enforces a finite total timeout against slow-loris responses.
fn media_client() -> reqwest::Client {
    match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(MEDIA_DOWNLOAD_TIMEOUT_SECS))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            // Builder failure means the TLS backend could not initialize. Keep
            // the channel running with a default client and surface the lost hardening.
            error!(error = %err, "failed to build hardened media client, using default");
            reqwest::Client::new()
        }
    }
}

/// Background Matrix `/sync` driver.
pub struct MatrixSyncPoller {
    channel: Arc<MatrixChannel>,
    inbox: Arc<dyn ChannelInbox>,
    commands: Option<CommandWiring>,
    sync_timeout: Duration,
    retry_delay: Duration,
    image_client: reqwest::Client,
    image_store: Option<Arc<dyn ImageStore>>,
    image_validator: Option<Arc<ImageValidator>>,
    audio_client: reqwest::Client,
    audio_validator: Option<Arc<AudioValidator>>,
    reaction_tracker: Arc<ReactionTracker>,
}

impl MatrixSyncPoller {
    /// Build a poller over a Matrix channel and channel inbox.
    pub fn new(channel: Arc<MatrixChannel>, inbox: Arc<dyn ChannelInbox>) -> Self {
        Self {
            channel,
            inbox,
            commands: None,
            sync_timeout: Duration::from_secs(DEFAULT_SYNC_TIMEOUT_SECS),
            retry_delay: Duration::from_secs(DEFAULT_RETRY_DELAY_SECS),
            image_client: media_client(),
            image_store: None,
            image_validator: None,
            audio_client: media_client(),
            audio_validator: None,
            reaction_tracker: Arc::new(ReactionTracker::default()),
        }
    }

    /// Enable inbound image downloads.
    #[must_use]
    pub fn with_image_support(
        mut self,
        image_client: reqwest::Client,
        image_store: Arc<dyn ImageStore>,
        image_validator: ImageValidator,
    ) -> Self {
        self.image_client = image_client;
        self.image_store = Some(image_store);
        self.image_validator = Some(Arc::new(image_validator));
        self
    }

    /// Enable inbound audio downloads.
    #[must_use]
    pub fn with_audio_support(
        mut self,
        audio_client: reqwest::Client,
        audio_validator: AudioValidator,
    ) -> Self {
        self.audio_client = audio_client;
        self.audio_validator = Some(Arc::new(audio_validator));
        self
    }

    /// Override the server long-poll timeout.
    #[must_use]
    pub const fn with_sync_timeout(mut self, timeout: Duration) -> Self {
        self.sync_timeout = timeout;
        self
    }

    /// Override retry delay after sync or inbox errors.
    #[must_use]
    pub const fn with_retry_delay(mut self, delay: Duration) -> Self {
        self.retry_delay = delay;
        self
    }

    #[must_use]
    pub fn with_commands(
        self,
        registry: CommandRegistry,
        agent_name: CommandAgentName,
        prefix: Option<CommandPrefix>,
        store: Arc<dyn SessionStore>,
        policy: Arc<dyn PolicyHook>,
    ) -> Self {
        self.try_with_commands(registry, agent_name, prefix, store, policy)
            .unwrap_or_else(|err| panic_invalid_command_config(&err))
    }

    pub fn try_with_commands(
        mut self,
        registry: CommandRegistry,
        agent_name: CommandAgentName,
        prefix: Option<CommandPrefix>,
        store: Arc<dyn SessionStore>,
        policy: Arc<dyn PolicyHook>,
    ) -> Result<Self, CommandError> {
        let resolver_agent = agent_name.as_str().to_owned();
        self.commands = Some(
            CommandWiring::try_new(
                self.inbox.as_ref(),
                registry,
                agent_name,
                // Matrix conventionally uses ! for bot commands.
                prefix.unwrap_or_else(default_command_prefix),
                store,
                policy,
            )?
            .with_subject_resolver(build_subject_resolver(
                Arc::clone(&self.channel),
                resolver_agent,
            )),
        );
        Ok(self)
    }

    #[must_use]
    pub fn command_prefix(&self) -> Option<&CommandPrefix> {
        self.commands.as_ref().map(CommandWiring::prefix)
    }

    /// Spawn the poller loop.
    pub fn start(self, cancel: CancellationToken) -> JoinHandle<Result<(), ChannelError>> {
        tokio::spawn(async move { self.run(cancel).await })
    }

    /// Run until cancellation.
    #[instrument(level = "debug", skip(self, cancel))]
    pub async fn run(self, cancel: CancellationToken) -> Result<(), ChannelError> {
        loop {
            if cancel.is_cancelled() {
                return Ok(());
            }
            tokio::select! {
                () = cancel.cancelled() => return Ok(()),
                result = self.tick_once() => {
                    if let Err(err) = result {
                        warn!("matrix poller tick failed: {err}");
                        tokio::select! {
                            () = cancel.cancelled() => return Ok(()),
                            () = sleep(self.retry_delay) => {}
                        }
                    }
                }
            }
        }
    }

    async fn tick_once(&self) -> Result<(), ChannelError> {
        let settings = SyncSettings::new().timeout(self.sync_timeout);
        let channel = Arc::clone(&self.channel);
        let inbox = self.dispatch_inbox();
        let image_client = self.image_client.clone();
        let image_store = self.image_store.clone();
        let image_validator = self.image_validator.clone();
        let audio_client = self.audio_client.clone();
        let audio_validator = self.audio_validator.clone();
        let reaction_tracker = Arc::clone(&self.reaction_tracker);
        self.channel
            .client()
            .sync_with_callback(settings, move |response| {
                let channel = Arc::clone(&channel);
                let inbox = Arc::clone(&inbox);
                let image_client = image_client.clone();
                let image_store = image_store.clone();
                let image_validator = image_validator.clone();
                let audio_client = audio_client.clone();
                let audio_validator = audio_validator.clone();
                let reaction_tracker = Arc::clone(&reaction_tracker);
                async move {
                    let dispatch = EventDispatch {
                        inbox: &inbox,
                        channel: &channel,
                        reaction_tracker: &reaction_tracker,
                        image_client: &image_client,
                        image_store: image_store.as_ref(),
                        image_validator: image_validator.as_deref(),
                        audio_client: &audio_client,
                        audio_validator: audio_validator.as_deref(),
                    };
                    for (room_id, update) in response.rooms.joined {
                        let kind = match channel.prefetch_kind(&room_id).await {
                            Ok(kind) => kind,
                            Err(err) => {
                                error!(room_id = %room_id, error = %err, "matrix kind prefetch failed");
                                continue;
                            }
                        };
                        for event in update.timeline.events {
                            dispatch.handle(&room_id, &event, kind).await;
                        }
                    }
                    LoopCtrl::Break
                }
            })
            .await
            .map_err(ChannelError::adapter)?;
        Ok(())
    }

    fn dispatch_inbox(&self) -> Arc<dyn ChannelInbox> {
        let Some(commands) = &self.commands else {
            return Arc::clone(&self.inbox);
        };
        let channel: Arc<dyn Channel> = self.channel.clone();
        commands.wrap_inbox(Arc::clone(&self.inbox), channel)
    }
}

fn default_command_prefix() -> CommandPrefix {
    CommandPrefix::parse(DEFAULT_COMMAND_PREFIX).expect("static Matrix command prefix is valid")
}

#[expect(
    clippy::panic,
    reason = "with_commands preserves the existing convenience-builder panic contract; try_with_commands is the fallible alternative"
)]
fn panic_invalid_command_config(err: &CommandError) -> ! {
    panic!(
        "invalid command dispatch configuration: {err}. Command dispatch must wrap mandatory channel gates explicitly"
    )
}

/// Per-tick dispatch context shared across timeline events.
struct EventDispatch<'a> {
    inbox: &'a Arc<dyn ChannelInbox>,
    channel: &'a Arc<MatrixChannel>,
    reaction_tracker: &'a ReactionTracker,
    image_client: &'a reqwest::Client,
    image_store: Option<&'a Arc<dyn ImageStore>>,
    image_validator: Option<&'a ImageValidator>,
    audio_client: &'a reqwest::Client,
    audio_validator: Option<&'a AudioValidator>,
}

impl EventDispatch<'_> {
    /// Try reaction -> redaction -> message and forward the first
    /// matching dispatch path. Each branch logs at `error!` when the
    /// inbox call fails; the sync loop continues.
    async fn handle(&self, room_id: &OwnedRoomId, event: &TimelineEvent, kind: ChannelKind) {
        let bot_user_id = self.channel.bot_user_id();
        if self.handle_reaction(room_id, event, bot_user_id).await {
            return;
        }
        if self.handle_redaction(room_id, event, bot_user_id).await {
            return;
        }
        self.handle_message(room_id, event, bot_user_id, kind).await;
    }

    async fn handle_reaction(
        &self,
        room_id: &OwnedRoomId,
        event: &TimelineEvent,
        bot_user_id: &OwnedUserId,
    ) -> bool {
        let Some(reaction) =
            timeline_event_to_inbound_reaction(room_id, event, bot_user_id, self.reaction_tracker)
        else {
            return false;
        };
        self.receive_reaction(room_id, reaction, "reaction").await;
        true
    }

    async fn handle_redaction(
        &self,
        room_id: &OwnedRoomId,
        event: &TimelineEvent,
        bot_user_id: &OwnedUserId,
    ) -> bool {
        let Some(reaction) =
            timeline_redaction_to_inbound_reaction(event, bot_user_id, self.reaction_tracker)
        else {
            return false;
        };
        self.receive_reaction(room_id, reaction, "redaction").await;
        true
    }

    async fn receive_reaction(
        &self,
        room_id: &OwnedRoomId,
        reaction: InboundReaction,
        action: &'static str,
    ) {
        if let Err(err) = self.inbox.receive_reaction(reaction).await {
            log_inbox_error(room_id, &err, action);
        }
    }

    async fn handle_message(
        &self,
        room_id: &OwnedRoomId,
        event: &TimelineEvent,
        bot_user_id: &OwnedUserId,
        kind: ChannelKind,
    ) {
        let access_token = self.channel.client().access_token();
        let media = self.media_clients(access_token.as_deref());
        if let Some(inbound) =
            timeline_event_to_inbound(room_id, event, bot_user_id, Some(kind), &media).await
        {
            self.receive_message(room_id, inbound).await;
        }
    }

    fn media_clients<'a>(&'a self, access_token: Option<&'a str>) -> InboundMediaClients<'a> {
        InboundMediaClients {
            matrix_client: self.channel.client(),
            image_http_client: self.image_client,
            image_store: self.image_store,
            image_validator: self.image_validator,
            audio_http_client: self.audio_client,
            audio_validator: self.audio_validator,
            access_token,
        }
    }

    async fn receive_message(&self, room_id: &OwnedRoomId, inbound: InboundEvent) {
        if let Err(err) = self.inbox.receive(inbound).await {
            log_inbox_error(room_id, &err, "message");
        }
    }
}

fn log_inbox_error(room_id: &OwnedRoomId, err: &ChannelError, action: &'static str) {
    error!(
        room_id = %room_id,
        error = %err,
        action,
        "matrix inbox dispatch failed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crabgent_channel::InboundEvent;
    use matrix_sdk::{Client, ruma::owned_user_id};
    use url::Url;

    struct NoopInbox;

    #[async_trait]
    impl ChannelInbox for NoopInbox {
        async fn receive(&self, _event: InboundEvent) -> Result<(), ChannelError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn builder_overrides_timeouts() {
        let client = Client::new(Url::parse("https://example.org").expect("test result"))
            .await
            .expect("test result");
        let channel = Arc::new(MatrixChannel::from_client(
            client,
            owned_user_id!("@bot:example.org"),
            None,
        ));
        let poller = MatrixSyncPoller::new(channel, Arc::new(NoopInbox))
            .with_sync_timeout(Duration::from_secs(1))
            .with_retry_delay(Duration::from_secs(3));
        assert_eq!(poller.sync_timeout, Duration::from_secs(1));
        assert_eq!(poller.retry_delay, Duration::from_secs(3));
    }

    #[tokio::test]
    async fn media_client_does_not_follow_redirects() {
        use httpmock::{Method::GET, MockServer};

        // A 3xx from the homeserver must not be followed: an attacker-controlled
        // redirect could retarget the authenticated media fetch at an internal
        // host (blind SSRF) and forward the bearer token. The hardened client
        // returns the 3xx response verbatim instead.
        let server = MockServer::start();
        let redirect = server.mock(|when, then| {
            when.method(GET)
                .path("/_matrix/client/v1/media/download/localhost/photo-id");
            then.status(302)
                .header("location", "http://169.254.169.254/latest/meta-data/");
        });
        let target = server.mock(|when, then| {
            when.method(GET).path("/latest/meta-data/");
            then.status(200).body("internal-secret");
        });

        let response = media_client()
            .get(server.url("/_matrix/client/v1/media/download/localhost/photo-id"))
            .send()
            .await
            .expect("request reaches the mock server");

        assert_eq!(response.status().as_u16(), 302);
        redirect.assert();
        target.assert_calls(0);
        let body = response.text().await.expect("redirect body readable");
        assert!(!body.contains("internal-secret"));
    }

    #[tokio::test]
    async fn run_returns_when_cancelled_before_first_tick() {
        let client = Client::new(Url::parse("https://example.org").expect("test result"))
            .await
            .expect("test result");
        let channel = Arc::new(MatrixChannel::from_client(
            client,
            owned_user_id!("@bot:example.org"),
            None,
        ));
        let poller = MatrixSyncPoller::new(channel, Arc::new(NoopInbox));
        let cancel = CancellationToken::new();
        cancel.cancel();
        poller.run(cancel).await.expect("test result");
    }

    #[tokio::test]
    async fn start_returns_when_cancelled_before_first_tick() {
        let client = Client::new(Url::parse("https://example.org").expect("test result"))
            .await
            .expect("test result");
        let channel = Arc::new(MatrixChannel::from_client(
            client,
            owned_user_id!("@bot:example.org"),
            None,
        ));
        let poller = MatrixSyncPoller::new(channel, Arc::new(NoopInbox));
        let cancel = CancellationToken::new();
        cancel.cancel();
        poller
            .start(cancel)
            .await
            .expect("test result")
            .expect("test result");
    }
}

#[cfg(test)]
mod command_tests;
