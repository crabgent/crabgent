//! Persistence records used across [`crate::traits`] implementations.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use crabgent_core::{
    MemoryId, MemoryScope, Message, ModelTarget, Owner, ReasoningEffort, ThreadId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ids::{ArchiveId, CronJobId, RelationId, SessionId, TaskId};
use crate::relation_type::RelationType;

/// A persisted conversation session: a series of messages plus metadata
/// scoped to an [`Owner`] (and optionally one [`ThreadId`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub owner: Owner,
    #[serde(default)]
    pub scope: MemoryScope,
    pub thread: Option<ThreadId>,
    pub title: Option<String>,
    pub summary: Option<String>,
    #[serde(default)]
    pub compaction_summary: Option<String>,
    pub model_override: Option<String>,
    #[serde(default)]
    pub reasoning_effort_override: Option<ReasoningEffort>,
    pub messages: Vec<Message>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Session {
    #[must_use]
    pub fn with_scope(mut self, scope: MemoryScope) -> Self {
        self.scope = scope;
        self
    }
}

/// Lossless archive of a compacted session message slice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionArchiveEntry {
    pub id: ArchiveId,
    pub session_id: SessionId,
    pub messages: Vec<Message>,
    pub created_at: DateTime<Utc>,
}

/// Lightweight projection for session listings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub owner: Owner,
    pub thread: Option<ThreadId>,
    pub title: Option<String>,
    pub message_count: usize,
    pub has_summary: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&Session> for SessionInfo {
    fn from(s: &Session) -> Self {
        Self {
            id: s.id.clone(),
            owner: s.owner.clone(),
            thread: s.thread.clone(),
            title: s.title.clone(),
            message_count: s.messages.len(),
            has_summary: s.summary.is_some(),
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

/// Lifecycle state of a persisted task.
///
/// `Paused` is non-terminal: the task was interrupted at (or folded into) a
/// resumable point and is eligible for `TaskStore::claim_for_resume`. `Done`
/// and `Failed` stay the only terminal states accepted by
/// `TaskStore::finish`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Running,
    Paused,
    Done,
    Failed,
}

impl TaskStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

/// Error returned when an unknown status string is parsed.
#[derive(Debug, thiserror::Error)]
#[error("unknown task status: {0}")]
pub struct ParseTaskStatusError(pub String);

impl FromStr for TaskStatus {
    type Err = ParseTaskStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "running" => Ok(Self::Running),
            "paused" => Ok(Self::Paused),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            other => Err(ParseTaskStatusError(other.to_owned())),
        }
    }
}

/// A persisted background task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub owner: Owner,
    #[serde(default)]
    pub name: Option<String>,
    pub prompt: String,
    pub status: TaskStatus,
    pub output: String,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub parent_session_id: Option<SessionId>,
    pub parent_task_id: Option<TaskId>,
    /// Opaque hint to the executor about how to assemble context (`fresh`,
    /// `recent_thread`, `summary`, custom string). Not interpreted by the store.
    pub context_mode: Option<String>,
    #[serde(default)]
    pub reasoning_effort_override: Option<ReasoningEffort>,
    /// Run parameters captured at spawn time for restart-resume. `None` on
    /// rows written before pause support existed; such orphans are not
    /// resumable and are finished `Failed` by the resume scan.
    #[serde(default)]
    pub resume_spec: Option<TaskResumeSpec>,
    /// How often this task has been claimed for resume. The claim CAS
    /// enforces a cap so a poison task cannot crash-loop the host across
    /// restarts.
    #[serde(default)]
    pub resume_count: u32,
    /// Why the task is `Paused`. `None` while not paused.
    #[serde(default)]
    pub pause_cause: Option<TaskPauseCause>,
    /// When the task last entered `Paused`. `None` while not paused.
    #[serde(default)]
    pub paused_at: Option<DateTime<Utc>>,
}

/// Schedule for a cron job. Either an interval or a 5-field cron expression
/// (mutually exclusive at construction).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronSchedule {
    pub interval_secs: Option<u64>,
    pub cron_expr: Option<String>,
    pub cron_tz: Option<String>,
}

