//! # crabgent-tool-task
//!
//! [`TaskTool`] exposes background task create/list/get/cancel operations to
//! the LLM as one policy-gated `task` tool.

#![forbid(unsafe_code)]

mod args;
mod blocking;
mod depth;
mod output;
pub mod tool;

pub use tool::TaskTool;
