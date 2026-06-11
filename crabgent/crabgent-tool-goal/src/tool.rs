//! [`GoalTool`] implementation.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::tool::{
    Tool, ToolCtx, gate_tool_action, parse_args_with_context, soft_error_object,
};
use crabgent_core::{Action, Owner, PolicyHook, ToolResult};
use crabgent_store::{
    GoalStatus, GoalStore, SessionId, StoreError, ThreadGoal, ThreadGoalUpdate,
    validate_goal_budget, validate_goal_objective,
};
use serde_json::{Value, json};

use crate::args::{Args, Op};
use crate::output::goal_to_json;

const TOOL_NAME: &str = "goal";

/// Subject attribute that trusted paths stamp to authorize goal creation.
/// A fail-closed policy gates `Action::GoalCreate` on this attr so the model
/// cannot create a goal on its own.
pub const GOAL_ORIGIN_ATTR: &str = "goal_origin";
/// [`GOAL_ORIGIN_ATTR`] value for an explicit user request (e.g. `/goal`).
pub const GOAL_ORIGIN_USER: &str = "user";
/// [`GOAL_ORIGIN_ATTR`] value for a system/developer-initiated request.
pub const GOAL_ORIGIN_SYSTEM: &str = "system";

const DESCRIPTION: &str = "Manage the persistent goal for the current thread. \
    Operations: `get`, `create`, `update`. \
    `get` returns the current goal (or null). \
    `create` starts a goal from an explicit `objective` (and an optional \
    positive `token_budget`); create a goal ONLY when the user or \
    system/developer instructions explicitly ask for one, never inferred from \
    an ordinary task. create fails if the thread already has a goal. \
    `update` may only set `status` to `complete` or `blocked`: use `complete` \
    only when the objective is actually achieved (not because a budget is \
    nearly spent or you are stopping), and `blocked` only after the SAME \
    blocking condition has persisted across at least 3 consecutive goal turns \
    and genuinely needs user input or an external change. You cannot pause, \
    resume, clear, usage-limit, or budget-limit a goal; those are controlled \
    by the user or system.";

/// LLM-facing thread-goal tool. Holds store + policy by `Arc`.
pub struct GoalTool {
    store: Arc<dyn GoalStore>,
    policy: Arc<dyn PolicyHook>,
}

impl GoalTool {
    #[must_use]
    pub fn new(store: Arc<dyn GoalStore>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { store, policy }
    }

    async fn gate(&self, action: &Action, ctx: &ToolCtx) -> Result<(), ToolError> {
        gate_tool_action(self.policy.as_ref(), ctx, action).await
    }

    async fn dispatch(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        let parsed: Args = parse_args_with_context(args, "goal args")?;
        let Some(session) = session_id(ctx) else {
            return Ok(soft_error_object(
                "goal tool requires an active session/thread; none is bound to this run",
            ));
        };
        let owner = Owner::new(ctx.subject.id());
        match parsed.op {
            Op::Get => self.do_get(&owner, &session, ctx).await,
            Op::Create => self.do_create(&parsed, &owner, &session, ctx).await,
            Op::Update => self.do_update(&parsed, &owner, &session, ctx).await,
        }
    }

    async fn do_get(
        &self,
        owner: &Owner,
        session: &SessionId,
        ctx: &ToolCtx,
    ) -> Result<ToolResult, ToolError> {
        self.gate(
            &Action::GoalGet {
                owner: Some(owner.clone()),
            },
            ctx,
        )
        .await?;
        let goal = self.load_for_session(session).await?;
        Ok(ToolResult::success(json!({
            "goal": goal.as_ref().map(goal_to_json),
        })))
    }

