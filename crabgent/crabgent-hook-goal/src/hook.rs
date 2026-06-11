//! [`GoalHook`]: turn-start steering injection plus per-turn token/time
//! accounting for the active thread goal.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crabgent_core::run_id::RunId;
use crabgent_core::{
    CancelReason, Decision, Hook, LlmRequest, LlmResponse, Message, Outcome, RunCtx, Usage,
};
use crabgent_store::{GoalId, GoalStatus, GoalStore, SessionId, ThreadGoal, ThreadGoalUpdate};

use crate::steering::{SteeringKind, defang_user_sentinels, steering_message};

/// Per-run accounting state for the goal active at turn start.
#[derive(Debug, Clone)]
struct TurnAccount {
    goal_id: GoalId,
    last_at: DateTime<Utc>,
}

/// Hook that injects goal steering at turn start and charges token/time usage
/// to the active goal after each LLM call.
///
/// Wire it AFTER a session-persisting hook so the session id is resolved on
/// `RunCtx` before this hook reads it, and after any history-prepending hook so
/// the steering block is the last item the model sees before responding. The
/// hook is fail-open: store failures are logged and never abort a run.
pub struct GoalHook {
    store: Arc<dyn GoalStore>,
    accounting: Arc<Mutex<HashMap<RunId, TurnAccount>>>,
}

impl GoalHook {
    #[must_use]
    pub fn new(store: Arc<dyn GoalStore>) -> Self {
        Self {
            store,
            accounting: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn lock(&self) -> Option<std::sync::MutexGuard<'_, HashMap<RunId, TurnAccount>>> {
        match self.accounting.lock() {
            Ok(guard) => Some(guard),
            Err(poisoned) => {
                crabgent_log::warn!("goal accounting mutex poisoned");
                Some(poisoned.into_inner())
            }
        }
    }

    async fn active_goal(&self, ctx: &RunCtx) -> Option<ThreadGoal> {
        let session = SessionId::from_str(ctx.session_id()?).ok()?;
        match self.store.get_for_session(&session).await {
            Ok(goal) => goal,
            Err(err) => {
                crabgent_log::warn!(
                    run_id = %ctx.run_id,
                    error_kind = err.kind(),
                    "goal lookup failed; skipping steering",
                );
                None
            }
        }
    }

    fn record_turn_start(&self, run_id: &RunId, goal_id: GoalId, now: DateTime<Utc>) {
        if let Some(mut guard) = self.lock() {
            guard.insert(
                run_id.clone(),
                TurnAccount {
                    goal_id,
                    last_at: now,
                },
            );
        }
    }

    fn take_account_point(&self, run_id: &RunId) -> Option<(GoalId, DateTime<Utc>)> {
        let guard = self.lock()?;
        guard
            .get(run_id)
            .map(|account| (account.goal_id.clone(), account.last_at))
    }

    fn advance_account_point(&self, run_id: &RunId, at: DateTime<Utc>) {
        if let Some(mut guard) = self.lock()
            && let Some(account) = guard.get_mut(run_id)
        {
            account.last_at = at;
        }
    }
}

/// Billable tokens for one LLM call: input minus cache reads, plus output,
/// matching the Codex goal-accounting delta (cached input is never charged).
fn billable_tokens(usage: Usage) -> i64 {
    i64::from(usage.input_tokens).saturating_sub(i64::from(usage.cache_read_tokens))
        + i64::from(usage.output_tokens)
}

#[async_trait]
impl Hook for GoalHook {
    async fn on_user_prompt_submit(
        &self,
        msgs: &[Message],
        ctx: &RunCtx,
    ) -> Decision<Vec<Message>> {
        let mut messages = msgs.to_vec();
        // Trust fence: neutralize any forged authentic sentinel in user input
        // regardless of whether a goal is active.
        let mut changed = defang_user_sentinels(&mut messages);

        match self.active_goal(ctx).await {
            Some(goal) if goal.status == GoalStatus::Active => {
                self.record_turn_start(&ctx.run_id, goal.id.clone(), Utc::now());
                messages.push(steering_message(&goal, SteeringKind::Reminder));
                changed = true;
            }
            Some(goal) if goal.status == GoalStatus::BudgetLimited => {
                messages.push(steering_message(&goal, SteeringKind::BudgetLimit));
                changed = true;
            }
            _ => {}
        }

        if changed {
            Decision::Replace(messages)
        } else {
            Decision::Continue
        }
    }

