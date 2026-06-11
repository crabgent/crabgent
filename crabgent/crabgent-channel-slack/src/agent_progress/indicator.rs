//! `SlackAgentProgressIndicator` implementation.
//!
//! Drives the Slack agent-progress surface (`assistant.threads.setStatus`
//! V2 plus `chat.startStream` / `chat.appendStream` / `chat.stopStream`
//! V3) on top of `SlackHttpClient`. The indicator auto-detects whether
//! the bot is configured as a Slack AI Agent on the first attempt and
//! caches the verdict in an `AtomicU8`. Sentinel error codes
//! (`feature_not_supported`, `not_allowed_token_type`, `access_denied`)
//! demote the workspace to `Standard` and stop further agent-surface
//! attempts for the indicator's lifetime; other transient errors leave
//! the verdict as `Unknown` so the next run retries.
//!
//! Per-run state lives in a `HashMap<RunId, RunState>` guarded by a
//! `std::sync::Mutex` (not a Tokio mutex, so `Drop` can lock synchronously).
//! Locks are released before any `.await`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel::subject::ChannelSubjectExt;
use crabgent_core::{RunCtx, RunId};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval, timeout};

use crate::CHANNEL_NAME;
use crate::api::SlackHttpClient;
use crate::block_kit::{PlanUpdateChunk, StreamChunk};
use crate::error::SlackError;
use crate::subject::{SLACK_CHANNEL_ID, SLACK_THREAD_ROOT};

use super::consumer::{RunConsumer, RunConsumerOptions, SentinelHandler};
use super::types::{
    AgentProgressConfig, AgentProgressResult, ProgressChunk, SENTINEL_NOT_AGENT_ERRORS,
    SlackAgentProgress, SlackAppType,
};

struct RunState {
    heartbeat: JoinHandle<()>,
    chunk_tx: mpsc::UnboundedSender<StreamChunk>,
    consumer: JoinHandle<()>,
    last_status: Arc<Mutex<String>>,
}

/// Live `SlackAgentProgress` implementation backed by `SlackHttpClient`.
pub struct SlackAgentProgressIndicator {
    client: Arc<SlackHttpClient>,
    app_type: Arc<AtomicU8>,
    runs: Arc<Mutex<HashMap<RunId, RunState>>>,
    warn_logged: Arc<AtomicBool>,
    config: AgentProgressConfig,
}

impl std::fmt::Debug for SlackAgentProgressIndicator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackAgentProgressIndicator")
            .field("app_type", &self.app_type())
            .finish_non_exhaustive()
    }
}

impl SlackAgentProgressIndicator {
    /// Build an indicator backed by the given Slack HTTP client with the
    /// default config (both surfaces on, production timings). The
    /// indicator starts in `SlackAppType::Unknown` and transitions on
    /// the first agent-surface call.
    #[must_use]
    pub fn new(client: Arc<SlackHttpClient>) -> Self {
        Self::with_config(client, AgentProgressConfig::default())
    }

    /// Build an indicator with an explicit config. Use this to disable
    /// the V2 bubble or V3 card surface independently, or to override
    /// the heartbeat and idle-flush timings for local iteration.
    #[must_use]
    pub fn with_config(client: Arc<SlackHttpClient>, config: AgentProgressConfig) -> Self {
        Self {
            client,
            app_type: Arc::new(AtomicU8::new(SlackAppType::Unknown.to_u8())),
            runs: Arc::new(Mutex::new(HashMap::new())),
            warn_logged: Arc::new(AtomicBool::new(false)),
            config,
        }
    }

    /// Return the current classification verdict.
    #[must_use]
    pub fn app_type(&self) -> SlackAppType {
        SlackAppType::try_from(self.app_type.load(Ordering::SeqCst))
            .unwrap_or(SlackAppType::Unknown)
    }

    /// Number of runs currently tracked. Useful for tests asserting
    /// lifecycle correctness.
    #[must_use]
    pub fn active_runs_count(&self) -> usize {
        self.lock_runs().len()
    }

    fn slack_target(ctx: &RunCtx) -> Option<(String, String)> {
        let channel_id = ctx.subject.attr(SLACK_CHANNEL_ID)?.to_owned();
        let thread_ts = ctx.subject.attr(SLACK_THREAD_ROOT)?.to_owned();
        Some((channel_id, thread_ts))
    }

    fn channel_mismatch(ctx: &RunCtx) -> bool {
        ctx.subject
            .channel()
            .is_none_or(|attr| attr.channel != CHANNEL_NAME)
    }

