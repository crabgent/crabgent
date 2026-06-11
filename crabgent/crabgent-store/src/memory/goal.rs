//! In-memory [`GoalStore`] backed by a `HashMap` behind a `Mutex`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::StoreError;
use crate::ids::{GoalId, SessionId};
use crate::page::Page;
use crate::records::{GoalStatus, ThreadGoal, ThreadGoalUpdate};
use crate::traits::GoalStore;

#[derive(Default)]
pub struct MemoryGoalStore {
    inner: Mutex<HashMap<GoalId, ThreadGoal>>,
}

impl MemoryGoalStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<GoalId, ThreadGoal>>, StoreError> {
        self.inner
            .lock()
            .map_err(|e| StoreError::backend(format!("goal mutex poisoned: {e}")))
    }
}

/// Flip `Active` to `BudgetLimited` when a positive budget is met or exceeded.
fn apply_budget_limit(goal: &mut ThreadGoal) {
    if goal.status == GoalStatus::Active
        && let Some(budget) = goal.token_budget
        && budget > 0
        && goal.tokens_used >= budget
    {
        goal.status = GoalStatus::BudgetLimited;
    }
}

#[async_trait]
impl GoalStore for MemoryGoalStore {
    async fn create(&self, goal: &ThreadGoal) -> Result<(), StoreError> {
        let mut goals = self.lock()?;
        if goals.contains_key(&goal.id) {
            return Err(StoreError::Conflict(format!(
                "goal already exists: {}",
                goal.id
            )));
        }
        if goals.values().any(|g| g.session == goal.session) {
            return Err(StoreError::Conflict(format!(
                "session already has a goal: {}",
                goal.session
            )));
        }
        goals.insert(goal.id.clone(), goal.clone());
        Ok(())
    }

    async fn get(&self, id: &GoalId) -> Result<Option<ThreadGoal>, StoreError> {
        let goals = self.lock()?;
        Ok(goals.get(id).cloned())
    }

    async fn get_for_session(&self, session: &SessionId) -> Result<Option<ThreadGoal>, StoreError> {
        let goals = self.lock()?;
        Ok(goals.values().find(|g| &g.session == session).cloned())
    }

