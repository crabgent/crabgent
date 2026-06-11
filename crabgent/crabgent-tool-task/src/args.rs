//! Deserializable argument surface for [`crate::TaskTool`].

use crabgent_core::message::Message;
use std::str::FromStr;

use crabgent_core::model::{ModelTarget, ReasoningEffort};
use crabgent_core::{Owner, ToolAccess};
use serde::Deserialize;
use serde::de::{self, Visitor};

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Create,
    List,
    Get,
    Cancel,
}

impl Op {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::List => "list",
            Self::Get => "get",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ContextInjection {
    pub context_messages: Option<Vec<Message>>,
    pub parent_session_id: Option<String>,
    pub context_mode: Option<String>,
    pub system_prompt: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReasoningEffortArg {
    #[default]
    Inherit,
    Clear,
    Set(ReasoningEffort),
}

struct ReasoningEffortArgVisitor;

impl<'de> Visitor<'de> for ReasoningEffortArgVisitor {
    type Value = ReasoningEffortArg;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("null or one of none, low, medium, high, xhigh")
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(ReasoningEffortArg::Clear)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(ReasoningEffortArg::Clear)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ReasoningEffort::from_str(&raw)
            .map(ReasoningEffortArg::Set)
            .map_err(de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for ReasoningEffortArg {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_option(ReasoningEffortArgVisitor)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Args {
    pub op: Op,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub model: Option<ModelTarget>,
    #[serde(default)]
    pub reasoning_effort: ReasoningEffortArg,
    #[serde(default, rename = "name")]
    pub name: Option<String>,
    #[serde(default)]
    pub block: bool,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub context: Option<ContextInjection>,
    #[serde(default)]
    pub tool_access: Option<ToolAccess>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

impl Args {
    pub fn owner(&self) -> Option<Owner> {
        self.owner.as_ref().map(|owner| Owner::new(owner.clone()))
    }

    pub fn required_prompt(&self) -> Result<String, String> {
        self.prompt
            .clone()
            .ok_or_else(|| "task.create: missing required field 'prompt'".to_owned())
    }

    pub fn required_task_id(&self) -> Result<&str, String> {
        self.task_id.as_deref().ok_or_else(|| {
            format!(
                "task.{}: missing required field 'task_id'",
                self.op.as_str()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: serde_json::Value) -> Args {
        serde_json::from_value(value).expect("args deserialize")
    }

    #[test]
    fn args_reasoning_effort_missing_inherits() {
        let args = parse(json!({"op": "create", "prompt": "p"}));

        assert_eq!(args.reasoning_effort, ReasoningEffortArg::Inherit);
    }

    #[test]
    fn args_reasoning_effort_null_clears() {
        let args = parse(json!({
            "op": "create",
            "prompt": "p",
            "reasoning_effort": null
        }));

        assert_eq!(args.reasoning_effort, ReasoningEffortArg::Clear);
    }

    #[test]
    fn args_reasoning_effort_none_sets_disabled() {
        let args = parse(json!({
            "op": "create",
            "prompt": "p",
            "reasoning_effort": "none"
        }));

        assert_eq!(
            args.reasoning_effort,
            ReasoningEffortArg::Set(ReasoningEffort::Disabled)
        );
    }

    #[test]
    fn args_create_missing_prompt_returns_error() {
        let args = parse(json!({"op": "create", "model": "m"}));

        let err = args.required_prompt().expect_err("expected error");

        assert_eq!(err, "task.create: missing required field 'prompt'");
    }

    #[test]
    fn args_get_missing_task_id_returns_error() {
        let args = parse(json!({"op": "get"}));

        let err = args.required_task_id().expect_err("expected error");

        assert_eq!(err, "task.get: missing required field 'task_id'");
    }

    #[test]
    fn args_cancel_missing_task_id_returns_error() {
        let args = parse(json!({"op": "cancel"}));

        let err = args.required_task_id().expect_err("expected error");

        assert_eq!(err, "task.cancel: missing required field 'task_id'");
    }
}
