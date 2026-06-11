//! TUI-facing activity bridge for upstream background task and cron observers.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{
    ActivityEventSummary, ActivityTextSummary, JsonValueKind, ToolCallResultActivitySummary,
};
use crabgent_cron::{CronActivityEvent, CronActivityKind, CronError, CronObserver};
use crabgent_task::{TaskActivityEvent, TaskActivityKind, TaskError, TaskObserver};
use tokio::sync::{RwLock, broadcast};

use crate::tui_channel::{TuiDelivery, TuiHub};

const CHANNEL_CAPACITY: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivitySource {
    Task,
    Cron,
    Turn,
}

impl ActivitySource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Cron => "cron",
            Self::Turn => "turn",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    Started,
    Progress,
    Done,
    Failed,
    Cancelled,
    TimedOut,
}

impl ActivityState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Progress => "progress",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityDelivery {
    pub agent: Option<String>,
    pub source: ActivitySource,
    pub id: String,
    pub state: ActivityState,
    pub line: String,
}

#[derive(Debug, Default, Clone)]
pub struct ActivityHub {
    topics: Arc<RwLock<HashMap<String, broadcast::Sender<ActivityDelivery>>>>,
}

impl ActivityHub {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn subscribe(&self, agent: &str) -> broadcast::Receiver<ActivityDelivery> {
        let mut topics = self.topics.write().await;
        topics
            .entry(agent.to_owned())
            .or_insert_with(|| {
                let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
                tx
            })
            .subscribe()
    }

    pub async fn publish_agent(&self, agent: &str, delivery: ActivityDelivery) {
        let tx = {
            let topics = self.topics.read().await;
            topics.get(agent).cloned()
        };
        if let Some(tx) = tx {
            let _ = tx.send(delivery);
        }
    }

    async fn publish_global(&self, delivery: ActivityDelivery) {
        let topics = {
            let topics = self.topics.read().await;
            topics.values().cloned().collect::<Vec<_>>()
        };
        for tx in topics {
            let _ = tx.send(delivery.clone());
        }
    }
}

pub struct TuiTaskObserver {
    agent: String,
    activity_hub: ActivityHub,
    tui_hub: TuiHub,
}

impl TuiTaskObserver {
    #[must_use]
    pub const fn new(agent: String, activity_hub: ActivityHub, tui_hub: TuiHub) -> Self {
        Self {
            agent,
            activity_hub,
            tui_hub,
        }
    }
}

#[async_trait]
impl TaskObserver for TuiTaskObserver {
    async fn observe(&self, event: TaskActivityEvent) -> Result<(), TaskError> {
        if let Some(delivery) = task_delivery(&self.agent, &event) {
            self.activity_hub.publish_agent(&self.agent, delivery).await;
        }
        if let Some(body) = task_terminal_failure_message(&event) {
            let _ = self
                .tui_hub
                .publish(
                    &self.agent,
                    TuiDelivery {
                        from: self.agent.clone(),
                        body,
                    },
                )
                .await;
        }
        Ok(())
    }
}

pub struct TuiCronObserver {
    agent: Option<String>,
    hub: ActivityHub,
}

impl TuiCronObserver {
    #[must_use]
    pub const fn for_agent(agent: String, hub: ActivityHub) -> Self {
        Self {
            agent: Some(agent),
            hub,
        }
    }

    #[must_use]
    pub const fn global(hub: ActivityHub) -> Self {
        Self { agent: None, hub }
    }
}

#[async_trait]
impl CronObserver for TuiCronObserver {
    async fn observe(&self, event: CronActivityEvent) -> Result<(), CronError> {
        if let Some(delivery) = cron_delivery(self.agent.as_deref(), &event) {
            if let Some(agent) = self.agent.as_deref() {
                self.hub.publish_agent(agent, delivery).await;
            } else {
                self.hub.publish_global(delivery).await;
            }
        }
        Ok(())
    }
}

