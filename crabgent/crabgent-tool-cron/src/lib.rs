//! # crabgent-tool-cron
//!
//! [`CronTool`] exposes cron-job CRUD to the LLM as a single tool with
//! op-arg dispatch (`create`/`list`/`get`/`update`/`delete`).
//!
//! The tool is policy-gated per operation via typed `Action::Cron*`
//! variants. The backend boundary is [`crabgent_store::traits::CronStore`];
//! scheduler internals such as claiming, finishing, and stuck-claim recovery
//! stay inside `crabgent-cron`.

#![forbid(unsafe_code)]

mod args;
mod output;
mod schedule;
pub mod tool;

pub use tool::CronTool;
