//! Per-run Slack stream consumer.

use std::sync::Arc;
use std::time::Duration;

use crabgent_core::RunId;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::api::SlackHttpClient;
use crate::block_kit::{StreamChunk, StreamHandle};
use crate::error::SlackError;

use super::types::SENTINEL_NOT_AGENT_ERRORS;

pub(crate) type SentinelHandler = Arc<dyn Fn(&str) + Send + Sync + 'static>;

pub(crate) struct RunConsumerOptions {
    pub(crate) initial_chunks: Vec<StreamChunk>,
    pub(crate) task_display_mode: Option<&'static str>,
    /// Idle window between flushes. Consecutive chunks coalesce into a
    /// single `chat.appendStream` call when no further chunk arrives
    /// within this window. `AgentProgressConfig::idle_flush_interval`
    /// in `types.rs` carries the production default.
    pub(crate) idle_flush_interval: Duration,
    pub(crate) sentinel_handler: Option<SentinelHandler>,
}

/// Owns lazy `chat.startStream` / `chat.appendStream` / `chat.stopStream`
/// sequencing for one kernel run.
pub(crate) struct RunConsumer;

impl RunConsumer {
    /// Spawn a consumer task for one run's ordered stream chunks.
    ///
    /// The consumer opens the stream eagerly with `initial_chunks` so the
    /// Slack card appears as soon as the run starts, before the first
    /// tool call or token chunk has been queued.
    #[must_use]
    pub(crate) fn spawn(
        client: Arc<SlackHttpClient>,
        run_id: RunId,
        channel_id: String,
        thread_ts: String,
        mut rx: mpsc::UnboundedReceiver<StreamChunk>,
        options: RunConsumerOptions,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let RunConsumerOptions {
                initial_chunks,
                task_display_mode,
                idle_flush_interval,
                sentinel_handler,
            } = options;
            let stream = match client
                .chat_start_stream(&channel_id, &thread_ts, task_display_mode, &initial_chunks)
                .await
            {
                Ok(handle) => Some(handle),
                Err(err) => {
                    if let Some(code) = sentinel_code(&err) {
                        if let Some(handler) = sentinel_handler.as_ref() {
                            handler(code);
                        }
                        drain(&mut rx).await;
                        return;
                    }
                    crabgent_log::warn!(
                        run_id = %run_id,
                        error = %err,
                        "slack chat.startStream failed"
                    );
                    None
                }
            };

            let mut buffer: Vec<StreamChunk> = Vec::new();
            loop {
                match timeout(idle_flush_interval, rx.recv()).await {
                    Ok(Some(chunk)) => buffer.push(chunk),
                    Ok(None) => break,
                    Err(_idle) => {
                        if !buffer.is_empty()
                            && let Some(handle) = stream.as_ref()
                        {
                            flush(&client, handle, &buffer, &run_id).await;
                            buffer.clear();
                        }
                    }
                }
            }
            if !buffer.is_empty()
                && let Some(handle) = stream.as_ref()
            {
                flush(&client, handle, &buffer, &run_id).await;
            }
            if let Some(handle) = stream
                && let Err(err) = client
                    .chat_stop_stream(&handle.channel, &handle.ts, &[])
                    .await
            {
                crabgent_log::warn!(
                    run_id = %run_id,
                    error = %err,
                    "slack chat.stopStream-on-close failed"
                );
            }
        })
    }
}

async fn flush(
    client: &SlackHttpClient,
    handle: &StreamHandle,
    chunks: &[StreamChunk],
    run_id: &RunId,
) {
    if let Err(err) = client
        .chat_append_stream(&handle.channel, &handle.ts, None, chunks)
        .await
    {
        crabgent_log::warn!(
            run_id = %run_id,
            error = %err,
            "slack chat.appendStream failed"
        );
    }
}

async fn drain(rx: &mut mpsc::UnboundedReceiver<StreamChunk>) {
    while rx.recv().await.is_some() {}
}

fn sentinel_code(err: &SlackError) -> Option<&str> {
    if let SlackError::ApiError { slack_code, .. } = err
        && SENTINEL_NOT_AGENT_ERRORS.contains(&slack_code.as_str())
    {
        return Some(slack_code);
    }
    None
}
