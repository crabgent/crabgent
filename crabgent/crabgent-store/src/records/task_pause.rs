//! Pause/resume records for [`Task`](super::Task): the pause cause and the
//! persisted spec a [`crate::traits::TaskStore`] consumer needs to rebuild
//! the run after a restart. See `TaskExecutor::resume_paused` in
//! `crabgent-task` for the consuming side.

use std::collections::HashMap;
use std::str::FromStr;

use crabgent_core::{ReasoningEffort, ToolAccess};
use serde::{Deserialize, Serialize};

use super::ModelTargetDto;

/// Why a task left `Running` for `Paused`.
///
/// `Shutdown` is the cooperative path: the run exited `Outcome::Paused` at a
/// safe boundary. `Forced` is the executor's force-pause after the pause
/// grace elapsed (the run was cancelled mid-flight; the transcript tail may
/// hold dangling tool calls). `Crash` is startup orphan adoption: the
/// previous process died without writing anything, and the stale `Running`
/// row was folded into `Paused` at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskPauseCause {
    Shutdown,
    Forced,
    Crash,
}

impl TaskPauseCause {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::Forced => "forced",
            Self::Crash => "crash",
        }
    }
}

/// Error returned when an unknown pause-cause string is parsed.
#[derive(Debug, thiserror::Error)]
#[error("unknown task pause cause: {0}")]
pub struct ParseTaskPauseCauseError(pub String);

impl FromStr for TaskPauseCause {
    type Err = ParseTaskPauseCauseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "shutdown" => Ok(Self::Shutdown),
            "forced" => Ok(Self::Forced),
            "crash" => Ok(Self::Crash),
            other => Err(ParseTaskPauseCauseError(other.to_owned())),
        }
    }
}

/// Run parameters persisted at spawn time so a paused task can be respawned
/// after a restart. Covers the `TaskRequest` fields that are not already on
/// the [`Task`](super::Task) record; the conversation transcript is stored
/// separately via `TaskStore::save_transcript`.
///
/// `subject_id`/`subject_attrs` flatten the kernel `Subject` (its fields are
/// private and its non-empty-id invariant is re-checked via
/// `Subject::try_new` on rebuild).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResumeSpec {
    pub subject_id: String,
    #[serde(default)]
    pub subject_attrs: HashMap<String, String>,
    pub model: ModelTargetDto,
    #[serde(default)]
    pub explicit_model: Option<ModelTargetDto>,
    #[serde(default)]
    pub session_model_override: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub tool_access: ToolAccess,
}
