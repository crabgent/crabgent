//! Host-facing goal control surface.
//!
//! [`GoalRuntime`] is the generic API a host wires to slash commands like
//! `/goal <objective>`, `/goal`, `/goal pause`, `/goal resume`, and
//! `/goal clear`, plus the turn-continuation mechanism. It is policy-free: the
//! caller (a command crate, an operator) applies any policy before invoking
//! it. Pause, resume, and clear live here (host-controlled) and never on the
//! model tool surface.

use std::sync::Arc;

use crabgent_core::{Message, Owner};
use crabgent_store::{
    GoalStatus, GoalStore, SessionId, StoreError, ThreadGoal, ThreadGoalUpdate,
    validate_goal_budget, validate_goal_objective,
};
use thiserror::Error;

use crate::steering::{SteeringKind, steering_message};

/// Errors surfaced by host goal operations.
#[derive(Debug, Error)]
pub enum GoalError {
    /// The objective or budget failed validation.
    #[error("invalid goal: {0}")]
    Invalid(String),
    /// The backing store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Host control surface over a [`GoalStore`].
#[derive(Clone)]
pub struct GoalRuntime {
    store: Arc<dyn GoalStore>,
}

impl GoalRuntime {
    #[must_use]
    pub fn new(store: Arc<dyn GoalStore>) -> Self {
        Self { store }
    }

    /// Fetch the goal for `session`, if any.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn get(&self, session: &SessionId) -> Result<Option<ThreadGoal>, StoreError> {
        self.store.get_for_session(session).await
    }

    /// Set the thread's objective: create a fresh active goal, or, if one
    /// already exists, replace its objective and re-activate it. `token_budget`
    /// only applies when creating; an existing goal keeps its budget.
    ///
    /// # Errors
    ///
    /// Returns [`GoalError::Invalid`] for an empty/oversize objective or a
    /// non-positive budget, or [`GoalError::Store`] on a backend failure.
    pub async fn set_objective(
        &self,
        owner: &Owner,
        session: &SessionId,
        objective: &str,
        token_budget: Option<i64>,
    ) -> Result<ThreadGoal, GoalError> {
        let objective = validate_goal_objective(objective).map_err(GoalError::Invalid)?;
        validate_goal_budget(token_budget).map_err(GoalError::Invalid)?;
        if let Some(existing) = self.store.get_for_session(session).await? {
            self.store
                .update(
                    &existing.id,
                    &ThreadGoalUpdate {
                        objective: Some(objective),
                        status: Some(GoalStatus::Active),
                    },
                )
                .await?;
            return self
                .store
                .get(&existing.id)
                .await?
                .ok_or(GoalError::Store(StoreError::NotFound));
        }
        let goal = ThreadGoal::new(owner.clone(), session.clone(), objective, token_budget);
        self.store.create(&goal).await?;
        Ok(goal)
    }