impl CronSchedule {
    /// Schedule by fixed interval in seconds.
    #[must_use]
    pub const fn every(secs: u64) -> Self {
        Self {
            interval_secs: Some(secs),
            cron_expr: None,
            cron_tz: None,
        }
    }

    /// Schedule by a 5-field cron expression (with optional IANA timezone).
    pub fn cron(expr: impl Into<String>, tz: Option<String>) -> Self {
        Self {
            interval_secs: None,
            cron_expr: Some(expr.into()),
            cron_tz: tz,
        }
    }
}

/// A persisted cron job. `delivery_ctx` carries channel-specific data
/// (Slack channel id, Matrix room id, ...) the store does not interpret.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: CronJobId,
    pub name: String,
    pub scope: MemoryScope,
    pub prompt: String,
    pub schedule: CronSchedule,
    pub enabled: bool,
    pub run_once: bool,
    pub model_override: Option<ModelTargetDto>,
    #[serde(default)]
    pub reasoning_effort_override: Option<ReasoningEffort>,
    pub pre_command: Option<String>,
    pub delivery_ctx: Value,
    pub last_run: Option<DateTime<Utc>>,
    pub next_run: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
}

/// Partial update applied to a [`CronJob`]. Only `Some` fields are written.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CronJobUpdate {
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub schedule: Option<CronSchedule>,
    pub enabled: Option<bool>,
    pub run_once: Option<bool>,
    pub model_override: Option<Option<ModelTargetDto>>,
    pub reasoning_effort_override: Option<Option<ReasoningEffort>>,
    pub pre_command: Option<Option<String>>,
    pub delivery_ctx: Option<Value>,
    pub next_run: Option<DateTime<Utc>>,
}

impl CronJobUpdate {
    pub fn apply_to(&self, job: &mut CronJob) {
        if let Some(name) = &self.name {
            job.name.clone_from(name);
        }
        if let Some(prompt) = &self.prompt {
            job.prompt.clone_from(prompt);
        }
        if let Some(schedule) = &self.schedule {
            job.schedule.clone_from(schedule);
        }
        if let Some(enabled) = self.enabled {
            job.enabled = enabled;
        }
        if let Some(run_once) = self.run_once {
            job.run_once = run_once;
        }
        if let Some(model) = &self.model_override {
            job.model_override.clone_from(model);
        }
        if let Some(effort) = &self.reasoning_effort_override {
            job.reasoning_effort_override.clone_from(effort);
        }
        if let Some(pre_command) = &self.pre_command {
            job.pre_command.clone_from(pre_command);
        }
        if let Some(delivery_ctx) = &self.delivery_ctx {
            job.delivery_ctx.clone_from(delivery_ctx);
        }
        if let Some(next_run) = self.next_run {
            job.next_run = next_run;
        }
    }
}

/// Serializable cron model selector. Supports old plain strings and
/// provider-qualified model targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelTargetDto {
    Id(String),
    Provider { provider: String, id: String },
}

impl From<ModelTargetDto> for ModelTarget {
    fn from(value: ModelTargetDto) -> Self {
        match value {
            ModelTargetDto::Id(id) => Self::id(id),
            ModelTargetDto::Provider { provider, id } => Self::new(provider, id),
        }
    }
}

impl From<ModelTarget> for ModelTargetDto {
    fn from(value: ModelTarget) -> Self {
        match value {
            ModelTarget::Id(id) => Self::Id(id.to_string()),
            ModelTarget::Provider { provider, model } => Self::Provider {
                provider,
                id: model.to_string(),
            },
        }
    }
}

pub fn serialize_model_target_dto(dto: &ModelTargetDto) -> String {
    match dto {
        ModelTargetDto::Id(id) => Value::String(id.clone()).to_string(),
        ModelTargetDto::Provider { provider, id } => {
            json!({ "provider": provider, "id": id }).to_string()
        }
    }
}

