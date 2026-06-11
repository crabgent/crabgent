//! # crabgent-hook-goal
//!
//! Runtime support for persistent thread goals.
//!
//! - [`GoalHook`] injects concise, sentinel-anchored, XML-escaped steering at
//!   turn start while a goal is active (or a budget-limit notice once the
//!   budget is spent), defangs forged goal sentinels in user input, and
//!   charges token and elapsed-time usage to the active goal after each LLM
//!   call. The store flips the goal to `budget_limited` when the token budget
//!   is reached. The hook is fail-open: store errors are logged, never fatal.
//! - [`GoalRuntime`] is the host-facing control surface (set objective, pause,
//!   resume, clear) plus the turn-continuation mechanism: when a goal is still
//!   active after a turn, [`GoalRuntime::continuation_input`] yields the
//!   steering input the host can re-pump the kernel with. Paused, blocked,
//!   budget-limited, complete, and absent goals never continue.

mod hook;
mod runtime;
mod steering;

pub use hook::GoalHook;
pub use runtime::{GoalError, GoalRuntime};