    fn lock_runs(&self) -> MutexGuard<'_, HashMap<RunId, RunState>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn cache_standard_with_warn(&self, ctx: &RunCtx, code: &str) {
        self.app_type
            .store(SlackAppType::Standard.to_u8(), Ordering::SeqCst);
        self.drain_active_runs();
        if !self.warn_logged.swap(true, Ordering::SeqCst) {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                slack_code = %code,
                "slack workspace classified as non-agent; further agent-progress surfaces disabled"
            );
        }
    }

    fn drain_active_runs(&self) {
        let mut guard = self.lock_runs();
        for (_, state) in guard.drain() {
            state.heartbeat.abort();
            state.consumer.abort();
        }
    }

    fn stream_sentinel_handler(&self, ctx: &RunCtx) -> SentinelHandler {
        let app_type = Arc::clone(&self.app_type);
        let runs = Arc::clone(&self.runs);
        let warn_logged = Arc::clone(&self.warn_logged);
        let run_id = ctx.run_id.clone();
        Arc::new(move |code| {
            app_type.store(SlackAppType::Standard.to_u8(), Ordering::SeqCst);
            let state = runs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&run_id);
            if let Some(state) = state {
                state.heartbeat.abort();
                drop(state.chunk_tx);
            }
            if !warn_logged.swap(true, Ordering::SeqCst) {
                crabgent_log::warn!(
                    run_id = %run_id,
                    slack_code = %code,
                    "slack workspace classified as non-agent; further agent-progress surfaces disabled"
                );
            }
        })
    }

    fn spawn_heartbeat(
        client: Arc<SlackHttpClient>,
        run_id: RunId,
        channel_id: String,
        thread_ts: String,
        last_status: Arc<Mutex<String>>,
        heartbeat_interval: Duration,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = interval(heartbeat_interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
            // First tick fires immediately; the status was posted by the
            // caller before spawn, so drop the immediate tick and wait
            // one full interval before re-posting.
            tick.tick().await;
            loop {
                tick.tick().await;
                let snapshot = last_status
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                if let Err(err) = client
                    .assistant_threads_set_status(&channel_id, &thread_ts, &snapshot)
                    .await
                {
                    crabgent_log::warn!(
                        run_id = %run_id,
                        error = %err,
                        "slack agent-status heartbeat post failed"
                    );
                }
            }
        })
    }

    fn insert_active_run(&self, ctx: &RunCtx, channel_id: &str, thread_ts: &str, status: &str) {
        let last_status = Arc::new(Mutex::new(status.to_owned()));
        let heartbeat = if self.config.enable_bubble {
            Self::spawn_heartbeat(
                Arc::clone(&self.client),
                ctx.run_id.clone(),
                channel_id.to_owned(),
                thread_ts.to_owned(),
                Arc::clone(&last_status),
                self.config.heartbeat_interval,
            )
        } else {
            tokio::spawn(async {})
        };
        let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel();
        let consumer = if self.config.enable_card {
            RunConsumer::spawn(
                Arc::clone(&self.client),
                ctx.run_id.clone(),
                channel_id.to_owned(),
                thread_ts.to_owned(),
                chunk_rx,
                RunConsumerOptions {
                    initial_chunks: vec![StreamChunk::PlanUpdate(PlanUpdateChunk {
                        title: "thinking...".into(),
                    })],
                    task_display_mode: Some("plan"),
                    idle_flush_interval: self.config.idle_flush_interval,
                    sentinel_handler: Some(self.stream_sentinel_handler(ctx)),
                },
            )
        } else {
            tokio::spawn(async move { while chunk_rx.recv().await.is_some() {} })
        };
        let state = RunState {
            heartbeat,
            chunk_tx,
            consumer,
            last_status,
        };
        let previous = self.lock_runs().insert(ctx.run_id.clone(), state);
        if let Some(prev) = previous {
            prev.heartbeat.abort();
            prev.consumer.abort();
        }
    }

    async fn apply_active_status(
        &self,
        ctx: &RunCtx,
        channel_id: &str,
        thread_ts: &str,
        status: &str,
    ) -> AgentProgressResult<()> {
        if self.config.enable_bubble
            && let Err(err) = self
                .client
                .assistant_threads_set_status(channel_id, thread_ts, status)
                .await
        {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                error = %err,
                "slack agent-status apply failed; skipping heartbeat spawn"
            );
            return Ok(());
        }
        self.insert_active_run(ctx, channel_id, thread_ts, status);
        Ok(())
    }

    async fn try_detect_app_type(
        &self,
        ctx: &RunCtx,
        channel_id: &str,
        thread_ts: &str,
        status: &str,
    ) -> AgentProgressResult<()> {
        if !self.config.enable_bubble {
            // No V2 surface available to probe; the V3 consumer's
            // sentinel handler is the only path that can demote to
            // Standard. Optimistically promote and let chat.startStream
            // surface a sentinel error if the workspace rejects it.
            _ = self.app_type.compare_exchange(
                SlackAppType::Unknown.to_u8(),
                SlackAppType::AiAgent.to_u8(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            self.insert_active_run(ctx, channel_id, thread_ts, status);
            return Ok(());
        }
        match self
            .client
            .assistant_threads_set_status(channel_id, thread_ts, status)
            .await
        {
            Ok(()) => {
                _ = self.app_type.compare_exchange(
                    SlackAppType::Unknown.to_u8(),
                    SlackAppType::AiAgent.to_u8(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                self.insert_active_run(ctx, channel_id, thread_ts, status);
            }
            Err(err) => self.handle_detect_error(ctx, &err),
        }
        Ok(())
    }

    fn handle_detect_error(&self, ctx: &RunCtx, err: &SlackError) {
        if let SlackError::ApiError { slack_code, .. } = err
            && SENTINEL_NOT_AGENT_ERRORS.contains(&slack_code.as_str())
        {
            self.cache_standard_with_warn(ctx, slack_code);
        } else {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                error = %err,
                "slack agent-status auto-detect failed; classification stays Unknown"
            );
        }
    }

    async fn update_status(
        &self,
        ctx: &RunCtx,
        channel_id: &str,
        thread_ts: &str,
        status: String,
    ) -> AgentProgressResult<()> {
        if !self.config.enable_bubble {
            return Ok(());
        }
        if let Err(err) = self
            .client
            .assistant_threads_set_status(channel_id, thread_ts, &status)
            .await
        {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                error = %err,
                "slack agent-status update failed; last_status unchanged"
            );
            return Ok(());
        }
        let guard = self.lock_runs();
        if let Some(state) = guard.get(&ctx.run_id) {
            *state
                .last_status
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = status;
        }
        Ok(())
    }

    fn try_send_chunk(&self, ctx: &RunCtx, chunk: StreamChunk) -> AgentProgressResult<()> {
        if let Some(state) = self.lock_runs().get(&ctx.run_id)
            && state.chunk_tx.send(chunk).is_err()
        {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                "slack agent-progress consumer channel closed"
            );
        }
        Ok(())
    }
}

