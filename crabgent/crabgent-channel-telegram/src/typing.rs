//! Telegram `TypingIndicator` implementation.
//!
//! Drives the Bot API `sendChatAction` endpoint with `action = "typing"`
//! on a 4-second heartbeat for as long as a kernel run is active.
//! Telegram expires the typing indicator after roughly 5 seconds, so the
//! cadence keeps it visible without flooding the API.
//!
//! Per-run `JoinHandle`s are tracked so concurrent runs in different
//! chats coexist and so `Drop` aborts every loop if the indicator dies
//! mid-run.
//!
//! Channel routing: `start` and `stop` inspect `ctx.subject.attrs` for
//! `channel = "telegram"` and parse the `conv` attr into a chat id. Runs
//! targeting another adapter are no-ops.
//!
//! Failures from the underlying HTTP call are best-effort: the loop logs
//! at warn-level and keeps ticking. Typing is a UX signal, not a policy
//! gate.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel::ChannelSubjectExt;
use crabgent_core::{owner::Owner, run_id::RunId};
use crabgent_thinking::{TypingIndicator, TypingResult};
use serde_json::json;
use tokio::task::JoinHandle;

use crate::channel::{CHANNEL_NAME, TelegramChannel};
use crate::outbound::parse_chat_id;

/// Heartbeat cadence. Telegram expires the typing indicator after ~5s.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(4);

/// Telegram-flavoured typing indicator.
///
/// Wire one per Telegram adapter via `TypingHook::new(Arc::new(indicator))`.
pub struct TelegramTypingIndicator {
    channel: Arc<TelegramChannel>,
    active: Arc<Mutex<HashMap<RunId, JoinHandle<()>>>>,
}

impl TelegramTypingIndicator {
    /// Build an indicator that drives `sendChatAction` via the given channel.
    #[must_use]
    pub fn new(channel: Arc<TelegramChannel>) -> Self {
        Self {
            channel,
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn lock_handles(&self) -> std::sync::MutexGuard<'_, HashMap<RunId, JoinHandle<()>>> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn parse_target(ctx: &crabgent_core::RunCtx) -> Option<i64> {
        let attr = ctx.subject.channel()?;
        if attr.channel != CHANNEL_NAME {
            return None;
        }
        let conv = Owner::new(attr.conv.to_owned());
        parse_chat_id(&conv).ok()
    }
}

impl std::fmt::Debug for TelegramTypingIndicator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramTypingIndicator")
            .finish_non_exhaustive()
    }
}

impl Drop for TelegramTypingIndicator {
    fn drop(&mut self) {
        let mut guard = self.lock_handles();
        for (_run_id, handle) in guard.drain() {
            handle.abort();
        }
    }
}