    async fn do_create(
        &self,
        args: &Args,
        owner: &Owner,
        session: &SessionId,
        ctx: &ToolCtx,
    ) -> Result<ToolResult, ToolError> {
        let objective = args
            .objective
            .as_deref()
            .ok_or_else(|| ToolError::InvalidArgs("objective required for op=create".to_owned()))?;
        let objective = validate_goal_objective(objective).map_err(ToolError::InvalidArgs)?;
        validate_goal_budget(args.token_budget).map_err(ToolError::InvalidArgs)?;
        self.gate(
            &Action::GoalCreate {
                owner: Some(owner.clone()),
            },
            ctx,
        )
        .await?;
        let goal = ThreadGoal::new(owner.clone(), session.clone(), objective, args.token_budget);
        match self.store.create(&goal).await {
            Ok(()) => Ok(ToolResult::success(
                json!({ "created": true, "goal": goal_to_json(&goal) }),
            )),
            Err(StoreError::Conflict(_)) => {
                let existing = self.load_for_session(session).await?;
                Ok(ToolResult::soft_error(json!({
                    "error": "this thread already has a goal; complete or clear it before creating a new one",
                    "goal": existing.as_ref().map(goal_to_json),
                })))
            }
            Err(err) => Err(store_unavailable("goal.create", &err)),
        }
    }

    async fn do_update(
        &self,
        args: &Args,
        owner: &Owner,
        session: &SessionId,
        ctx: &ToolCtx,
    ) -> Result<ToolResult, ToolError> {
        let status = parse_update_status(args.status.as_deref())?;
        self.gate(
            &Action::GoalUpdate {
                owner: Some(owner.clone()),
            },
            ctx,
        )
        .await?;
        let Some(goal) = self.load_for_session(session).await? else {
            return Ok(soft_error_object(
                "no goal exists for this thread; nothing to update",
            ));
        };
        let updated = self
            .store
            .update(
                &goal.id,
                &ThreadGoalUpdate {
                    status: Some(status),
                    objective: None,
                },
            )
            .await
            .map_err(|err| store_unavailable("goal.update", &err))?;
        if !updated {
            return Ok(soft_error_object(
                "no goal exists for this thread; nothing to update",
            ));
        }
        let refreshed = self.load_for_session(session).await?;
        Ok(ToolResult::success(json!({
            "updated": true,
            "goal": refreshed.as_ref().map(goal_to_json),
        })))
    }

    async fn load_for_session(&self, session: &SessionId) -> Result<Option<ThreadGoal>, ToolError> {
        self.store
            .get_for_session(session)
            .await
            .map_err(|err| store_unavailable("goal.get", &err))
    }
}

#[async_trait]
impl Tool for GoalTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["op"],
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["get", "create", "update"],
                    "description": "Operation to perform."
                },
                "objective": {
                    "type": "string",
                    "description": "Objective for op=create. Required for create."
                },
                "token_budget": {
                    "type": ["integer", "null"],
                    "minimum": 1,
                    "description": "Optional positive token budget for op=create. Omit unless explicitly requested."
                },
                "status": {
                    "type": "string",
                    "enum": ["complete", "blocked"],
                    "description": "Target status for op=update. Only complete or blocked are permitted."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        self.dispatch(args, ctx).await.map(|result| result.output)
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        self.dispatch(args, ctx).await
    }
}

fn session_id(ctx: &ToolCtx) -> Option<SessionId> {
    ctx.session_id
        .as_deref()
        .and_then(|raw| SessionId::from_str(raw).ok())
}

fn parse_update_status(status: Option<&str>) -> Result<GoalStatus, ToolError> {
    match status {
        Some("complete") => Ok(GoalStatus::Complete),
        Some("blocked") => Ok(GoalStatus::Blocked),
        Some(other) => Err(ToolError::InvalidArgs(format!(
            "update_goal can only set status to complete or blocked, got '{other}'"
        ))),
        None => Err(ToolError::InvalidArgs(
            "status required for op=update (complete or blocked)".to_owned(),
        )),
    }
}

fn store_unavailable(op: &str, err: &StoreError) -> ToolError {
    crabgent_log::warn!(
        op = %op,
        error_kind = err.kind(),
        transient = err.is_transient(),
        "goal store unavailable",
    );
    ToolError::backend_unavailable(op, err)
}

#[cfg(test)]
mod tests;
