//! # crabgent-tool-goal
//!
//! LLM-facing thread-goal tool. A single `goal` tool dispatches `get`,
//! `create`, and `update` operations over a [`crabgent_store::GoalStore`],
//! gated by a [`crabgent_core::PolicyHook`] on typed `Action::Goal*` variants.
//!
//! The model surface is intentionally tiny:
//! - `create` takes an `objective` (and an optional positive `token_budget`).
//!   Creation is meant to happen only on an explicit user/system request; a
//!   fail-closed policy gates it on the [`GOAL_ORIGIN_ATTR`] subject attr that
//!   only a trusted path (e.g. a `/goal` command) stamps.
//! - `update` may only mark the goal `complete` or `blocked`. Pause, resume,
//!   clear, usage-limit, and budget-limit transitions are host-controlled and
//!   never reachable from this tool.
//! - the goal is scoped to the run's session: the tool always operates on the
//!   current thread's goal, never on an arbitrary id supplied by the model.

mod args;
mod output;
mod tool;

pub use tool::{GOAL_ORIGIN_ATTR, GOAL_ORIGIN_SYSTEM, GOAL_ORIGIN_USER, GoalTool};
