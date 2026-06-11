//! LLM-facing model registry tool.
//!
//! Opt-in via `KernelBuilder::add_tool(ModelRegistryTool::new(kernel, policy,
//! session_store, global_model_override_store,
//! global_reasoning_effort_override_store))`.

#![forbid(unsafe_code)]

mod args;
mod catalog;
mod current;
mod output;
mod overrides;
mod tool;

pub use tool::ModelRegistryTool;