fn task_delivery(agent: &str, event: &TaskActivityEvent) -> Option<ActivityDelivery> {
    let id = event.meta.task_id.to_string();
    let label = task_label(&event.meta).unwrap_or_else(|| short_id(&id));
    let owner = event.meta.owner.as_str();
    let parent = event
        .meta
        .parent_task_id
        .as_ref()
        .map(|p| format!(" · parent {}", short_id(&p.to_string())))
        .unwrap_or_default();
    let (state, detail) = match &event.kind {
        TaskActivityKind::Started => (
            ActivityState::Started,
            format!(
                "started · {owner}{parent} · {}B prompt",
                event.meta.prompt_bytes
            ),
        ),
        TaskActivityKind::Kernel(summary) => {
            let detail = kernel_detail(summary)?;
            (ActivityState::Progress, detail)
        }
        TaskActivityKind::Completed => (
            ActivityState::Done,
            terminal_detail("done", event.meta.duration),
        ),
        TaskActivityKind::Failed { error } => (
            ActivityState::Failed,
            format!("failed · {}", summary_bytes(error)),
        ),
        TaskActivityKind::Cancelled => (ActivityState::Cancelled, "cancelled".to_owned()),
        TaskActivityKind::TimedOut => (ActivityState::TimedOut, "timed out".to_owned()),
        _ => return None,
    };
    Some(ActivityDelivery {
        agent: Some(agent.to_owned()),
        source: ActivitySource::Task,
        id,
        state,
        line: format!("bg {label} · {detail}"),
    })
}

fn task_label(meta: &crabgent_task::TaskActivityMeta) -> Option<String> {
    meta.name
        .as_ref()
        .and_then(|name| name.preview.as_deref())
        .or(meta.prompt.preview.as_deref())
        .map(first_activity_line)
        .filter(|label| !label.is_empty())
        .map(|label| truncate_activity_label(&label))
}

fn task_terminal_failure_message(event: &TaskActivityEvent) -> Option<String> {
    let label =
        task_label(&event.meta).unwrap_or_else(|| short_id(&event.meta.task_id.to_string()));
    match &event.kind {
        TaskActivityKind::Failed { error } => Some(format!(
            "Task \"{label}\" fehlgeschlagen: {}",
            summary_text(error)
        )),
        TaskActivityKind::TimedOut => Some(format!("Task \"{label}\" ist getimed out.")),
        _ => None,
    }
}

fn cron_delivery(agent: Option<&str>, event: &CronActivityEvent) -> Option<ActivityDelivery> {
    let id = event
        .job
        .as_ref()
        .map_or_else(|| "scheduler".to_owned(), |job| job.job_id.to_string());
    let label = event.job.as_ref().map_or_else(
        || "scheduler".to_owned(),
        |job| {
            job.name.preview.as_deref().map_or_else(
                || short_id(&job.job_id.to_string()),
                truncate_activity_label,
            )
        },
    );
    let (state, detail) = match &event.kind {
        CronActivityKind::ClaimedBatch { count, claim_limit } => {
            if *count == 0 {
                return None;
            }
            (
                ActivityState::Progress,
                format!("claimed {count}/{claim_limit} due jobs"),
            )
        }
        CronActivityKind::ClaimFailed { error } => (
            ActivityState::Failed,
            format!("claim failed · {}", summary_bytes(error)),
        ),
        CronActivityKind::ConcurrencyLimit { max_concurrent } => (
            ActivityState::Progress,
            format!("concurrency limit · max {max_concurrent}"),
        ),
        CronActivityKind::ClaimReleased => (ActivityState::Progress, "claim released".to_owned()),
        CronActivityKind::ClaimReleaseFailed { error } => (
            ActivityState::Failed,
            format!("claim release failed · {}", summary_bytes(error)),
        ),
        CronActivityKind::Started => (ActivityState::Started, "started".to_owned()),
        CronActivityKind::PreProcessorSkipped => {
            (ActivityState::Progress, "pre-processor skipped".to_owned())
        }
        CronActivityKind::PreProcessorDelivered { text } => (
            ActivityState::Progress,
            format!("pre-processor delivered · {}", summary_bytes(text)),
        ),
        CronActivityKind::PreProcessorRunLlm { prompt } => (
            ActivityState::Progress,
            format!("pre-processor changed prompt · {}", summary_bytes(prompt)),
        ),
        CronActivityKind::PreProcessorPassthrough => {
            (ActivityState::Progress, "pre-processor pass".to_owned())
        }
        CronActivityKind::Kernel(summary) => {
            let detail = kernel_detail(summary)?;
            (ActivityState::Progress, detail)
        }
        CronActivityKind::Completed => (ActivityState::Done, "done".to_owned()),
        CronActivityKind::Failed { error } => (
            ActivityState::Failed,
            format!("failed · {}", summary_bytes(error)),
        ),
        CronActivityKind::Cancelled => (ActivityState::Cancelled, "cancelled".to_owned()),
        CronActivityKind::TimedOut => (ActivityState::TimedOut, "timed out".to_owned()),
        CronActivityKind::ScheduleAdvanced { next_run, disabled } => {
            let state = if *disabled { "disabled" } else { "next" };
            (
                ActivityState::Progress,
                format!("{state} · {}", next_run.format("%Y-%m-%d %H:%M:%S UTC")),
            )
        }
        CronActivityKind::ScheduleAdvanceFailed { error } => (
            ActivityState::Failed,
            format!("schedule advance failed · {}", summary_bytes(error)),
        ),
        _ => return None,
    };
    Some(ActivityDelivery {
        agent: agent.map(str::to_owned),
        source: ActivitySource::Cron,
        id,
        state,
        line: format!("cron {label} · {detail}"),
    })
}

