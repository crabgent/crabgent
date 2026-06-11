//! JSON projection helpers for task tool responses.

use chrono::{DateTime, Utc};
use crabgent_core::{Owner, ReasoningEffort};
use crabgent_store::{Task, TaskId, TaskStatus};
use serde_json::{Value, json};

pub fn create_running(id: &TaskId) -> Value {
    json!({
        "task_id": id.to_string(),
        "status": TaskStatus::Running.as_str(),
    })
}

pub fn create_terminal(task: &Task) -> Value {
    json!({
        "task_id": task.id.to_string(),
        "status": task.status.as_str(),
        "output": &task.output,
        "error": &task.error,
    })
}

pub fn list(tasks: &[Task]) -> Value {
    json!({
        "count": tasks.len(),
        "tasks": tasks.iter().map(summary).collect::<Vec<_>>(),
    })
}

pub fn get(task: &Task) -> Value {
    json!({ "task": detail(task) })
}

pub fn cancel(id: &TaskId, cancelled: bool, status: TaskStatus) -> Value {
    json!({
        "task_id": id.to_string(),
        "cancelled": cancelled,
        "status": status.as_str(),
    })
}

fn summary(task: &Task) -> Value {
    json!({
        "task_id": task.id.to_string(),
        "status": task.status.as_str(),
        "owner": owner_to_json(&task.owner),
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.updated_at.to_rfc3339(),
        "reasoning_effort_override": task.reasoning_effort_override.map(ReasoningEffort::as_str),
    })
}

fn detail(task: &Task) -> Value {
    json!({
        "task_id": task.id.to_string(),
        "status": task.status.as_str(),
        "owner": owner_to_json(&task.owner),
        "prompt": &task.prompt,
        "output": &task.output,
        "error": &task.error,
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.updated_at.to_rfc3339(),
        "finished_at": opt_time(task.finished_at.as_ref()),
        "parent_session_id": task.parent_session_id.as_ref().map(ToString::to_string),
        "parent_task_id": task.parent_task_id.as_ref().map(ToString::to_string),
        "reasoning_effort_override": task.reasoning_effort_override.map(ReasoningEffort::as_str),
    })
}

fn owner_to_json(owner: &Owner) -> &str {
    owner.as_str()
}

fn opt_time(value: Option<&DateTime<Utc>>) -> Option<String> {
    value.map(DateTime::to_rfc3339)
}