    async fn update(&self, id: &GoalId, update: &ThreadGoalUpdate) -> Result<bool, StoreError> {
        let mut goals = self.lock()?;
        if let Some(goal) = goals.get_mut(id) {
            update.apply_to(goal, Utc::now());
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn account_usage(
        &self,
        id: &GoalId,
        token_delta: i64,
        time_delta_seconds: i64,
        at: DateTime<Utc>,
    ) -> Result<Option<ThreadGoal>, StoreError> {
        let mut goals = self.lock()?;
        let Some(goal) = goals.get_mut(id) else {
            return Ok(None);
        };
        goal.tokens_used = goal.tokens_used.saturating_add(token_delta.max(0));
        goal.time_used_seconds = goal
            .time_used_seconds
            .saturating_add(time_delta_seconds.max(0));
        goal.updated_at = at;
        apply_budget_limit(goal);
        Ok(Some(goal.clone()))
    }

    async fn delete(&self, id: &GoalId) -> Result<bool, StoreError> {
        let mut goals = self.lock()?;
        Ok(goals.remove(id).is_some())
    }

    async fn list_by_status(
        &self,
        status: GoalStatus,
        page: Page,
    ) -> Result<Vec<ThreadGoal>, StoreError> {
        let goals = self.lock()?;
        let mut matching: Vec<&ThreadGoal> =
            goals.values().filter(|g| g.status == status).collect();
        matching.sort_by_key(|goal| std::cmp::Reverse(goal.updated_at));
        Ok(matching
            .into_iter()
            .skip(page.offset)
            .take(page.limit)
            .cloned()
            .collect())
    }

    async fn resume_suspended(&self, at: DateTime<Utc>) -> Result<Vec<ThreadGoal>, StoreError> {
        let mut goals = self.lock()?;
        let mut resumed = Vec::new();
        for goal in goals.values_mut() {
            if goal.status == GoalStatus::Suspended {
                goal.status = GoalStatus::Active;
                goal.updated_at = at;
                resumed.push(goal.clone());
            }
        }
        Ok(resumed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::Owner;

    fn goal(session: &SessionId, budget: Option<i64>) -> ThreadGoal {
        ThreadGoal::new(Owner::new("u"), session.clone(), "ship the feature", budget)
    }

    #[tokio::test]
    async fn create_get_roundtrip() {
        let store = MemoryGoalStore::default();
        let session = SessionId::new();
        let g = goal(&session, None);
        store.create(&g).await.expect("create");
        let got = store.get(&g.id).await.expect("get").expect("present");
        assert_eq!(got.objective, "ship the feature");
        assert_eq!(got.status, GoalStatus::Active);
        let by_session = store
            .get_for_session(&session)
            .await
            .expect("get_for_session")
            .expect("present");
        assert_eq!(by_session.id, g.id);
    }

    #[tokio::test]
    async fn second_goal_for_session_conflicts() {
        let store = MemoryGoalStore::default();
        let session = SessionId::new();
        store.create(&goal(&session, None)).await.expect("first");
        let err = store
            .create(&goal(&session, None))
            .await
            .expect_err("second conflicts");
        assert!(matches!(err, StoreError::Conflict(_)));
    }

    #[tokio::test]
    async fn distinct_sessions_each_get_a_goal() {
        let store = MemoryGoalStore::default();
        let a = SessionId::new();
        let b = SessionId::new();
        store.create(&goal(&a, None)).await.expect("a");
        store.create(&goal(&b, None)).await.expect("b");
        assert!(store.get_for_session(&a).await.expect("a").is_some());
        assert!(store.get_for_session(&b).await.expect("b").is_some());
    }

    #[tokio::test]
    async fn update_sets_status_and_objective() {
        let store = MemoryGoalStore::default();
        let session = SessionId::new();
        let g = goal(&session, None);
        store.create(&g).await.expect("create");
        let applied = store
            .update(
                &g.id,
                &ThreadGoalUpdate {
                    status: Some(GoalStatus::Complete),
                    objective: None,
                },
            )
            .await
            .expect("update");
        assert!(applied);
        let got = store.get(&g.id).await.expect("get").expect("present");
        assert_eq!(got.status, GoalStatus::Complete);
    }

    #[tokio::test]
    async fn update_unknown_returns_false() {
        let store = MemoryGoalStore::default();
        let applied = store
            .update(&GoalId::new(), &ThreadGoalUpdate::default())
            .await
            .expect("update");
        assert!(!applied);
    }

    #[tokio::test]
    async fn account_usage_accumulates_and_flips_budget() {
        let store = MemoryGoalStore::default();
        let session = SessionId::new();
        let g = goal(&session, Some(100));
        store.create(&g).await.expect("create");

        let after = store
            .account_usage(&g.id, 40, 5, Utc::now())
            .await
            .expect("account")
            .expect("present");
        assert_eq!(after.tokens_used, 40);
        assert_eq!(after.time_used_seconds, 5);
        assert_eq!(after.status, GoalStatus::Active);

        let after = store
            .account_usage(&g.id, 70, 3, Utc::now())
            .await
            .expect("account")
            .expect("present");
        assert_eq!(after.tokens_used, 110);
        assert_eq!(after.time_used_seconds, 8);
        assert_eq!(after.status, GoalStatus::BudgetLimited);
    }

    #[tokio::test]
    async fn account_usage_flips_exactly_at_budget_boundary() {
        let store = MemoryGoalStore::default();
        let session = SessionId::new();
        let g = goal(&session, Some(100));
        store.create(&g).await.expect("create");
        // One token below the budget stays active.
        let below = store
            .account_usage(&g.id, 99, 0, Utc::now())
            .await
            .expect("account")
            .expect("present");
        assert_eq!(below.status, GoalStatus::Active);
        // Reaching the budget exactly (tokens_used == budget) flips: pins >= vs >.
        let at = store
            .account_usage(&g.id, 1, 0, Utc::now())
            .await
            .expect("account")
            .expect("present");
        assert_eq!(at.tokens_used, 100);
        assert_eq!(at.status, GoalStatus::BudgetLimited);
    }

    #[tokio::test]
    async fn account_usage_clamps_negative_deltas() {
        let store = MemoryGoalStore::default();
        let session = SessionId::new();
        let g = goal(&session, None);
        store.create(&g).await.expect("create");
        let after = store
            .account_usage(&g.id, -5, -9, Utc::now())
            .await
            .expect("account")
            .expect("present");
        assert_eq!(after.tokens_used, 0);
        assert_eq!(after.time_used_seconds, 0);
    }

    #[tokio::test]
    async fn unbudgeted_goal_never_budget_limits() {
        let store = MemoryGoalStore::default();
        let session = SessionId::new();
        let g = goal(&session, None);
        store.create(&g).await.expect("create");
        let after = store
            .account_usage(&g.id, 1_000_000, 0, Utc::now())
            .await
            .expect("account")
            .expect("present");
        assert_eq!(after.status, GoalStatus::Active);
    }

    #[tokio::test]
    async fn delete_then_create_allows_new_goal() {
        let store = MemoryGoalStore::default();
        let session = SessionId::new();
        let g = goal(&session, None);
        store.create(&g).await.expect("create");
        assert!(store.delete(&g.id).await.expect("delete"));
        // session is free again
        store.create(&goal(&session, None)).await.expect("recreate");
    }

    async fn create_with_status(store: &MemoryGoalStore, status: GoalStatus) -> ThreadGoal {
        let g = goal(&SessionId::new(), None);
        store.create(&g).await.expect("create");
        let update = ThreadGoalUpdate {
            objective: None,
            status: Some(status),
        };
        assert!(store.update(&g.id, &update).await.expect("update"));
        store.get(&g.id).await.expect("get").expect("present")
    }

    #[tokio::test]
    async fn list_by_status_filters() {
        let store = MemoryGoalStore::default();
        let suspended = create_with_status(&store, GoalStatus::Suspended).await;
        let _paused = create_with_status(&store, GoalStatus::Paused).await;
        let _active = create_with_status(&store, GoalStatus::Active).await;

        let listed = store
            .list_by_status(GoalStatus::Suspended, Page::first(10))
            .await
            .expect("list");
        assert_eq!(
            listed.iter().map(|g| &g.id).collect::<Vec<_>>(),
            vec![&suspended.id]
        );
    }

    #[tokio::test]
    async fn resume_suspended_flips_only_suspended_and_is_idempotent() {
        let store = MemoryGoalStore::default();
        let suspended = create_with_status(&store, GoalStatus::Suspended).await;
        let paused = create_with_status(&store, GoalStatus::Paused).await;
        let blocked = create_with_status(&store, GoalStatus::Blocked).await;

        let at = Utc::now();
        let resumed = store.resume_suspended(at).await.expect("resume");

        assert_eq!(
            resumed.iter().map(|g| &g.id).collect::<Vec<_>>(),
            vec![&suspended.id]
        );
        let got = store
            .get(&suspended.id)
            .await
            .expect("get")
            .expect("present");
        assert_eq!(got.status, GoalStatus::Active);
        assert_eq!(got.updated_at, at);
        // Host-paused and blocked goals stay untouched.
        for (id, status) in [
            (&paused.id, GoalStatus::Paused),
            (&blocked.id, GoalStatus::Blocked),
        ] {
            let g = store.get(id).await.expect("get").expect("present");
            assert_eq!(g.status, status);
        }
        // Second call returns nothing.
        let again = store.resume_suspended(Utc::now()).await.expect("resume");
        assert!(again.is_empty());
    }
}
