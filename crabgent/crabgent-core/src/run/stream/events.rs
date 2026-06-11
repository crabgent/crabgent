//! Event emission helpers for the streaming run loop.

use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use crate::error::KernelError;
use crate::hook::{Event, RunCtx};
use crate::hook_chain::HookChain;
use crate::types::{ToolCall, ToolResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamItemDelivery {
    Sent,
    DroppedFull,
    Closed,
}

pub(in crate::run) async fn emit_event(
    chain: &HookChain,
    ctx: &RunCtx,
    event: Event,
    tx: &mpsc::Sender<Result<Event, KernelError>>,
) -> Result<(), KernelError> {
    let outgoing = prepare_event(chain, ctx, event).await?;
    match send_stream_item(tx, Ok(outgoing)).await {
        StreamItemDelivery::Sent | StreamItemDelivery::DroppedFull => Ok(()),
        StreamItemDelivery::Closed => Err(KernelError::Internal("stream receiver dropped".into())),
    }
}

pub(super) async fn send_stream_item(
    tx: &mpsc::Sender<Result<Event, KernelError>>,
    item: Result<Event, KernelError>,
) -> StreamItemDelivery {
    match tx.try_send(item) {
        Ok(()) => StreamItemDelivery::Sent,
        Err(TrySendError::Full(item)) if should_wait_for_stream_item(&item) => {
            if tx.send(item).await.is_ok() {
                StreamItemDelivery::Sent
            } else {
                StreamItemDelivery::Closed
            }
        }
        Err(TrySendError::Full(_item)) => StreamItemDelivery::DroppedFull,
        Err(TrySendError::Closed(_)) => StreamItemDelivery::Closed,
    }
}

const fn should_wait_for_stream_item(item: &Result<Event, KernelError>) -> bool {
    !matches!(item, Ok(Event::Token(_) | Event::Reasoning(_)))
}

pub(super) async fn prepare_event(
    chain: &HookChain,
    ctx: &RunCtx,
    event: Event,
) -> Result<Event, KernelError> {
    let event = chain.apply_on_event(&event, ctx).await?;
    if let Event::Notification(note) = &event {
        chain.apply_on_notification(note, ctx).await?;
    }
    Ok(event)
}

pub(super) async fn emit_completed(
    chain: &HookChain,
    ctx: &RunCtx,
    call: &ToolCall,
    result: &ToolResult,
    tx: &mpsc::Sender<Result<Event, KernelError>>,
) -> Result<(), KernelError> {
    let event = Event::ToolCallCompleted {
        call: call.clone(),
        result: result.clone(),
    };
    emit_event(chain, ctx, event, tx).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{Notification, NotificationLevel, RunId, Subject};

    #[tokio::test]
    async fn emit_event_errors_when_stream_receiver_dropped() {
        let chain = HookChain::new();
        let ctx = RunCtx::new(RunId::new(), Subject::new("u"));
        let (tx, rx) = mpsc::channel(1);
        drop(rx);

        let err = emit_event(&chain, &ctx, Event::Token("x".into()), &tx)
            .await
            .expect_err("dropped receiver should stop stream emission");

        assert!(matches!(
            err,
            KernelError::Internal(msg) if msg == "stream receiver dropped"
        ));
    }

    #[tokio::test]
    async fn send_stream_item_sends_full_channel_final_after_backpressure_clears() {
        let (tx, mut rx) = mpsc::channel(1);

        let first = send_stream_item(&tx, Ok(Event::Token("first".into()))).await;
        let final_send = send_stream_item(&tx, Ok(Event::Final("second".into())));
        tokio::pin!(final_send);

        assert_eq!(first, StreamItemDelivery::Sent);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut final_send)
                .await
                .is_err(),
            "terminal send should wait for channel capacity"
        );
        assert!(matches!(
            rx.recv().await.expect("first item should be queued"),
            Ok(Event::Token(text)) if text == "first"
        ));
        assert_eq!(final_send.await, StreamItemDelivery::Sent);
        assert!(matches!(
            rx.recv().await.expect("final item should be queued"),
            Ok(Event::Final(text)) if text == "second"
        ));
    }

    #[tokio::test]
    async fn send_stream_item_sends_full_channel_error_after_backpressure_clears() {
        let (tx, mut rx) = mpsc::channel(1);

        let first = send_stream_item(&tx, Ok(Event::Token("first".into()))).await;
        let error_send = send_stream_item(&tx, Err(KernelError::Cancelled));
        tokio::pin!(error_send);

        assert_eq!(first, StreamItemDelivery::Sent);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut error_send)
                .await
                .is_err(),
            "terminal error send should wait for channel capacity"
        );
        assert!(matches!(
            rx.recv().await.expect("first item should be queued"),
            Ok(Event::Token(text)) if text == "first"
        ));
        assert_eq!(error_send.await, StreamItemDelivery::Sent);
        assert!(matches!(
            rx.recv().await.expect("error item should be queued"),
            Err(KernelError::Cancelled)
        ));
    }

    #[tokio::test]
    async fn send_stream_item_drops_full_delta() {
        let (tx, _rx) = mpsc::channel(1);

        let first = send_stream_item(&tx, Ok(Event::Token("first".into()))).await;
        let second = send_stream_item(&tx, Ok(Event::Token("second".into()))).await;

        assert_eq!(first, StreamItemDelivery::Sent);
        assert_eq!(second, StreamItemDelivery::DroppedFull);
    }

    #[tokio::test]
    async fn send_stream_item_waits_for_lifecycle_event_after_backpressure_clears() {
        let (tx, mut rx) = mpsc::channel(1);

        let first = send_stream_item(&tx, Ok(Event::Token("first".into()))).await;
        let lifecycle_send = send_stream_item(
            &tx,
            Ok(Event::ToolCallStarted(ToolCall {
                id: "call-1".into(),
                name: "lookup".into(),
                args: serde_json::json!({}),
                thought_signature: None,
            })),
        );
        tokio::pin!(lifecycle_send);

        assert_eq!(first, StreamItemDelivery::Sent);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut lifecycle_send)
                .await
                .is_err(),
            "lifecycle event send should wait for channel capacity"
        );
        assert!(matches!(
            rx.recv().await.expect("first item should be queued"),
            Ok(Event::Token(text)) if text == "first"
        ));
        assert_eq!(lifecycle_send.await, StreamItemDelivery::Sent);
        assert!(matches!(
            rx.recv().await.expect("lifecycle item should be queued"),
            Ok(Event::ToolCallStarted(call)) if call.id == "call-1"
        ));
    }

    #[test]
    fn send_stream_item_wait_predicate_covers_non_delta_events() {
        assert!(should_wait_for_stream_item(&Ok(Event::Notification(
            Notification {
                kind: "status".into(),
                message: "working".into(),
                level: NotificationLevel::Info,
            }
        ))));
        assert!(should_wait_for_stream_item(&Ok(Event::ServerToolResult {
            provider: "google".into(),
            name: "google_search".into(),
            citations: Vec::new(),
            raw: serde_json::json!({"ok": true}),
        })));
        assert!(should_wait_for_stream_item(&Err(KernelError::Cancelled)));
        assert!(!should_wait_for_stream_item(&Ok(Event::Token(
            "delta".into()
        ))));
        assert!(!should_wait_for_stream_item(&Ok(Event::Reasoning(
            "delta".into()
        ))));
    }

    #[tokio::test]
    async fn send_stream_item_waits_for_server_tool_result_after_backpressure_clears() {
        let (tx, mut rx) = mpsc::channel(1);

        let first = send_stream_item(&tx, Ok(Event::Token("first".into()))).await;
        let non_delta_send = send_stream_item(
            &tx,
            Ok(Event::ServerToolResult {
                provider: "google".into(),
                name: "google_search".into(),
                citations: Vec::new(),
                raw: serde_json::json!({"ok": true}),
            }),
        );
        tokio::pin!(non_delta_send);

        assert_eq!(first, StreamItemDelivery::Sent);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut non_delta_send)
                .await
                .is_err(),
            "non-delta event send should wait for channel capacity"
        );
        assert!(matches!(
            rx.recv().await.expect("first item should be queued"),
            Ok(Event::Token(text)) if text == "first"
        ));
        assert_eq!(non_delta_send.await, StreamItemDelivery::Sent);
        assert!(matches!(
            rx.recv().await.expect("non-delta item should be queued"),
            Ok(Event::ServerToolResult { provider, name, .. })
                if provider == "google" && name == "google_search"
        ));
    }
}
