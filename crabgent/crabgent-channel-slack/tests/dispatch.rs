use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_channel_slack::SlackError;
use crabgent_channel_slack::dispatch::{ListenerRegistry, SlackEventListener};
use crabgent_channel_slack::events::{SlackEvent, SlackMessageEvent};
use tokio::sync::Notify;

#[tokio::test]
async fn dispatch_is_concurrent_and_isolates_listener_failures() {
    let registry = ListenerRegistry::new();
    let received = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicUsize::new(0));
    let failure_recorded = Arc::new(Notify::new());
    let receive_recorded = Arc::new(Notify::new());
    registry.register(Arc::new(FailingListener {
        failed: Arc::clone(&failed),
        done: Arc::clone(&failure_recorded),
    }));
    registry.register(Arc::new(NotifyListener {
        received: Arc::clone(&received),
        done: Arc::clone(&receive_recorded),
    }));

    registry.dispatch(sample_event()).await;
    tokio::time::timeout(Duration::from_secs(1), failure_recorded.notified())
        .await
        .expect("failing listener recorded completion");
    tokio::time::timeout(Duration::from_secs(1), receive_recorded.notified())
        .await
        .expect("notify listener recorded completion");

    assert_eq!(failed.load(Ordering::SeqCst), 1);
    assert!(received.load(Ordering::SeqCst));
}

struct FailingListener {
    failed: Arc<AtomicUsize>,
    done: Arc<Notify>,
}

#[async_trait]
impl SlackEventListener for FailingListener {
    async fn on_event(&self, _event: SlackEvent) -> Result<(), SlackError> {
        self.failed.fetch_add(1, Ordering::SeqCst);
        self.done.notify_one();
        Err(SlackError::Internal("listener failure".to_owned()))
    }
}

struct NotifyListener {
    received: Arc<AtomicBool>,
    done: Arc<Notify>,
}

#[async_trait]
impl SlackEventListener for NotifyListener {
    async fn on_event(&self, _event: SlackEvent) -> Result<(), SlackError> {
        self.received.store(true, Ordering::SeqCst);
        self.done.notify_one();
        Ok(())
    }
}

fn sample_event() -> SlackEvent {
    SlackEvent::AppMention(SlackMessageEvent {
        channel: "C123".to_owned(),
        user: Some("U123".to_owned()),
        bot_id: None,
        text: Some("hi".to_owned()),
        ts: "1.2".to_owned(),
        thread_ts: None,
        channel_type: Some("channel".to_owned()),
        team_id: Some("T123".to_owned()),
        subtype: None,
        files: None,
    })
}