fn kernel_detail(summary: &ActivityEventSummary) -> Option<String> {
    // Streaming text deltas arrive in very small chunks and flood the TUI.
    // Tool calls, compact final output and terminal state still show useful
    // progress without turning background activity into a token log.
    if matches!(
        summary,
        ActivityEventSummary::ReasoningDelta(_) | ActivityEventSummary::OutputDelta(_)
    ) {
        return None;
    }

    match summary {
        ActivityEventSummary::ToolCallStarted(call) => Some(format!(
            "tool {} · args {}",
            call.tool_name,
            json_shape(&call.args)
        )),
        ActivityEventSummary::ToolCallCompleted(result) => Some(tool_completed_detail(result)),
        ActivityEventSummary::Notification(note) => Some(format!(
            "notification {} {:?} · {}",
            note.kind,
            note.level,
            summary_bytes(&note.message)
        )),
        ActivityEventSummary::ServerToolResult(result) => Some(format!(
            "server tool {}:{} · {} citations · {}",
            result.provider,
            result.name,
            result.citation_count,
            json_shape(&result.raw)
        )),
        ActivityEventSummary::AttemptFailed(attempt) => Some(format!(
            "attempt {}/{} failed · {} {} · fallback {}",
            attempt.attempt_idx.saturating_add(1),
            attempt.total_attempts,
            attempt.provider,
            attempt.model,
            attempt.will_fallback
        )),
        ActivityEventSummary::Final(text) => text.preview.as_deref().map(|preview| {
            format!(
                "final {}B · {}",
                text.bytes,
                truncate_activity_label(preview)
            )
        }),
        _ => None,
    }
}

fn tool_completed_detail(result: &ToolCallResultActivitySummary) -> String {
    let status = if result.is_error { "error" } else { "ok" };
    let mut parts = vec![
        format!("tool {} {status}", result.tool_name),
        json_shape(&result.output),
    ];
    if result.channel_outbound_count > 0 {
        parts.push(format!("{} outbound", result.channel_outbound_count));
    }
    if result.run_message_count > 0 {
        parts.push(format!("{} run messages", result.run_message_count));
    }
    parts.join(" · ")
}

fn json_shape(shape: &crabgent_core::JsonShapeSummary) -> String {
    let kind = match shape.kind {
        JsonValueKind::Null => "null",
        JsonValueKind::Bool => "bool",
        JsonValueKind::Number => "number",
        JsonValueKind::String => "string",
        JsonValueKind::Array => "array",
        JsonValueKind::Object => "object",
        _ => "value",
    };
    let mut out = format!("{kind} {}B", shape.bytes);
    if let Some(len) = shape.array_len {
        let _ = write!(out, " len {len}");
    }
    if let Some(len) = shape.object_len {
        let _ = write!(out, " keys {len}");
    }
    out
}

fn summary_bytes(summary: &ActivityTextSummary) -> String {
    summary.preview.as_ref().map_or_else(
        || format!("{}B redacted", summary.bytes),
        |preview| format!("{}B · {}", summary.bytes, truncate_activity_label(preview)),
    )
}

