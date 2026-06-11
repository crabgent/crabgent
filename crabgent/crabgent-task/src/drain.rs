//! Stream drain logic that turns kernel events into incremental
//! [`TaskStore::append_output`] writes.
//!
//! Pulled out of [`crate::executor`] so the debounce + flush behaviour
//! can be tested with a fabricated stream, without standing up a real
//! [`crabgent_core::Kernel`].

use std::sync::Arc;
use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use crabgent_core::ActivityEventSummary;
use crabgent_core::error::KernelError;
use crabgent_core::hook::Event;
use crabgent_log::warn;
use crabgent_store::TaskId;
use crabgent_store::traits::TaskStore;
use futures::Stream;
use futures::StreamExt;

use crate::observer::notify_observers;
use crate::observer::{TaskActivityEvent, TaskActivityKind, TaskActivityMeta, TaskObserver};

/// Terminal state observed while draining the stream. The caller
/// finalises the task (`TaskStore::finish`) and dispatches notifiers.
#[derive(Debug, Clone)]
pub struct DrainOutcome {
    /// Final assistant text. Set when the stream emits `Event::Final`.
    pub final_text: Option<String>,
    /// Error string. Set when the stream yields `Err(_)` mid-flight,
    /// except for `KernelError::Paused`, which sets `paused` instead.
    pub error: Option<String>,
    /// The run exited cooperatively at a safe pause boundary
    /// (`KernelError::Paused`). The executor routes this to
    /// `TaskStore::pause` instead of a terminal `finish`.
    pub paused: bool,
}

impl DrainOutcome {
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.error.is_none()
    }
}

/// Pull every event from `stream`, persist token deltas to
/// [`TaskStore::append_output`] with size+time debouncing, and return
/// the terminal outcome.
///
/// The drain is fail-soft on persistence errors: store failures are
/// logged via `crabgent_log::warn!` and the in-memory buffer is kept so the
/// next successful flush includes the unsent text.
pub async fn drain_stream<S, St>(
    store: Arc<S>,
    task_id: TaskId,
    stream: St,
    output_debounce_bytes: usize,
    progress_debounce: Duration,
) -> DrainOutcome
where
    S: TaskStore + ?Sized,
    St: Stream<Item = Result<Event, KernelError>> + Unpin,
{
    drain_stream_inner(
        store,
        task_id,
        stream,
        output_debounce_bytes,
        progress_debounce,
        None,
    )
    .await
}

pub async fn drain_stream_observed<S, St>(
    store: Arc<S>,
    task_id: TaskId,
    stream: St,
    output_debounce_bytes: usize,
    progress_debounce: Duration,
    meta: TaskActivityMeta,
    observers: Vec<Arc<dyn TaskObserver>>,
) -> DrainOutcome
where
    S: TaskStore + ?Sized,
    St: Stream<Item = Result<Event, KernelError>> + Unpin,
{
    drain_stream_inner(
        store,
        task_id,
        stream,
        output_debounce_bytes,
        progress_debounce,
        Some(TaskDrainObserver { meta, observers }),
    )
    .await
}

async fn drain_stream_inner<S, St>(
    store: Arc<S>,
    task_id: TaskId,
    stream: St,
    output_debounce_bytes: usize,
    progress_debounce: Duration,
    observer: Option<TaskDrainObserver>,
) -> DrainOutcome
where
    S: TaskStore + ?Sized,
    St: Stream<Item = Result<Event, KernelError>> + Unpin,
{
    let mut buffer = String::new();
    let mut last_flush = Utc::now();
    let mut final_text: Option<String> = None;
    let mut error: Option<String> = None;
    let mut paused = false;
    let mut s = stream;

    while let Some(item) = s.next().await {
        match item {
            Ok(Event::Token(t)) => {
                observe_event(observer.as_ref(), &Event::Token(t.clone())).await;
                buffer.push_str(&t);
                if should_flush(
                    &buffer,
                    output_debounce_bytes,
                    last_flush,
                    progress_debounce,
                ) {
                    flush(&store, &task_id, &mut buffer, &mut last_flush).await;
                }
            }
            Ok(Event::Final(text)) => {
                observe_event(observer.as_ref(), &Event::Final(text.clone())).await;
                final_text = Some(text);
                break;
            }
            Ok(event) => {
                observe_event(observer.as_ref(), &event).await;
                // Tool lifecycle, notifications, and any future
                // non_exhaustive Event variants are not persisted as
                // task output; they remain visible via the streaming
                // channel and tracing instrumentation.
            }
            Err(KernelError::Paused) => {
                paused = true;
                break;
            }
            Err(e) => {
                error = Some(e.to_string());
                break;
            }
        }
    }

    if !buffer.is_empty() {
        flush(&store, &task_id, &mut buffer, &mut last_flush).await;
    }

    DrainOutcome {
        final_text,
        error,
        paused,
    }
}

#[derive(Clone)]
struct TaskDrainObserver {
    meta: TaskActivityMeta,
    observers: Vec<Arc<dyn TaskObserver>>,
}

async fn observe_event(observer: Option<&TaskDrainObserver>, event: &Event) {
    let Some(observer) = observer else {
        return;
    };
    notify_observers(
        &observer.observers,
        TaskActivityEvent {
            meta: observer.meta.clone(),
            kind: TaskActivityKind::Kernel(ActivityEventSummary::from_event(event)),
        },
    )
    .await;
}

fn should_flush(
    buffer: &str,
    output_debounce_bytes: usize,
    last_flush: DateTime<Utc>,
    progress_debounce: Duration,
) -> bool {
    if buffer.len() >= output_debounce_bytes {
        return true;
    }
    let elapsed = Utc::now().signed_duration_since(last_flush);
    let elapsed_std = elapsed.to_std().unwrap_or_default();
    elapsed_std >= progress_debounce
}

async fn flush<S>(
    store: &Arc<S>,
    task_id: &TaskId,
    buffer: &mut String,
    last_flush: &mut DateTime<Utc>,
) where
    S: TaskStore + ?Sized,
{
    if buffer.is_empty() {
        return;
    }
    if let Err(e) = store.append_output(task_id, buffer).await {
        warn!(
            task_id = %task_id,
            error = %e,
            "task drain: append_output failed; keeping buffer for retry"
        );
        return;
    }
    buffer.clear();
    *last_flush = Utc::now();
}

#[cfg(test)]
#[path = "drain_tests.rs"]
mod tests;
