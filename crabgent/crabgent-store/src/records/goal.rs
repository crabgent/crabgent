//! Thread-goal records: an explicit user-defined objective a model keeps
//! working toward across turns. See [`crate::traits::GoalStore`].

use std::str::FromStr;

use chrono::{DateTime, Utc};
use crabgent_core::Owner;
use serde::{Deserialize, Serialize};

use crate::ids::{GoalId, SessionId};

/// Maximum length (in characters) of a thread-goal objective.
pub const MAX_GOAL_OBJECTIVE_CHARS: usize = 4_000;

/// Lifecycle state of a [`ThreadGoal`].
///
/// `Active` is the only status that drives runtime continuation. `Complete`
/// and `BudgetLimited` are terminal: an active goal that reaches its token
/// budget flips to `BudgetLimited` (host/runtime controlled). `Paused`,
/// `Suspended`, `Blocked`, and `UsageLimited` are non-active but not
/// terminal: a host can resume them back to `Active`. The model may only
/// ever drive a goal to `Complete` or `Blocked`; every other transition is
/// host-initiated.
///
/// `Paused` and `Suspended` split pause intent: `Paused` is host/user
/// initiated (an operator or a user-cancelled turn paused the goal) and is
/// never resumed automatically. `Suspended` is system-paused (process
/// shutdown or crash interrupted the goal's work) and is the only status
/// `GoalStore::resume_suspended` flips back to `Active` at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GoalStatus {
    Active,
    Paused,
    Suspended,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

impl GoalStatus {
    /// Stable lowercase label used for persistence and steering rendering.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Suspended => "suspended",
            Self::Blocked => "blocked",
            Self::UsageLimited => "usage_limited",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
        }
    }

    /// True only for [`GoalStatus::Active`]: the status that drives
    /// runtime continuation.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }

    /// True for terminal statuses that no host action resumes
    /// ([`GoalStatus::Complete`], [`GoalStatus::BudgetLimited`]).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::BudgetLimited)
    }
}

/// Error returned when an unknown goal status string is parsed.
#[derive(Debug, thiserror::Error)]
#[error("unknown goal status: {0}")]
pub struct ParseGoalStatusError(pub String);

impl FromStr for GoalStatus {
    type Err = ParseGoalStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "suspended" => Ok(Self::Suspended),
            "blocked" => Ok(Self::Blocked),
            "usage_limited" => Ok(Self::UsageLimited),
            "budget_limited" => Ok(Self::BudgetLimited),
            "complete" => Ok(Self::Complete),
            other => Err(ParseGoalStatusError(other.to_owned())),
        }
    }
}

/// A persistent per-thread goal: an explicit user-defined objective a model
/// keeps working toward across turns. A goal is a singleton per
/// [`SessionId`]; the store rejects a second goal for a session with
/// [`StoreError::Conflict`](crate::error::StoreError::Conflict).
///
/// `tokens_used` and `time_used_seconds` accumulate across every goal turn.
/// When `token_budget` is set and `tokens_used` reaches it, the status flips
/// to [`GoalStatus::BudgetLimited`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadGoal {
    pub id: GoalId,
    pub owner: Owner,
    pub session: SessionId,
    pub objective: String,
    pub status: GoalStatus,
    pub token_budget: Option<i64>,
    pub tokens_used: i64,
    pub time_used_seconds: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ThreadGoal {
    /// Build a fresh `Active` goal for `session`, owned by `owner`.
    #[must_use]
    pub fn new(
        owner: Owner,
        session: SessionId,
        objective: impl Into<String>,
        token_budget: Option<i64>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: GoalId::new(),
            owner,
            session,
            objective: objective.into(),
            status: GoalStatus::Active,
            token_budget,
            tokens_used: 0,
            time_used_seconds: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// Remaining token budget (`token_budget - tokens_used`, floored at 0),
    /// or `None` when the goal carries no budget.
    #[must_use]
    pub fn remaining_tokens(&self) -> Option<i64> {
        self.token_budget.map(|b| (b - self.tokens_used).max(0))
    }
}

/// Partial update applied to a [`ThreadGoal`]. Only `Some` fields are written.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadGoalUpdate {
    pub objective: Option<String>,
    pub status: Option<GoalStatus>,
}

impl ThreadGoalUpdate {
    /// Apply the present fields to `goal` and bump `updated_at`.
    pub fn apply_to(&self, goal: &mut ThreadGoal, updated_at: DateTime<Utc>) {
        if let Some(objective) = &self.objective {
            goal.objective.clone_from(objective);
        }
        if let Some(status) = self.status {
            goal.status = status;
        }
        goal.updated_at = updated_at;
    }
}

/// Validate a goal objective: trimmed, non-empty, within the length cap.
/// Returns the trimmed objective on success.
///
/// # Errors
///
/// Returns an error message string when the objective is empty or too long.
pub fn validate_goal_objective(objective: &str) -> Result<String, String> {
    let trimmed = objective.trim();
    if trimmed.is_empty() {
        return Err("goal objective must not be empty".to_owned());
    }
    if trimmed.chars().count() > MAX_GOAL_OBJECTIVE_CHARS {
        return Err(format!(
            "goal objective must be at most {MAX_GOAL_OBJECTIVE_CHARS} characters"
        ));
    }
    Ok(trimmed.to_owned())
}

/// Validate an optional token budget: positive when provided.
///
/// # Errors
///
/// Returns an error message string when a non-positive budget is given.
pub fn validate_goal_budget(token_budget: Option<i64>) -> Result<(), String> {
    if let Some(budget) = token_budget
        && budget <= 0
    {
        return Err("goal token_budget must be positive when provided".to_owned());
    }
    Ok(())
}
