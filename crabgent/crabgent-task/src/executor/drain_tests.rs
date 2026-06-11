use std::future::pending;
use std::time::Duration;

use crabgent_store::TaskId;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::drain_timeout;
use super::{CANCELLED_MESSAGE, TIMEOUT_MESSAGE};
use crate::DrainOutcome;

#[tokio::test]
async fn drain_timeout_cancels_before_grace_wait() {
    let id = TaskId::new();
    let cancel = CancellationToken::new();
    let cancel_for_drain = cancel.clone();
    let (cancelled_tx, cancelled_rx) = oneshot::channel();
    let drain = async move {
        cancel_for_drain.cancelled().await;
        let _receiver_dropped = cancelled_tx.send(()).is_err();
        pending::<DrainOutcome>().await
    };

    let outcome = drain_timeout::run_drain_with_timeout(
        &id,
        cancel,
        Duration::from_millis(10),
        Duration::from_millis(25),
        drain,
    )
    .await;

    cancelled_rx
        .await
        .expect("drain should observe timeout cancellation before grace elapses");
    assert_eq!(outcome.error.as_deref(), Some(TIMEOUT_MESSAGE));
}

#[tokio::test]
async fn drain_shutdown_cancel_waits_before_returning() {
    let id = TaskId::new();
    let cancel = CancellationToken::new();
    let cancel_for_drain = cancel.clone();
    let (started_tx, started_rx) = oneshot::channel();
    let (cancelled_tx, cancelled_rx) = oneshot::channel();
    let drain = async move {
        let _receiver_dropped = started_tx.send(()).is_err();
        cancel_for_drain.cancelled().await;
        let _receiver_dropped = cancelled_tx.send(()).is_err();
        pending::<DrainOutcome>().await
    };
    let handle = tokio::spawn({
        let cancel = cancel.clone();
        async move {
            drain_timeout::run_drain_with_timeout(
                &id,
                cancel,
                Duration::from_mins(1),
                Duration::from_millis(25),
                drain,
            )
            .await
        }
    });

    started_rx.await.expect("drain should start");
    cancel.cancel();
    cancelled_rx
        .await
        .expect("drain should observe shutdown cancellation");
    let outcome = handle.await.expect("timeout helper should join");
    assert_eq!(outcome.error.as_deref(), Some(CANCELLED_MESSAGE));
}

#[tokio::test]
async fn drain_shutdown_cancel_wins_over_cancelled_drain_error() {
    let id = TaskId::new();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let drain = async {
        DrainOutcome {
            paused: false,
            final_text: None,
            error: Some("cancelled".into()),
        }
    };

    let outcome = drain_timeout::run_drain_with_timeout(
        &id,
        cancel,
        Duration::from_mins(1),
        Duration::from_millis(25),
        drain,
    )
    .await;

    assert_eq!(outcome.error.as_deref(), Some(CANCELLED_MESSAGE));
}
