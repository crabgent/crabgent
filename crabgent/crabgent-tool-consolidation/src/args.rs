//! JSON args for [`crate::ConsolidationTool`].

use crabgent_core::MemoryScope;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Run,
    Status,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Args {
    pub op: Op,
    pub scope: MemoryScope,
}