    /// Pause the thread's goal. Returns `false` when no goal exists.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn pause(&self, session: &SessionId) -> Result<bool, StoreError> {
        self.set_status(session, GoalStatus::Paused).await
    }

    /// Resume the thread's goal back to `active`. Returns `false` when no goal
    /// exists.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn resume(&self, session: &SessionId) -> Result<bool, StoreError> {
        self.set_status(session, GoalStatus::Active).await
    }

    /// Startup primitive: atomically flip every shutdown-suspended goal
    /// back to `Active` and return them, so the host loop can pump
    /// continuation for each returned session. Host-paused goals
    /// (`GoalStatus::Paused`) are never touched. Idempotent: a second
    /// call returns an empty list.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn resume_suspended(&self) -> Result<Vec<ThreadGoal>, StoreError> {
        self.store.resume_suspended(chrono::Utc::now()).await
    }

    /// Clear (delete) the thread's goal, freeing the session for a new one.
    /// Returns `false` when no goal exists.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn clear(&self, session: &SessionId) -> Result<bool, StoreError> {
        let Some(goal) = self.store.get_for_session(session).await? else {
            return Ok(false);
        };
        self.store.delete(&goal.id).await
    }

    /// Continuation mechanism: when the thread's goal is still `active`, return
    /// the steering input that drives the next autonomous turn. The host
    /// chooses whether to re-pump the kernel with it. Returns `None` for a
    /// paused, blocked, usage-limited, budget-limited, complete, or absent
    /// goal, so those never trigger continuation.
    ///
    /// # Errors
    ///
    /// Propagates store failures.
    pub async fn continuation_input(
        &self,
        session: &SessionId,
    ) -> Result<Option<Vec<Message>>, StoreError> {
        let Some(goal) = self.store.get_for_session(session).await? else {
            return Ok(None);
        };
        if goal.status.is_active() {
            Ok(Some(vec![steering_message(
                &goal,
                SteeringKind::Continuation,
            )]))
        } else {
            Ok(None)
        }
    }

    async fn set_status(
        &self,
        session: &SessionId,
        status: GoalStatus,
    ) -> Result<bool, StoreError> {
        let Some(goal) = self.store.get_for_session(session).await? else {
            return Ok(false);
        };
        self.store
            .update(
                &goal.id,
                &ThreadGoalUpdate {
                    objective: None,
                    status: Some(status),
                },
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_store::MemoryGoalStore;

    fn runtime() -> GoalRuntime {
        GoalRuntime::new(Arc::new(MemoryGoalStore::default()))
    }

    #[tokio::test]
    async fn set_objective_creates_then_replaces() {
        let rt = runtime();
        let owner = Owner::new("alice");
        let session = SessionId::new();
        let created = rt
            .set_objective(&owner, &session, "first", Some(500))
            .await
            .expect("create");
        assert_eq!(created.objective, "first");
        assert_eq!(created.token_budget, Some(500));

        let replaced = rt
            .set_objective(&owner, &session, "second", None)
            .await
            .expect("replace");
        assert_eq!(replaced.objective, "second");
        assert_eq!(replaced.status, GoalStatus::Active);
        // budget is preserved on replace
        assert_eq!(replaced.token_budget, Some(500));
        assert_eq!(replaced.id, created.id);
    }

    #[tokio::test]
    async fn set_objective_rejects_invalid() {
        let rt = runtime();
        let owner = Owner::new("alice");
        let session = SessionId::new();
        assert!(matches!(
            rt.set_objective(&owner, &session, "   ", None).await,
            Err(GoalError::Invalid(_))
        ));
        assert!(matches!(
            rt.set_objective(&owner, &session, "obj", Some(0)).await,
            Err(GoalError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn pause_resume_clear_roundtrip() {
        let rt = runtime();
        let owner = Owner::new("alice");
        let session = SessionId::new();

        assert!(!rt.pause(&session).await.expect("pause missing"));

        rt.set_objective(&owner, &session, "obj", None)
            .await
            .expect("create");
        assert!(rt.pause(&session).await.expect("pause"));
        assert_eq!(
            rt.get(&session).await.expect("get").expect("goal").status,
            GoalStatus::Paused
        );
        assert!(rt.resume(&session).await.expect("resume"));
        assert_eq!(
            rt.get(&session).await.expect("get").expect("goal").status,
            GoalStatus::Active
        );
        assert!(rt.clear(&session).await.expect("clear"));
        assert!(rt.get(&session).await.expect("get").is_none());
    }

    #[tokio::test]
    async fn continuation_only_for_active_goal() {
        let rt = runtime();
        let owner = Owner::new("alice");
        let session = SessionId::new();

        // No goal -> no continuation.
        assert!(
            rt.continuation_input(&session)
                .await
                .expect("none")
                .is_none()
        );

        rt.set_objective(&owner, &session, "obj", None)
            .await
            .expect("create");
        let cont = rt
            .continuation_input(&session)
            .await
            .expect("active continuation")
            .expect("some");
        assert_eq!(cont.len(), 1);

        // Paused -> no continuation.
        rt.pause(&session).await.expect("pause");
        assert!(
            rt.continuation_input(&session)
                .await
                .expect("paused")
                .is_none()
        );
    }

    #[tokio::test]
    async fn continuation_none_for_completed_goal() {
        let rt = runtime();
        let owner = Owner::new("alice");
        let session = SessionId::new();
        let goal = rt
            .set_objective(&owner, &session, "obj", None)
            .await
            .expect("create");
        // Mark complete via the store directly (host could also do this).
        rt.store
            .update(
                &goal.id,
                &ThreadGoalUpdate {
                    objective: None,
                    status: Some(GoalStatus::Complete),
                },
            )
            .await
            .expect("complete");
        assert!(
            rt.continuation_input(&session)
                .await
                .expect("complete")
                .is_none()
        );
    }

    #[tokio::test]
    async fn resume_suspended_flips_only_suspended_goals() {
        let rt = runtime();
        let suspended_session = SessionId::new();
        let paused_session = SessionId::new();
        let owner = Owner::new("alice");
        let suspended = rt
            .set_objective(&owner, &suspended_session, "a", None)
            .await
            .expect("create");
        rt.store
            .update(
                &suspended.id,
                &ThreadGoalUpdate {
                    objective: None,
                    status: Some(GoalStatus::Suspended),
                },
            )
            .await
            .expect("suspend");
        let _ = rt
            .set_objective(&owner, &paused_session, "b", None)
            .await
            .expect("create");
        assert!(rt.pause(&paused_session).await.expect("pause"));

        let resumed = rt.resume_suspended().await.expect("resume suspended");

        assert_eq!(resumed.len(), 1);
        assert_eq!(resumed[0].id, suspended.id);
        let paused_goal = rt
            .store
            .get_for_session(&paused_session)
            .await
            .expect("get")
            .expect("goal");
        assert_eq!(
            paused_goal.status,
            GoalStatus::Paused,
            "host-paused goals never auto-resume"
        );
        assert!(rt.resume_suspended().await.expect("second call").is_empty());
    }
}