    async fn after_llm(
        &self,
        _req: &LlmRequest,
        resp: &LlmResponse,
        ctx: &RunCtx,
    ) -> Decision<LlmResponse> {
        let Some((goal_id, last_at)) = self.take_account_point(&ctx.run_id) else {
            return Decision::Continue;
        };
        let now = Utc::now();
        let token_delta = billable_tokens(resp.usage);
        let time_delta = (now - last_at).num_seconds().max(0);
        if let Err(err) = self
            .store
            .account_usage(&goal_id, token_delta, time_delta, now)
            .await
        {
            crabgent_log::warn!(
                run_id = %ctx.run_id,
                error_kind = err.kind(),
                "goal usage accounting failed",
            );
        }
        self.advance_account_point(&ctx.run_id, now);
        Decision::Continue
    }

    async fn on_stop(&self, ctx: &RunCtx, outcome: &Outcome) {
        let account = if let Some(mut guard) = self.lock() {
            guard.remove(&ctx.run_id)
        } else {
            None
        };
        let Some(account) = account else {
            return;
        };
        // Pause intent discrimination: a cooperative pause exit or a
        // shutdown-attributed cancel suspends the goal (system-paused,
        // auto-resumed at startup via `GoalStore::resume_suspended`); any
        // other cancel keeps the host-paused semantics and is never
        // auto-resumed (user cancel intent survives a restart).
        let target = match outcome {
            Outcome::Paused => GoalStatus::Suspended,
            Outcome::Cancelled => match ctx.cancel_reason() {
                Some(CancelReason::Shutdown) => GoalStatus::Suspended,
                _ => GoalStatus::Paused,
            },
            _ => return,
        };
        charge_trailing_wall_time(self.store.as_ref(), &account, ctx).await;
        pause_active_goal(self.store.as_ref(), &account.goal_id, ctx, target).await;
    }
}

/// Charge the wall-time tail between the last accounted point and the
/// pause: tool execution time after the final `after_llm` would otherwise
/// be lost. Tokens of an interrupted provider call stay unknowable
/// (`after_llm` never fired for it) and are documented as lost.
async fn charge_trailing_wall_time(store: &dyn GoalStore, account: &TurnAccount, ctx: &RunCtx) {
    let now = Utc::now();
    let time_delta = (now - account.last_at).num_seconds().max(0);
    if time_delta == 0 {
        return;
    }
    if let Err(err) = store
        .account_usage(&account.goal_id, 0, time_delta, now)
        .await
    {
        crabgent_log::warn!(
            run_id = %ctx.run_id,
            goal_id = %account.goal_id,
            error_kind = err.kind(),
            "goal trailing wall-time accounting failed",
        );
    }
}

async fn pause_active_goal(
    store: &dyn GoalStore,
    goal_id: &GoalId,
    ctx: &RunCtx,
    target: GoalStatus,
) {
    let goal = match store.get(goal_id).await {
        Ok(Some(goal)) => goal,
        Ok(None) => return,
        Err(err) => {
            log_pause_lookup_failed(ctx, goal_id, &err);
            return;
        }
    };
    if goal.status != GoalStatus::Active {
        return;
    }
    if let Err(err) = store
        .update(
            goal_id,
            &ThreadGoalUpdate {
                objective: None,
                status: Some(target),
            },
        )
        .await
    {
        log_pause_update_failed(ctx, goal_id, &err);
    }
}

fn log_pause_lookup_failed(ctx: &RunCtx, goal_id: &GoalId, err: &crabgent_store::StoreError) {
    crabgent_log::warn!(
        run_id = %ctx.run_id,
        goal_id = %goal_id,
        error_kind = err.kind(),
        "goal cancel pause lookup failed",
    );
}

fn log_pause_update_failed(ctx: &RunCtx, goal_id: &GoalId, err: &crabgent_store::StoreError) {
    crabgent_log::warn!(
        run_id = %ctx.run_id,
        goal_id = %goal_id,
        error_kind = err.kind(),
        "goal cancel pause update failed",
    );
}

#[cfg(test)]
mod tests;