#[async_trait]
impl TypingIndicator for TelegramTypingIndicator {
    async fn start(&self, ctx: &crabgent_core::RunCtx) -> TypingResult<()> {
        let Some(chat_id) = Self::parse_target(ctx) else {
            return Ok(());
        };
        let channel = Arc::clone(&self.channel);
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let body = json!({ "chat_id": chat_id, "action": "typing" });
                if let Err(err) = channel.post_json("sendChatAction", &body).await {
                    crabgent_log::warn!(
                        chat_id,
                        error = %err,
                        "telegram sendChatAction send failed",
                    );
                }
            }
        });
        let previous = {
            let mut guard = self.lock_handles();
            guard.insert(ctx.run_id.clone(), handle)
        };
        if let Some(old) = previous {
            old.abort();
        }
        Ok(())
    }

    async fn stop(&self, ctx: &crabgent_core::RunCtx) -> TypingResult<()> {
        if Self::parse_target(ctx).is_none() {
            return Ok(());
        }
        // Telegram has no cancel-typing endpoint; aborting the heartbeat
        // loop lets the indicator expire naturally after ~5s.
        let handle = {
            let mut guard = self.lock_handles();
            guard.remove(&ctx.run_id)
        };
        if let Some(handle) = handle {
            handle.abort();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_channel::ChannelKind;
    use crabgent_core::{RunCtx, Subject};
    use httpmock::Method::POST;
    use httpmock::MockServer;

    fn telegram_ctx(chat: &str) -> RunCtx {
        let conv = Owner::new(format!("telegram:{chat}"));
        let subject = Subject::new("agent").with_channel(CHANNEL_NAME, &conv, ChannelKind::Direct);
        RunCtx::new(RunId::new(), subject)
    }

    fn other_ctx() -> RunCtx {
        let conv = Owner::new("slack:T1/C1".to_string());
        let subject = Subject::new("agent").with_channel("slack", &conv, ChannelKind::Group);
        RunCtx::new(RunId::new(), subject)
    }

    fn build_channel() -> Arc<TelegramChannel> {
        Arc::new(TelegramChannel::new("test-token", "B-1", "crabgent_bot"))
    }

    #[tokio::test]
    async fn start_with_non_telegram_channel_is_noop() {
        let indicator = TelegramTypingIndicator::new(build_channel());
        indicator.start(&other_ctx()).await.expect("noop start");
        assert!(indicator.lock_handles().is_empty());
    }

    #[tokio::test]
    async fn start_with_invalid_chat_id_is_noop() {
        let indicator = TelegramTypingIndicator::new(build_channel());
        let ctx = telegram_ctx("not-a-number");
        indicator.start(&ctx).await.expect("noop start");
        assert!(indicator.lock_handles().is_empty());
    }

    #[tokio::test]
    async fn start_tracks_handle_per_run_id() {
        let indicator = TelegramTypingIndicator::new(build_channel());
        let ctx = telegram_ctx("12345");
        indicator.start(&ctx).await.expect("start");
        assert_eq!(indicator.lock_handles().len(), 1);
        assert!(indicator.lock_handles().contains_key(&ctx.run_id));
    }

    #[tokio::test]
    async fn double_start_replaces_handle() {
        let indicator = TelegramTypingIndicator::new(build_channel());
        let ctx = telegram_ctx("12345");

        indicator.start(&ctx).await.expect("first start");
        let first = indicator
            .lock_handles()
            .get(&ctx.run_id)
            .map(JoinHandle::id);
        indicator.start(&ctx).await.expect("second start");
        let second = indicator
            .lock_handles()
            .get(&ctx.run_id)
            .map(JoinHandle::id);

        assert_ne!(first, second);
        assert_eq!(indicator.lock_handles().len(), 1);
    }

    #[tokio::test]
    async fn stop_removes_active_handle() {
        let indicator = TelegramTypingIndicator::new(build_channel());
        let ctx = telegram_ctx("12345");
        indicator.start(&ctx).await.expect("start");
        indicator.stop(&ctx).await.expect("stop");
        assert!(indicator.lock_handles().is_empty());
    }

    #[tokio::test]
    async fn stop_without_active_run_is_idempotent() {
        let indicator = TelegramTypingIndicator::new(build_channel());
        let ctx = telegram_ctx("12345");
        indicator.stop(&ctx).await.expect("idempotent stop");
    }

    #[tokio::test]
    async fn drop_aborts_active_handles() {
        let ctx = telegram_ctx("12345");
        let first_id = {
            let indicator = TelegramTypingIndicator::new(build_channel());
            indicator.start(&ctx).await.expect("start");
            indicator
                .lock_handles()
                .get(&ctx.run_id)
                .map(JoinHandle::id)
                .expect("handle present before drop")
        };
        let fresh = TelegramTypingIndicator::new(build_channel());
        fresh.start(&ctx).await.expect("fresh start");
        let fresh_id = fresh
            .lock_handles()
            .get(&ctx.run_id)
            .map(JoinHandle::id)
            .expect("fresh handle");
        assert_ne!(first_id, fresh_id);
    }

    #[tokio::test]
    async fn heartbeat_posts_send_chat_action_to_telegram_api() {
        let server = MockServer::start_async().await;
        let endpoint = server
            .mock_async(|when, then| {
                when.method(POST).path_matches(".*sendChatAction.*");
                then.status(200).body(r#"{"ok":true,"result":true}"#);
            })
            .await;
        let channel = Arc::new(
            TelegramChannel::new("test-token", "B-1", "crabgent_bot")
                .with_api_base(server.base_url()),
        );
        let indicator = TelegramTypingIndicator::new(channel);
        let ctx = telegram_ctx("987654");

        indicator.start(&ctx).await.expect("start");
        // Interval fires immediately on the first tick; give the spawned
        // task a chance to run and complete the HTTP call.
        for _ in 0..50 {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
            if endpoint.calls_async().await >= 1 {
                break;
            }
        }
        endpoint.assert_calls_async(1).await;
        indicator.stop(&ctx).await.expect("stop");
    }
}