fn summary_text(summary: &ActivityTextSummary) -> String {
    summary.preview.as_deref().map_or_else(
        || format!("{}B redacted", summary.bytes),
        truncate_activity_label,
    )
}

fn terminal_detail(label: &str, duration: Option<std::time::Duration>) -> String {
    duration.map_or_else(
        || label.to_owned(),
        |duration| format!("{label} · {}", human_duration(duration)),
    )
}

fn human_duration(duration: std::time::Duration) -> String {
    let secs = duration.as_secs();
    if secs < 60 {
        return format!("{secs}s");
    }
    format!("{}m{:02}s", secs / 60, secs % 60)
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn truncate_activity_label(value: &str) -> String {
    const MAX_CHARS: usize = 80;
    if value.chars().count() <= MAX_CHARS {
        return value.to_owned();
    }
    let mut out: String = value.chars().take(MAX_CHARS).collect();
    out.push_str("...");
    out
}

fn first_activity_line(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use crabgent_core::{ActivityEventSummary, ActivityTextSummary, RunId};
    use crabgent_cron::CronJobActivityMeta;
    use crabgent_store::records::TaskStatus;
    use crabgent_store::{CronJobId, Owner, TaskId};
    use crabgent_task::{TaskActivityEvent, TaskActivityKind, TaskActivityMeta};

    fn task_event(kind: TaskActivityKind) -> TaskActivityEvent {
        TaskActivityEvent {
            meta: TaskActivityMeta {
                task_id: TaskId::new(),
                owner: Owner::new("tui:local"),
                name: Some(ActivityTextSummary::with_preview("demo task")),
                prompt: ActivityTextSummary::with_preview("do demo work"),
                status: TaskStatus::Running,
                run_id: RunId::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                finished_at: None,
                duration: None,
                parent_session_id: None,
                parent_task_id: None,
                context_mode: None,
                prompt_bytes: 42,
                reasoning_effort: None,
            },
            kind,
        }
    }

    #[test]
    fn task_started_delivery_is_short_and_stateful() {
        let delivery =
            task_delivery("local", &task_event(TaskActivityKind::Started)).expect("delivery");

        assert_eq!(delivery.agent.as_deref(), Some("local"));
        assert_eq!(delivery.source, ActivitySource::Task);
        assert_eq!(delivery.state, ActivityState::Started);
        assert!(delivery.line.contains("started"));
        assert!(delivery.line.starts_with("bg demo task"));
    }

    #[test]
    fn output_delta_is_suppressed() {
        let event = task_event(TaskActivityKind::Kernel(ActivityEventSummary::OutputDelta(
            ActivityTextSummary::with_preview("hello world"),
        )));

        assert!(task_delivery("local", &event).is_none());
    }

    #[test]
    fn reasoning_delta_is_suppressed() {
        let event = task_event(TaskActivityKind::Kernel(
            ActivityEventSummary::ReasoningDelta(ActivityTextSummary::redacted("abc")),
        ));

        assert!(task_delivery("local", &event).is_none());
    }

    #[test]
    fn timed_out_task_builds_user_visible_failure_message() {
        let event = task_event(TaskActivityKind::TimedOut);

        let message = task_terminal_failure_message(&event).expect("terminal message");

        assert_eq!(message, "Task \"demo task\" ist getimed out.");
    }

    #[test]
    fn cron_started_delivery_uses_job_name() {
        let now = Utc::now();
        let event = CronActivityEvent::new(
            Some(CronJobActivityMeta {
                job_id: CronJobId::new(),
                name: ActivityTextSummary::with_preview("daily backup"),
                owner: Some(Owner::new("tui:local")),
                run_id: None,
                prompt: ActivityTextSummary::redacted("do backup"),
                created_at: now,
                claimed_at: Some(now),
                last_run: None,
                next_run: now,
            }),
            CronActivityKind::Started,
        );

        let delivery = cron_delivery(Some("local"), &event).expect("delivery");

        assert_eq!(delivery.agent.as_deref(), Some("local"));
        assert_eq!(delivery.source, ActivitySource::Cron);
        assert_eq!(delivery.state, ActivityState::Started);
        assert!(delivery.line.starts_with("cron daily backup"));
    }
}
