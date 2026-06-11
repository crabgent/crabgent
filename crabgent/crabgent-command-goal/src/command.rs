//! [`GoalCommand`]: the `/goal` slash-command adapter.

use async_trait::async_trait;
use crabgent_command::{Command, CommandCtx, CommandError, CommandName, CommandOutput};
use crabgent_core::{Action, Owner};
use crabgent_hook_goal::{GoalError, GoalRuntime};
use crabgent_store::ThreadGoal;

use crate::parser::GoalCmd;

const COMMAND_NAME: &str = "goal";
const DESCRIPTION: &str = "Show or control this thread's goal: \
    `/goal` shows it, `/goal <objective>` sets it, \
    `/goal pause|resume|clear` control it.";

/// Host `/goal` command over a [`GoalRuntime`]. No kernel run is started, and
/// pause/resume/clear are host-only (never reachable as model tools).
pub struct GoalCommand {
    name: CommandName,
    runtime: GoalRuntime,
}

impl GoalCommand {
    /// Build a goal command around a host [`GoalRuntime`].
    #[must_use]
    pub fn new(runtime: GoalRuntime) -> Self {
        Self {
            name: COMMAND_NAME
                .parse()
                .expect("static goal command name is valid"),
            runtime,
        }
    }
}

#[async_trait]
impl Command for GoalCommand {
    fn name(&self) -> &CommandName {
        &self.name
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    async fn policy_action(&self, input: &str, ctx: &CommandCtx) -> Result<Action, CommandError> {
        let owner = Some(Owner::new(ctx.subject().id()));
        Ok(match GoalCmd::parse(input) {
            // Read-only view of the goal.
            GoalCmd::Show => Action::GoalGet { owner },
            // Host-controlled mutations: set objective, pause, resume, clear.
            GoalCmd::Set(_) | GoalCmd::Pause | GoalCmd::Resume | GoalCmd::Clear => {
                Action::GoalManage { owner }
            }
        })
    }

    async fn execute(&self, input: &str, ctx: &CommandCtx) -> Result<CommandOutput, CommandError> {
        let owner = Owner::new(ctx.subject().id());
        let session = ctx.session_id();
        let reply = match GoalCmd::parse(input) {
            GoalCmd::Show => match self.runtime.get(session).await? {
                Some(goal) => format_goal(&goal),
                None => "No goal set for this thread.".to_owned(),
            },
            GoalCmd::Set(objective) => {
                let goal = self
                    .runtime
                    .set_objective(&owner, session, &objective, None)
                    .await
                    .map_err(map_goal_error)?;
                format!("Goal set: {}", goal.objective)
            }
            GoalCmd::Pause => reply_for(
                self.runtime.pause(session).await?,
                "Goal paused.",
                "No goal to pause.",
            ),
            GoalCmd::Resume => reply_for(
                self.runtime.resume(session).await?,
                "Goal resumed.",
                "No goal to resume.",
            ),
            GoalCmd::Clear => reply_for(
                self.runtime.clear(session).await?,
                "Goal cleared.",
                "No goal to clear.",
            ),
        };
        ctx.send_reply(reply.clone())
            .await
            .map_err(|err| CommandError::Execution(format!("goal reply send failed: {err}")))?;
        Ok(CommandOutput::new(reply))
    }
}

fn format_goal(goal: &ThreadGoal) -> String {
    let budget = goal
        .token_budget
        .map_or_else(|| "unbounded".to_owned(), |b| b.to_string());
    format!(
        "Goal: {objective}\nStatus: {status}\nTokens used: {used} / {budget} | Time: {time}s",
        objective = goal.objective,
        status = goal.status.as_str(),
        used = goal.tokens_used,
        time = goal.time_used_seconds,
    )
}

fn reply_for(applied: bool, yes: &str, no: &str) -> String {
    if applied {
        yes.to_owned()
    } else {
        no.to_owned()
    }
}

fn map_goal_error(err: GoalError) -> CommandError {
    match err {
        GoalError::Invalid(message) => CommandError::InvalidArgs(message),
        GoalError::Store(store) => CommandError::Store(store),
    }
}

#[cfg(test)]
mod tests;
