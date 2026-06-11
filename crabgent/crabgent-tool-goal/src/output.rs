//! Model-facing JSON projection of a [`ThreadGoal`].

use crabgent_store::ThreadGoal;
use serde_json::{Value, json};

/// Render a goal for the LLM. `goal_id` is intentionally omitted: the model
/// never references a goal by id (the tool always operates on the session's
/// goal), so exposing it would only invite invalid id arguments.
pub fn goal_to_json(goal: &ThreadGoal) -> Value {
    json!({
        "objective": goal.objective,
        "status": goal.status.as_str(),
        "token_budget": goal.token_budget,
        "tokens_used": goal.tokens_used,
        "remaining_tokens": goal.remaining_tokens(),
        "time_used_seconds": goal.time_used_seconds,
        "created_at": goal.created_at.to_rfc3339(),
        "updated_at": goal.updated_at.to_rfc3339(),
    })
}
