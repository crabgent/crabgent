//! # crabgent-command-goal
//!
//! `/goal` slash-command surface over the host [`GoalRuntime`]. This is the
//! host-controlled half of the thread-goal feature: it never starts a kernel
//! run and never exposes pause/resume/clear to the model.
//!
//! Sub-commands:
//! - `/goal` shows the current goal and its usage.
//! - `/goal <objective>` sets (creates or replaces) the thread objective.
//! - `/goal pause` / `/goal resume` toggle the active state.
//! - `/goal clear` removes the goal, freeing the thread for a new one.
//!
//! Set/pause/resume/clear gate on `Action::GoalManage` (host control); show
//! gates on `Action::GoalGet`. The model's `create_goal` tool gates on the
//! separate `Action::GoalCreate` instead, so the two creation paths carry
//! distinct authorization.
//!
//! [`GoalRuntime`]: crabgent_hook_goal::GoalRuntime

mod command;
mod parser;

pub use command::GoalCommand;
