//! Deserializable argument surface for [`crate::GoalTool`].

use serde::Deserialize;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Get,
    Create,
    Update,
}

#[derive(Debug, Deserialize)]
pub struct Args {
    pub op: Op,
    /// Objective for `op=create`.
    #[serde(default)]
    pub objective: Option<String>,
    /// Optional positive token budget for `op=create`.
    #[serde(default)]
    pub token_budget: Option<i64>,
    /// Target status for `op=update`: only `complete` or `blocked` are valid.
    #[serde(default)]
    pub status: Option<String>,
}
