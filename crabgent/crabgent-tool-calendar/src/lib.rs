//! # crabgent-tool-calendar
//!
//! [`CalendarTool`] exposes read-only calendar operations to the LLM as a
//! single `calendar` tool with op-arg dispatch.

#![forbid(unsafe_code)]

mod args;
mod ops;
mod output;
pub mod tool;

pub use tool::CalendarTool;
