use std::sync::Arc;
use std::time::{Duration, Instant};

use crabgent_core::error::KernelError;
use crabgent_core::hook::{CancelReason, Event};
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;
use crabgent_core::types::{ToolCall, ToolResult};
use crabgent_log::warn;
use serde_json::Value;

use crate::channel::ChannelKind;
use crate::envelope::{MessageRef, OutboundMessage};
use crate::sink::ChannelSink;
use crate::subject::{ChannelSubjectExt, attr_keys};

#[path = "live_turn_render.rs"]
mod live_turn_render;
use live_turn_render::{
    compact_line, current_participant_id, display_tool_name, is_builtin_channel_response_tool,
    public_error_status, render_attempt_failed, render_tool_completed, should_silence_error,
    tool_result_error_hint,
};

const DEFAULT_EDIT_THROTTLE: Duration = Duration::from_millis(1500);
const CHANNEL_SEND_TOOL: &str = "channel_send";
const CHANNEL_EDIT_TOOL: &str = "channel_edit";
const CHANNEL_UPLOAD_TOOL: &str = "channel_upload";
const CHANNEL_REACT_TOOL: &str = "channel_react";
const NOTIFY_USER_TOOL: &str = "notify_user";
const DEFAULT_RESPONSE_TOOLS: &[&str] = &[
    CHANNEL_SEND_TOOL,
    CHANNEL_EDIT_TOOL,
    CHANNEL_UPLOAD_TOOL,
    CHANNEL_REACT_TOOL,
];
const DEFAULT_IGNORED_PROGRESS_TOOLS: &[&str] = &[
    CHANNEL_SEND_TOOL,
    CHANNEL_EDIT_TOOL,
    CHANNEL_UPLOAD_TOOL,
    CHANNEL_REACT_TOOL,
    NOTIFY_USER_TOOL,
];
const MAX_STATUS_BYTES: usize = 160;
const MAX_ERROR_BYTES: usize = 240;
const DEFAULT_EMPTY_FINAL_STATUS: &str = "No response produced.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveProgressMode {
    Lazy,
    Eager,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalDeliveryPolicy {
    EditProgressMessage,
    SendSeparateMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveTurnConfig {
    pub edit_throttle: Duration,
    pub progress_mode: LiveProgressMode,
    pub final_delivery_policy: FinalDeliveryPolicy,
    pub ignored_tools: Vec<String>,
    pub response_tools: Vec<String>,
    pub empty_final_status: String,
}

impl Default for LiveTurnConfig {
    fn default() -> Self {
        Self {
            edit_throttle: DEFAULT_EDIT_THROTTLE,
            progress_mode: LiveProgressMode::Lazy,
            final_delivery_policy: FinalDeliveryPolicy::EditProgressMessage,
            ignored_tools: DEFAULT_IGNORED_PROGRESS_TOOLS
                .iter()
                .map(ToString::to_string)
                .collect(),
            response_tools: DEFAULT_RESPONSE_TOOLS
                .iter()
                .map(ToString::to_string)
                .collect(),
            empty_final_status: DEFAULT_EMPTY_FINAL_STATUS.to_owned(),
        }
    }
}

impl LiveTurnConfig {
    #[must_use]
    pub const fn with_edit_throttle(mut self, throttle: Duration) -> Self {
        self.edit_throttle = throttle;
        self
    }

    #[must_use]
    pub const fn with_progress_mode(mut self, mode: LiveProgressMode) -> Self {
        self.progress_mode = mode;
        self
    }

    #[must_use]
    pub const fn with_final_delivery_policy(mut self, policy: FinalDeliveryPolicy) -> Self {
        self.final_delivery_policy = policy;
        self
    }

    #[must_use]
    pub fn with_ignored_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.ignored_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_response_tools<I, S>(mut self, tools: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.response_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_empty_final_status(mut self, status: impl Into<String>) -> Self {
        self.empty_final_status = status.into();
        self
    }
}

#[derive(Clone)]
pub(super) struct LiveTurnDelivery {
    pub(super) sink: Arc<dyn ChannelSink>,
    pub(super) config: LiveTurnConfig,
}

impl LiveTurnDelivery {
    pub(super) fn new(sink: Arc<dyn ChannelSink>, config: LiveTurnConfig) -> Self {
        Self { sink, config }
    }
}

pub(super) struct LiveTurnState {
    delivery: LiveTurnDelivery,
    subject: Subject,
    conv: Owner,
    channel: String,
    progress_ref: Option<MessageRef>,
    last_render: Option<String>,
    pending_render: Option<String>,
    last_edit_at: Option<Instant>,
    response_side_effect_succeeded: bool,
    last_response_error: Option<String>,
}

impl LiveTurnState {
    pub(super) const fn new(
        delivery: LiveTurnDelivery,
        subject: Subject,
        conv: Owner,
        channel: String,
    ) -> Self {
        Self {
            delivery,
            subject,
            conv,
            channel,
            progress_ref: None,
            last_render: None,
            pending_render: None,
            last_edit_at: None,
            response_side_effect_succeeded: false,
            last_response_error: None,
        }
    }

    pub(super) async fn observe(&mut self, event: &Event) {
        match event {
            Event::ToolCallStarted(call) if self.should_render_tool(&call.name) => {
                self.render_progress(format!("Using {}...", display_tool_name(&call.name)))
                    .await;
            }
            Event::ToolCallCompleted { call, result } => {
                self.record_channel_tool_result(call, result);
                if self.should_render_tool(&call.name) {
                    self.render_progress(render_tool_completed(call, result))
                        .await;
                }
            }
            Event::Notification(note) => {
                let body = compact_line(&note.message, MAX_STATUS_BYTES);
                if !body.is_empty() {
                    self.render_progress(body).await;
                }
            }
            Event::ServerToolResult { name, .. } => {
                self.render_progress(format!("{} done", display_tool_name(name)))
                    .await;
            }
            Event::AttemptFailed {
                provider,
                model,
                error_class,
                message,
                will_fallback,
                ..
            } => {
                self.render_progress(render_attempt_failed(
                    provider,
                    model,
                    error_class,
                    message,
                    *will_fallback,
                ))
                .await;
            }
            Event::Reasoning(_)
                if (self.progress_ref.is_some()
                    || self.delivery.config.progress_mode == LiveProgressMode::Eager) =>
            {
                self.render_progress("Working...").await;
            }
            _ => {}
        }
    }

    pub(super) async fn finish_success(&mut self, text: &str) {
        if self.response_side_effect_succeeded {
            self.finish_progress_only().await;
            return;
        }

        let body = text.trim();
        if body.is_empty() {
            if let Some(error) = self.last_response_error.clone() {
                self.deliver_status(format!("Delivery failed: {error}"))
                    .await;
            } else {
                let status = self.delivery.config.empty_final_status.trim().to_owned();
                if status.is_empty() {
                    self.finish_progress_only().await;
                } else {
                    self.deliver_status(status).await;
                }
            }
            return;
        }

        self.deliver_final(body).await;
    }

    pub(super) async fn finish_error(
        &mut self,
        err: &KernelError,
        cancel_reason: Option<CancelReason>,
        shutting_down: bool,
    ) {
        if self.response_side_effect_succeeded {
            self.finish_progress_only().await;
            return;
        }
        if should_silence_error(err, cancel_reason, shutting_down) {
            return;
        }
        if let Some(error) = self.last_response_error.clone() {
            self.deliver_status(format!("Delivery failed: {error}"))
                .await;
            return;
        }
        self.deliver_status(public_error_status(err)).await;
    }

    fn should_render_tool(&self, tool: &str) -> bool {
        !self
            .delivery
            .config
            .ignored_tools
            .iter()
            .any(|ignored| ignored == tool)
    }

    fn record_channel_tool_result(&mut self, call: &ToolCall, result: &ToolResult) {
        let is_channel_tool = is_builtin_channel_response_tool(&call.name)
            || self
                .delivery
                .config
                .response_tools
                .iter()
                .any(|tool| tool == &call.name)
            || call.name == NOTIFY_USER_TOOL;
        if !is_channel_tool {
            return;
        }

        if result.is_error {
            self.last_response_error = Some(tool_result_error_hint(result));
            return;
        }

        if self.suppresses_final_delivery(call) {
            self.response_side_effect_succeeded = true;
        }
    }

    fn suppresses_final_delivery(&self, call: &ToolCall) -> bool {
        if is_builtin_channel_response_tool(&call.name)
            || self
                .delivery
                .config
                .response_tools
                .iter()
                .any(|tool| tool == &call.name)
        {
            return true;
        }
        call.name == NOTIFY_USER_TOOL && self.notify_user_targets_current_direct_subject(call)
    }

    fn notify_user_targets_current_direct_subject(&self, call: &ToolCall) -> bool {
        if self.subject.attr(attr_keys::CHANNEL_KIND) != Some(ChannelKind::Direct.as_str()) {
            return false;
        }
        let Some(channel) = call.args.get("channel").and_then(Value::as_str) else {
            return false;
        };
        if channel != self.channel {
            return false;
        }
        let Some(recipient) = call.args.get("participant_id").and_then(Value::as_str) else {
            return false;
        };
        current_participant_id(&self.subject, channel).as_deref() == Some(recipient)
    }

    async fn render_progress(&mut self, body: impl Into<String>) {
        let body = body.into();
        if body.trim().is_empty() || self.current_render() == Some(body.as_str()) {
            return;
        }

        if self.progress_ref.is_none() {
            self.send_progress(body).await;
            return;
        }

        if self.should_edit_now() {
            self.edit_progress(body).await;
        } else {
            self.pending_render = Some(body);
        }
    }

    async fn finish_progress_only(&mut self) {
        if self.progress_ref.is_some() {
            self.edit_progress_unthrottled("Done.").await;
        }
    }

    async fn deliver_status(&mut self, body: String) {
        if self.progress_ref.is_some() && self.edit_progress_unthrottled(&body).await {
            return;
        }
        self.send_final(body).await;
    }

    async fn deliver_final(&mut self, body: &str) {
        if self.progress_ref.is_some()
            && self.delivery.config.final_delivery_policy
                == FinalDeliveryPolicy::EditProgressMessage
            && self.edit_progress_unthrottled(body).await
        {
            return;
        }
        self.send_final(body.to_owned()).await;
    }

    async fn send_progress(&mut self, body: String) {
        let msg = self.outbound(body.clone());
        match self
            .delivery
            .sink
            .send(&self.subject, &self.conv, &msg)
            .await
        {
            Ok(reference) => {
                self.progress_ref = Some(reference);
                self.last_render = Some(body);
                self.pending_render = None;
                self.last_edit_at = Some(Instant::now());
            }
            Err(err) => warn!("live turn progress send failed: {err}"),
        }
    }

    async fn edit_progress(&mut self, body: String) {
        if self.edit_progress_unthrottled(&body).await {
            return;
        }
        self.pending_render = Some(body);
    }

    async fn edit_progress_unthrottled(&mut self, body: &str) -> bool {
        let Some(target) = self.progress_ref.clone() else {
            self.send_progress(body.to_owned()).await;
            return self.progress_ref.is_some();
        };
        match self
            .delivery
            .sink
            .edit(&self.subject, &self.conv, &target, body)
            .await
        {
            Ok(()) => {
                self.last_render = Some(body.to_owned());
                self.pending_render = None;
                self.last_edit_at = Some(Instant::now());
                true
            }
            Err(err) => {
                warn!("live turn progress edit failed: {err}");
                false
            }
        }
    }

    async fn send_final(&self, body: String) {
        let msg = self.outbound(body);
        if let Err(err) = self
            .delivery
            .sink
            .send(&self.subject, &self.conv, &msg)
            .await
        {
            warn!("live turn final send failed: {err}");
        }
    }

    fn outbound(&self, body: String) -> OutboundMessage {
        let mut msg = OutboundMessage::new(body).with_metadata("channel", self.channel.clone());
        if let Some(parent) = self.subject.inbound_message_ref() {
            msg = msg.in_thread(parent);
        }
        msg
    }

    fn current_render(&self) -> Option<&str> {
        self.pending_render
            .as_deref()
            .or(self.last_render.as_deref())
    }

    fn should_edit_now(&self) -> bool {
        self.last_edit_at
            .is_none_or(|last| last.elapsed() >= self.delivery.config.edit_throttle)
    }
}