#[async_trait]
impl SlackAgentProgress for SlackAgentProgressIndicator {
    async fn start(&self, ctx: &RunCtx, initial_status: &str) -> AgentProgressResult<()> {
        if Self::channel_mismatch(ctx) {
            return Ok(());
        }
        let Some((channel_id, thread_ts)) = Self::slack_target(ctx) else {
            return Ok(());
        };
        match self.app_type() {
            SlackAppType::Standard => Ok(()),
            SlackAppType::AiAgent => {
                self.apply_active_status(ctx, &channel_id, &thread_ts, initial_status)
                    .await
            }
            SlackAppType::Unknown => {
                self.try_detect_app_type(ctx, &channel_id, &thread_ts, initial_status)
                    .await
            }
        }
    }

    async fn chunk(&self, ctx: &RunCtx, chunk: ProgressChunk) -> AgentProgressResult<()> {
        if Self::channel_mismatch(ctx) {
            return Ok(());
        }
        if !matches!(self.app_type(), SlackAppType::AiAgent) {
            return Ok(());
        }
        match chunk {
            ProgressChunk::Status(status) => {
                let Some((channel_id, thread_ts)) = Self::slack_target(ctx) else {
                    return Ok(());
                };
                self.update_status(ctx, &channel_id, &thread_ts, status)
                    .await
            }
            ProgressChunk::MarkdownText(c) => {
                self.try_send_chunk(ctx, StreamChunk::MarkdownText(c))
            }
            ProgressChunk::TaskUpdate(c) => self.try_send_chunk(ctx, StreamChunk::TaskUpdate(c)),
            ProgressChunk::PlanUpdate(c) => self.try_send_chunk(ctx, StreamChunk::PlanUpdate(c)),
            ProgressChunk::Blocks(c) => self.try_send_chunk(ctx, StreamChunk::Blocks(c)),
        }
    }

    async fn stop(&self, ctx: &RunCtx) -> AgentProgressResult<()> {
        let state = self.lock_runs().remove(&ctx.run_id);
        let Some(state) = state else {
            return Ok(());
        };
        state.heartbeat.abort();
        drop(state.chunk_tx);
        match timeout(Duration::from_millis(500), state.consumer).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                crabgent_log::warn!(
                    run_id = %ctx.run_id,
                    error = %err,
                    "slack agent-progress consumer join failed"
                );
            }
            Err(_elapsed) => {
                crabgent_log::warn!(
                    run_id = %ctx.run_id,
                    "slack agent-progress consumer stop timed out"
                );
            }
        }
        // The setStatus banner is keyed by the subject's slack target, so it
        // can only be cleared when those attrs are still present. Skip the
        // clear entirely when the V2 surface is disabled.
        if self.config.enable_bubble
            && let Some((channel_id, thread_ts)) = Self::slack_target(ctx)
            && let Err(err) = self
                .client
                .assistant_threads_set_status(&channel_id, &thread_ts, "")
                .await
        {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                error = %err,
                "slack agent-status clear-on-stop failed"
            );
        }
        Ok(())
    }
}

impl Drop for SlackAgentProgressIndicator {
    fn drop(&mut self) {
        let mut guard = self.lock_runs();
        for (_, state) in guard.drain() {
            state.heartbeat.abort();
            state.consumer.abort();
        }
    }
}
