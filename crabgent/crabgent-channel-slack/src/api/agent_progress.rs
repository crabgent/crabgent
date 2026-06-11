//! Slack agent-progress endpoints: `assistant.threads.setStatus` and the
//! `chat.startStream` / `chat.appendStream` / `chat.stopStream` triad.
//!
//! Each method mirrors the JSON-POST template established by
//! `post_message`: serialize a typed request body, hit the endpoint via
//! `retry_json` with the bot token, and let `decode_slack_response`
//! surface `ok=false` payloads as `SlackError::ApiError`.

use serde::{Deserialize, Serialize};

use crate::api::SlackHttpClient;
use crate::block_kit::{StreamChunk, StreamHandle};
use crate::error::SlackError;

#[derive(Debug, Serialize)]
struct SetStatusRequest<'a> {
    channel_id: &'a str,
    thread_ts: &'a str,
    status: &'a str,
}

#[derive(Debug, Serialize)]
struct StartStreamRequest<'a> {
    channel: &'a str,
    thread_ts: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_display_mode: Option<&'a str>,
    chunks: &'a [StreamChunk],
}

#[derive(Debug, Serialize)]
struct AppendStreamRequest<'a> {
    channel: &'a str,
    ts: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    markdown_text: Option<&'a str>,
    chunks: &'a [StreamChunk],
}

#[derive(Debug, Serialize)]
struct StopStreamRequest<'a> {
    channel: &'a str,
    ts: &'a str,
    chunks: &'a [StreamChunk],
}

#[derive(Debug, Deserialize)]
struct AckResponse {}

#[derive(Debug, Deserialize)]
struct StartStreamResponse {
    channel: String,
    ts: String,
}

impl SlackHttpClient {
    /// Call `assistant.threads.setStatus`.
    ///
    /// Sends the current "agent is thinking" label on a Slack assistant
    /// thread. An empty `status` clears the label.
    ///
    /// # Errors
    ///
    /// Returns `SlackError::ApiError` when Slack responds with
    /// `ok=false` (e.g. `feature_not_supported`,
    /// `not_allowed_token_type`, `access_denied`), `SlackError::Transport`
    /// on network failures, or other variants per `decode_slack_response`.
    #[crabgent_log::instrument(skip(self))]
    pub async fn assistant_threads_set_status(
        &self,
        channel_id: &str,
        thread_ts: &str,
        status: &str,
    ) -> Result<(), SlackError> {
        let body = SetStatusRequest {
            channel_id,
            thread_ts,
            status,
        };
        let _ack: AckResponse = self
            .retry_json("assistant.threads.setStatus", &body, self.bot_token())
            .await?;
        Ok(())
    }

    /// Call `chat.startStream`.
    ///
    /// Opens a streaming message and returns the `(channel, ts)` handle
    /// that addresses subsequent `chat.appendStream` / `chat.stopStream`
    /// calls.
    ///
    /// # Errors
    ///
    /// Returns `SlackError::ApiError` when Slack responds with
    /// `ok=false`, `SlackError::Transport` on network failures, or other
    /// variants per `decode_slack_response`.
    #[crabgent_log::instrument(skip(self, chunks))]
    pub async fn chat_start_stream(
        &self,
        channel: &str,
        thread_ts: &str,
        task_display_mode: Option<&str>,
        chunks: &[StreamChunk],
    ) -> Result<StreamHandle, SlackError> {
        let body = StartStreamRequest {
            channel,
            thread_ts,
            task_display_mode,
            chunks,
        };
        let response: StartStreamResponse = self
            .retry_json("chat.startStream", &body, self.bot_token())
            .await?;
        Ok(StreamHandle {
            channel: response.channel,
            ts: response.ts,
        })
    }

    /// Call `chat.appendStream`.
    ///
    /// Appends content to an existing streaming message identified by
    /// the `(channel, ts)` handle from `chat.startStream`. Callers may
    /// pass either `markdown_text` (shorthand for a single markdown
    /// chunk) or structured `chunks`, or both.
    ///
    /// # Errors
    ///
    /// Returns `SlackError::ApiError` when Slack responds with
    /// `ok=false`, `SlackError::Transport` on network failures, or other
    /// variants per `decode_slack_response`.
    #[crabgent_log::instrument(skip(self, chunks))]
    pub async fn chat_append_stream(
        &self,
        channel: &str,
        ts: &str,
        markdown_text: Option<&str>,
        chunks: &[StreamChunk],
    ) -> Result<(), SlackError> {
        let body = AppendStreamRequest {
            channel,
            ts,
            markdown_text,
            chunks,
        };
        let _ack: AckResponse = self
            .retry_json("chat.appendStream", &body, self.bot_token())
            .await?;
        Ok(())
    }

    /// Call `chat.stopStream`.
    ///
    /// Closes an active streaming message identified by the
    /// `(channel, ts)` handle from `chat.startStream`. The optional
    /// `chunks` slice carries any final content to render before
    /// finalisation.
    ///
    /// # Errors
    ///
    /// Returns `SlackError::ApiError` when Slack responds with
    /// `ok=false`, `SlackError::Transport` on network failures, or other
    /// variants per `decode_slack_response`.
    #[crabgent_log::instrument(skip(self, chunks))]
    pub async fn chat_stop_stream(
        &self,
        channel: &str,
        ts: &str,
        chunks: &[StreamChunk],
    ) -> Result<(), SlackError> {
        let body = StopStreamRequest {
            channel,
            ts,
            chunks,
        };
        let _ack: AckResponse = self
            .retry_json("chat.stopStream", &body, self.bot_token())
            .await?;
        Ok(())
    }
}
