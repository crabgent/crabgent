//! Channel-backed cron delivery.
//!
//! Cron execution is per-agent in this crate, so final cron text must be
//! delivered through the same agent's channel router. This adapter keeps the
//! scheduler generic while preserving the configured Matrix/Telegram/TUI
//! identity for each job.

use std::{borrow::Cow, collections::HashMap, sync::Arc};

use async_trait::async_trait;
use crabgent_channel::{ChannelSink, OutboundMessage, ParticipantId};
use crabgent_core::{Owner, Subject};
use crabgent_cron::{CronDelivery, CronError};
use crabgent_log::{info, warn};
use crabgent_store::records::CronJob;

use crate::runtime::AgentHandle;

pub struct ChannelCronDelivery {
    per_agent: HashMap<String, Arc<dyn ChannelSink>>,
}

pub struct FinalTextCronDelivery {
    inner: Arc<dyn CronDelivery>,
}

impl FinalTextCronDelivery {
    #[must_use]
    pub fn new(inner: Arc<dyn CronDelivery>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl CronDelivery for FinalTextCronDelivery {
    async fn deliver(&self, job: &CronJob, message: &str) -> Result<bool, CronError> {
        if final_text_delivery_suppressed(job) {
            info!(
                job = %job.name,
                agent = job.scope.agent.as_deref().unwrap_or(""),
                "cron: suppressed final text delivery",
            );
            return Ok(true);
        }
        self.inner.deliver(job, message).await
    }
}

impl ChannelCronDelivery {
    #[must_use]
    pub fn new(per_agent: HashMap<String, Arc<dyn ChannelSink>>) -> Self {
        Self { per_agent }
    }

    #[must_use]
    pub fn from_handles(handles: &[AgentHandle]) -> Self {
        Self::new(
            handles
                .iter()
                .map(|handle| (handle.name.clone(), Arc::clone(&handle.channel_sink)))
                .collect(),
        )
    }

    fn sink_for(&self, job: &CronJob) -> Result<Arc<dyn ChannelSink>, CronError> {
        let agent = job
            .scope
            .agent
            .as_deref()
            .ok_or_else(|| CronError::delivery("cron delivery requires scope.agent"))?;
        self.per_agent.get(agent).cloned().ok_or_else(|| {
            CronError::delivery(format!(
                "cron delivery scope.agent={agent:?} is not configured"
            ))
        })
    }
}

fn final_text_delivery_suppressed(job: &CronJob) -> bool {
    ctx_bool(job, "silent")
        || ctx_bool(job, "silent_final")
        || job
            .delivery_ctx
            .get("deliver_final")
            .and_then(serde_json::Value::as_bool)
            .is_some_and(|value| !value)
}

#[async_trait]
impl CronDelivery for ChannelCronDelivery {
    async fn deliver(&self, job: &CronJob, message: &str) -> Result<bool, CronError> {
        if message.trim().is_empty() {
            return Ok(true);
        }
        let sink = self.sink_for(job)?;
        let channel = delivery_channel(job)
            .ok_or_else(|| CronError::delivery("cron delivery target missing channel"))?;
        let target = delivery_target(job, channel)?;
        let subject = delivery_subject(job, channel);
        let msg = OutboundMessage::new(message).with_metadata("channel", channel);
        let result = match &target {
            DeliveryTarget::Notify(recipient) => {
                sink.notify_user(&subject, &ParticipantId::new(recipient.clone()), &msg)
                    .await
            }
            DeliveryTarget::Send(conv) => {
                sink.send(&subject, &Owner::new(conv.clone()), &msg).await
            }
        };
        match result {
            Ok(_) => {
                info!(
                    job = %job.name,
                    agent = job.scope.agent.as_deref().unwrap_or(""),
                    channel,
                    target = %target,
                    "cron: delivered final text to channel",
                );
                Ok(true)
            }
            Err(err) => {
                warn!(
                    job = %job.name,
                    agent = job.scope.agent.as_deref().unwrap_or(""),
                    channel,
                    target = %target,
                    error = ?err,
                    "cron: channel delivery failed",
                );
                Err(CronError::delivery(format!("{err:?}")))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeliveryTarget {
    Notify(String),
    Send(String),
}

impl std::fmt::Display for DeliveryTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Notify(recipient) => write!(f, "notify:{recipient}"),
            Self::Send(conv) => write!(f, "send:{conv}"),
        }
    }
}

fn delivery_channel(job: &CronJob) -> Option<&str> {
    ctx_str(job, "channel")
        .or(job.scope.channel.as_deref())
        .or_else(|| prefixed_channel(ctx_str(job, "conv")?))
        .or_else(|| prefixed_channel(ctx_str(job, "owner")?))
        .or_else(|| prefixed_channel(job.scope.conv.as_deref()?))
        .or_else(|| prefixed_channel(job.scope.owner.as_ref()?.as_str()))
}

fn delivery_target(job: &CronJob, channel: &str) -> Result<DeliveryTarget, CronError> {
    if let Some(recipient) = ctx_str(job, "participant_id")
        .or_else(|| ctx_str(job, "recipient"))
        .or_else(|| ctx_str(job, "user"))
    {
        return Ok(DeliveryTarget::Notify(normalize_notify_recipient(
            channel, recipient,
        )));
    }

    for value in [
        ctx_str(job, "conv"),
        ctx_str(job, "owner"),
        job.scope.conv.as_deref(),
        job.scope.owner.as_ref().map(Owner::as_str),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(target) = target_from_prefixed_value(channel, value) {
            return Ok(target);
        }
    }

    Err(CronError::delivery(
        "cron delivery target missing participant_id or conv",
    ))
}

fn target_from_prefixed_value(channel: &str, value: &str) -> Option<DeliveryTarget> {
    let rest = strip_channel_prefix(channel, value)?;
    if should_notify_prefixed_target(channel, rest) {
        Some(DeliveryTarget::Notify(normalize_notify_recipient(
            channel, rest,
        )))
    } else {
        Some(DeliveryTarget::Send(value.to_owned()))
    }
}

fn normalize_notify_recipient(channel: &str, recipient: &str) -> String {
    if channel != "matrix" {
        return recipient.to_owned();
    }
    percent_decode(recipient).into_owned()
}

fn percent_decode(input: &str) -> Cow<'_, str> {
    let bytes = input.as_bytes();
    let mut decoded: Option<Vec<u8>> = None;
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%'
            && idx + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[idx + 1]), hex_value(bytes[idx + 2]))
        {
            let out = decoded.get_or_insert_with(|| bytes[..idx].to_vec());
            out.push((hi << 4) | lo);
            idx += 3;
            continue;
        }
        if let Some(out) = &mut decoded {
            out.push(bytes[idx]);
        }
        idx += 1;
    }
    decoded.map_or(Cow::Borrowed(input), |out| {
        String::from_utf8(out).map_or_else(
            |err| Cow::Owned(String::from_utf8_lossy(err.as_bytes()).into_owned()),
            Cow::Owned,
        )
    })
}

const fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn should_notify_prefixed_target(channel: &str, rest: &str) -> bool {
    match channel {
        "matrix" => rest.starts_with('@'),
        "telegram" | "tui" => true,
        _ => false,
    }
}

fn delivery_subject(job: &CronJob, channel: &str) -> Subject {
    let owner = job.scope.owner.as_ref().map_or_else(
        || format!("cron:{}", job.id),
        |owner| owner.as_str().to_owned(),
    );
    let mut subject = Subject::new(owner)
        .with_attr("channel", channel)
        .with_attr("cron_job_id", job.id.to_string());
    if let Some(agent) = &job.scope.agent {
        subject = subject.with_attr("agent", agent);
    }
    if let Some(conv) = ctx_str(job, "conv").or(job.scope.conv.as_deref()) {
        subject = subject.with_attr("conv", conv);
    }
    if let Some(kind) = &job.scope.kind {
        subject = subject.with_attr("channel_kind", kind);
    }
    subject
}

fn ctx_str<'a>(job: &'a CronJob, key: &str) -> Option<&'a str> {
    job.delivery_ctx
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn ctx_bool(job: &CronJob, key: &str) -> bool {
    job.delivery_ctx
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn prefixed_channel(value: &str) -> Option<&str> {
    let (channel, rest) = value.split_once(':')?;
    (!channel.is_empty() && !rest.is_empty()).then_some(channel)
}

fn strip_channel_prefix<'a>(channel: &str, value: &'a str) -> Option<&'a str> {
    let (prefix, rest) = value.split_once(':')?;
    (prefix == channel && !rest.is_empty()).then_some(rest)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::Utc;
    use crabgent_channel::{ChannelError, MessageRef, ReadMessage};
    use crabgent_core::MemoryScope;
    use crabgent_store::CronJobId;
    use crabgent_store::records::CronSchedule;
    use serde_json::json;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum RecordedCall {
        Notify {
            recipient: String,
            channel: Option<String>,
            body: String,
        },
        Send {
            conv: String,
            channel: Option<String>,
            body: String,
        },
    }

    struct RecordingSink {
        calls: Arc<Mutex<Vec<RecordedCall>>>,
    }

    impl RecordingSink {
        fn new(calls: Arc<Mutex<Vec<RecordedCall>>>) -> Self {
            Self { calls }
        }

        fn record(&self, call: RecordedCall) {
            self.calls.lock().expect("recording sink mutex").push(call);
        }
    }

    #[async_trait]
    impl ChannelSink for RecordingSink {
        async fn send(
            &self,
            _ctx: &Subject,
            conv: &Owner,
            msg: &OutboundMessage,
        ) -> Result<MessageRef, ChannelError> {
            self.record(RecordedCall::Send {
                conv: conv.as_str().to_owned(),
                channel: msg.metadata.get("channel").cloned(),
                body: msg.body.clone(),
            });
            Ok(MessageRef::top_level("test", conv.clone(), "send-1"))
        }

        async fn react(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
            _parent: &MessageRef,
            _emoji: &str,
        ) -> Result<MessageRef, ChannelError> {
            Err(ChannelError::Unsupported("react"))
        }

        async fn notify_user(
            &self,
            _ctx: &Subject,
            recipient: &ParticipantId,
            msg: &OutboundMessage,
        ) -> Result<MessageRef, ChannelError> {
            self.record(RecordedCall::Notify {
                recipient: recipient.as_str().to_owned(),
                channel: msg.metadata.get("channel").cloned(),
                body: msg.body.clone(),
            });
            Ok(MessageRef::top_level(
                "test",
                Owner::new(format!("test:{}", recipient.as_str())),
                "notify-1",
            ))
        }

        async fn read(
            &self,
            _ctx: &Subject,
            _conv: &Owner,
            _thread_parent: Option<&MessageRef>,
            _limit: usize,
        ) -> Result<Vec<ReadMessage>, ChannelError> {
            Err(ChannelError::Unsupported("read"))
        }
    }

    fn fixture(delivery_ctx: serde_json::Value) -> CronJob {
        CronJob {
            id: CronJobId::new(),
            name: "mail-watcher".into(),
            scope: MemoryScope::for_owner(Owner::new("matrix:@alice:matrix.example"))
                .with_channel("matrix")
                .with_agent("local")
                .with_kind("direct"),
            prompt: "check mail".into(),
            schedule: CronSchedule::every(60),
            enabled: true,
            run_once: false,
            model_override: None,
            reasoning_effort_override: None,
            pre_command: None,
            delivery_ctx,
            last_run: None,
            next_run: Utc::now(),
            created_at: Utc::now(),
            claimed_at: None,
        }
    }

    fn delivery(calls: Arc<Mutex<Vec<RecordedCall>>>) -> ChannelCronDelivery {
        let sink: Arc<dyn ChannelSink> = Arc::new(RecordingSink::new(calls));
        ChannelCronDelivery::new(HashMap::from([("local".to_owned(), sink)]))
    }

    #[tokio::test]
    async fn participant_id_uses_notify_user() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let delivery = delivery(Arc::clone(&calls));
        let job = fixture(json!({
            "channel": "matrix",
            "participant_id": "@alice:matrix.example"
        }));

        let delivered = delivery
            .deliver(&job, "2 neue Mails")
            .await
            .expect("delivery");

        assert!(delivered);
        assert_eq!(
            *calls.lock().expect("recording sink mutex"),
            vec![RecordedCall::Notify {
                recipient: "@alice:matrix.example".into(),
                channel: Some("matrix".into()),
                body: "2 neue Mails".into(),
            }]
        );
    }

    #[tokio::test]
    async fn encoded_matrix_participant_id_is_decoded_for_notify_user() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let delivery = delivery(Arc::clone(&calls));
        let job = fixture(json!({
            "channel": "matrix",
            "participant_id": "@alice%3Amatrix.example"
        }));

        let delivered = delivery
            .deliver(&job, "2 neue Mails")
            .await
            .expect("delivery");

        assert!(delivered);
        assert_eq!(
            *calls.lock().expect("recording sink mutex"),
            vec![RecordedCall::Notify {
                recipient: "@alice:matrix.example".into(),
                channel: Some("matrix".into()),
                body: "2 neue Mails".into(),
            }]
        );
    }

    #[tokio::test]
    async fn encoded_matrix_owner_is_decoded_for_notify_user() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let delivery = delivery(Arc::clone(&calls));
        let mut job = fixture(json!({}));
        job.scope.owner = Some(Owner::new("matrix:@alice%3Amatrix.example"));

        let delivered = delivery.deliver(&job, "Report").await.expect("delivery");

        assert!(delivered);
        assert_eq!(
            *calls.lock().expect("recording sink mutex"),
            vec![RecordedCall::Notify {
                recipient: "@alice:matrix.example".into(),
                channel: Some("matrix".into()),
                body: "Report".into(),
            }]
        );
    }

    #[tokio::test]
    async fn matrix_room_conv_uses_send() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let delivery = delivery(Arc::clone(&calls));
        let job = fixture(json!({
            "channel": "matrix",
            "conv": "matrix:!room:matrix.example"
        }));

        let delivered = delivery.deliver(&job, "Report").await.expect("delivery");

        assert!(delivered);
        assert_eq!(
            *calls.lock().expect("recording sink mutex"),
            vec![RecordedCall::Send {
                conv: "matrix:!room:matrix.example".into(),
                channel: Some("matrix".into()),
                body: "Report".into(),
            }]
        );
    }

    #[tokio::test]
    async fn empty_final_text_stays_silent() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let delivery = delivery(Arc::clone(&calls));
        let job = fixture(json!({
            "channel": "matrix",
            "participant_id": "@alice:matrix.example"
        }));

        let delivered = delivery.deliver(&job, "  \n").await.expect("delivery");

        assert!(delivered);
        assert!(calls.lock().expect("recording sink mutex").is_empty());
    }

    #[tokio::test]
    async fn final_text_delivery_can_be_suppressed() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let inner: Arc<dyn CronDelivery> = Arc::new(delivery(Arc::clone(&calls)));
        let delivery = FinalTextCronDelivery::new(inner);
        let job = fixture(json!({
            "channel": "matrix",
            "participant_id": "@alice:matrix.example",
            "deliver_final": false
        }));

        let delivered = delivery.deliver(&job, "Report").await.expect("delivery");

        assert!(delivered);
        assert!(calls.lock().expect("recording sink mutex").is_empty());
    }

    #[tokio::test]
    async fn final_text_delivery_still_delivers_by_default() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let inner: Arc<dyn CronDelivery> = Arc::new(delivery(Arc::clone(&calls)));
        let delivery = FinalTextCronDelivery::new(inner);
        let job = fixture(json!({
            "channel": "matrix",
            "participant_id": "@alice:matrix.example"
        }));

        let delivered = delivery.deliver(&job, "Report").await.expect("delivery");

        assert!(delivered);
        assert_eq!(
            *calls.lock().expect("recording sink mutex"),
            vec![RecordedCall::Notify {
                recipient: "@alice:matrix.example".into(),
                channel: Some("matrix".into()),
                body: "Report".into(),
            }]
        );
    }
}