/// Migration helper: parse a stored model target string.
///
/// Plain `"<provider>/<id>"` form maps to the provider variant.
/// JSON-tagged form `{"provider": "...", "id": "..."}` maps to the provider variant.
/// Plain strings without `/` map to the id variant as a legacy fallback.
/// Used by `SqliteCronStore` row reads to preserve legacy stored values.
pub fn deserialize_model_target_dto(raw: &str) -> ModelTargetDto {
    match serde_json::from_str::<ModelTargetDto>(raw) {
        Ok(dto) => normalize_model_target_dto(dto),
        Err(_) => legacy_model_target_dto(raw),
    }
}

fn normalize_model_target_dto(dto: ModelTargetDto) -> ModelTargetDto {
    match dto {
        ModelTargetDto::Id(id) => legacy_model_target_dto(&id),
        provider @ ModelTargetDto::Provider { .. } => provider,
    }
}

fn legacy_model_target_dto(raw: &str) -> ModelTargetDto {
    raw.split_once('/').map_or_else(
        || ModelTargetDto::Id(raw.to_owned()),
        |(provider, id)| {
            if provider.is_empty() || id.is_empty() {
                ModelTargetDto::Id(raw.to_owned())
            } else {
                ModelTargetDto::Provider {
                    provider: provider.to_owned(),
                    id: id.to_owned(),
                }
            }
        },
    )
}

/// Persisted entry in the tool-output cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCacheEntry {
    pub id: String,
    pub session_id: SessionId,
    pub tool_name: String,
    pub content: String,
    pub preview: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// A persisted memory document: a long-term fact with a scoping vector
/// and a free-text body. Search returns [`MemoryHit`]; `get` returns
/// the full document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryDoc {
    pub id: MemoryId,
    pub scope: MemoryScope,
    pub body: String,
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub importance: Option<f32>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MemoryDoc {
    /// Build a fresh memory document with caller-provided scope and body.
    #[must_use]
    pub fn new(scope: MemoryScope, body: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: MemoryId::new(),
            scope,
            body: body.into(),
            class: None,
            importance: None,
            expires_at: None,
            archived_at: None,
            embedding: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// A directed edge between two [`MemoryDoc`]s in the relation graph.
///
/// The `from_id` and `to_id` documents may belong to different owners
/// (cross-owner relations are allowed); `scope` is the writer's, so read
/// visibility and delete authority follow the edge owner, not the linked
/// documents. `relation_type` is an open [`RelationType`] label.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRelation {
    pub id: RelationId,
    pub from_id: MemoryId,
    pub to_id: MemoryId,
    pub relation_type: RelationType,
    pub scope: MemoryScope,
    pub created_at: DateTime<Utc>,
}

impl MemoryRelation {
    /// Build a fresh relation edge with a generated [`RelationId`].
    #[must_use]
    pub fn new(
        from_id: MemoryId,
        to_id: MemoryId,
        relation_type: RelationType,
        scope: MemoryScope,
    ) -> Self {
        Self {
            id: RelationId::new(),
            from_id,
            to_id,
            relation_type,
            scope,
            created_at: Utc::now(),
        }
    }
}

/// One ranked hit from a memory search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    pub id: MemoryId,
    pub body: String,
    pub score: f32,
    #[serde(default)]
    pub cosine_similarity: Option<f32>,
    pub created_at: DateTime<Utc>,
}

/// One ranked hit from a session search. Carries an excerpt rather
/// than the full message log so the LLM can decide whether to fetch
/// the full session via a follow-up tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSearchHit {
    pub session_id: SessionId,
    pub excerpt: String,
    pub score: f64,
    pub occurred_at: DateTime<Utc>,
}

mod goal;
mod task_pause;
pub use goal::{
    GoalStatus, MAX_GOAL_OBJECTIVE_CHARS, ParseGoalStatusError, ThreadGoal, ThreadGoalUpdate,
    validate_goal_budget, validate_goal_objective,
};
pub use task_pause::{ParseTaskPauseCauseError, TaskPauseCause, TaskResumeSpec};

#[cfg(test)]
mod tests;
