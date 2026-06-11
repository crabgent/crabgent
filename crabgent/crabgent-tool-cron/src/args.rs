//! Deserializable argument surface for [`crate::CronTool`].

use crabgent_core::{MemoryScope, ReasoningEffort};
use crabgent_store::records::{CronSchedule, ModelTargetDto};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Create,
    List,
    Get,
    Update,
    Delete,
}

impl Op {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::List => "list",
            Self::Get => "get",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

#[derive(Clone, Default)]
pub enum FieldUpdate<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

impl<T> FieldUpdate<T> {
    pub fn into_create_value(self) -> Option<T> {
        match self {
            Self::Keep | Self::Clear => None,
            Self::Set(value) => Some(value),
        }
    }
}

impl<'de, T> Deserialize<'de> for FieldUpdate<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<T>::deserialize(deserializer).map(|value| match value {
            Some(value) => Self::Set(value),
            None => Self::Clear,
        })
    }
}

#[derive(Deserialize)]
pub struct Args {
    pub op: Op,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub scope: MemoryScope,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub schedule: Option<CronSchedule>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub run_once: Option<bool>,
    #[serde(default)]
    pub model_override: FieldUpdate<ModelTargetDto>,
    #[serde(default)]
    pub reasoning_effort_override: FieldUpdate<ReasoningEffort>,
    #[serde(default)]
    pub pre_command: FieldUpdate<String>,
    #[serde(default)]
    pub delivery_ctx: Option<Value>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

impl Args {
    pub fn required_string(&self, value: Option<&str>, field: &str) -> Result<String, String> {
        value
            .map(str::to_owned)
            .ok_or_else(|| format!("{field} required for op={}", self.op.as_str()))
    }

    pub fn required_schedule(&self) -> Result<CronSchedule, String> {
        self.schedule
            .clone()
            .ok_or_else(|| format!("schedule required for op={}", self.op.as_str()))
    }
}
