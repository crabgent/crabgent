//! [`CronTool`] implementation.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_core::error::ToolError;
use crabgent_core::tool::{
    Tool, ToolCtx, clamp_positive_limit, gate_tool_action, parse_args_with_context,
};
use crabgent_core::{Action, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT, PolicyHook};
use crabgent_store::{CronJob, CronJobId, CronJobUpdate, CronStore, Page, StoreError};
use serde_json::{Value, json};

use crate::args::{Args, FieldUpdate, Op};
use crate::output::job_to_json;
use crate::schedule::{first_next_run, validate_schedule};

const TOOL_NAME: &str = "cron";

const DESCRIPTION: &str = "Manage persisted cron jobs. Operations: \
    `create`, `list`, `get`, `update`, `delete`. \
    create/update validate schedules before writing; list is scope-filtered; \
    get/update/delete policy-gate the requested scope before reading and \
    verify the stored scope before returning or mutating a job.";

/// LLM-facing cron CRUD tool. Holds store + policy by `Arc`.
pub struct CronTool {
    store: Arc<dyn CronStore>,
    policy: Arc<dyn PolicyHook>,
}

impl CronTool {
    pub fn new(store: Arc<dyn CronStore>, policy: Arc<dyn PolicyHook>) -> Self {
        Self { store, policy }
    }
}

#[async_trait]
impl Tool for CronTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["op"],
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["create", "list", "get", "update", "delete"],
                    "description": "Operation to perform."
                },
                "job_id": {"type": "string", "description": "Cron job id for op=get/update/delete."},
                "scope": cron_scope_schema(),
                "name": {"type": "string", "description": "Cron job name for op=create/update."},
                "prompt": {"type": "string", "description": "Prompt to run for op=create/update."},
                "schedule": {
                    "type": "object",
                    "description": "Exactly one schedule mode: interval_secs or cron_expr with optional cron_tz.",
                    "properties": {
                        "interval_secs": {"type": ["integer", "null"], "minimum": 1},
                        "cron_expr": {"type": ["string", "null"]},
                        "cron_tz": {"type": ["string", "null"]}
                    }
                },
                "enabled": {"type": "boolean", "default": true},
                "run_once": {"type": "boolean", "default": false},
                "model_override": {
                    "description": "Missing means keep on update, null means clear, string or provider object means set.",
                    "oneOf": [
                        {"type": "null"},
                        {"type": "string"},
                        {
                            "type": "object",
                            "required": ["provider", "id"],
                            "properties": {
                                "provider": {"type": "string"},
                                "id": {"type": "string"}
                            }
                        }
                    ]
                },
                "reasoning_effort_override": {
                    "type": ["string", "null"],
                    "enum": ["none", "low", "medium", "high", "xhigh", null],
                    "description": "Missing means keep on update, null means clear, string means set."
                },
                "pre_command": {
                    "type": ["string", "null"],
                    "description": "Missing means keep on update, null means clear, string means set."
                },
                "delivery_ctx": {"type": "object", "default": {}},
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT},
                "offset": {"type": "integer", "minimum": 0}
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: Args = parse_args_with_context(args, "cron args")?;
        match parsed.op {
            Op::Create => self.do_create(&parsed, ctx).await,
            Op::List => self.do_list(&parsed, ctx).await,
            Op::Get => self.do_get(&parsed, ctx).await,
            Op::Update => self.do_update(&parsed, ctx).await,
            Op::Delete => self.do_delete(&parsed, ctx).await,
        }
    }
}

impl CronTool {
    async fn do_create(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let schedule = args.required_schedule().map_err(ToolError::InvalidArgs)?;
        validate_schedule(&schedule)?;
        let now = Utc::now();
        let next_run = first_next_run(&schedule, now)?;
        let scope = args.scope.clone();
        self.gate(
            &Action::CronCreate {
                scope: scope.clone(),
            },
            ctx,
        )
        .await?;
        let job = CronJob {
            id: CronJobId::new(),
            name: args
                .required_string(args.name.as_deref(), "name")
                .map_err(ToolError::InvalidArgs)?,
            scope,
            prompt: args
                .required_string(args.prompt.as_deref(), "prompt")
                .map_err(ToolError::InvalidArgs)?,
            schedule,
            enabled: args.enabled.unwrap_or(true),
            run_once: args.run_once.unwrap_or(false),
            model_override: args.model_override.clone().into_create_value(),
            reasoning_effort_override: args.reasoning_effort_override.clone().into_create_value(),
            pre_command: args.pre_command.clone().into_create_value(),
            delivery_ctx: delivery_ctx(args)?,
            last_run: None,
            next_run,
            created_at: now,
            claimed_at: None,
        };
        self.store
            .create(&job)
            .await
            .map_err(|err| store_unavailable("cron.create", &err))?;
        Ok(json!({ "created": true, "job": job_to_json(&job) }))
    }

    async fn do_list(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let scope = args.scope.clone();
        self.gate(
            &Action::CronList {
                scope: scope.clone(),
            },
            ctx,
        )
        .await?;
        let page = page(args)?;
        let jobs = self
            .store
            .list(&scope, page)
            .await
            .map_err(|err| store_unavailable("cron.list", &err))?;
        Ok(json!({
            "count": jobs.len(),
            "jobs": jobs.iter().map(job_to_json).collect::<Vec<_>>()
        }))
    }

    async fn do_get(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let id = parse_job_id(args, "get")?;
        self.gate(
            &Action::CronGet {
                id: id.to_string(),
                scope: args.scope.clone(),
            },
            ctx,
        )
        .await?;
        let job = self.load_visible_required(&id, &args.scope).await?;
        self.gate(
            &Action::CronGet {
                id: job.id.to_string(),
                scope: job.scope.clone(),
            },
            ctx,
        )
        .await?;
        Ok(json!({ "job": job_to_json(&job) }))
    }

    async fn do_update(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let id = parse_job_id(args, "update")?;
        self.gate(
            &Action::CronUpdate {
                id: id.to_string(),
                scope: args.scope.clone(),
            },
            ctx,
        )
        .await?;
        let existing = self.load_visible_required(&id, &args.scope).await?;
        self.gate(
            &Action::CronUpdate {
                id: existing.id.to_string(),
                scope: existing.scope.clone(),
            },
            ctx,
        )
        .await?;
        let update = build_update(args)?;
        let updated = self
            .store
            .update(&existing.id, &update)
            .await
            .map_err(|err| store_unavailable("cron.update", &err))?;
        if !updated {
            return Err(ToolError::NotFound(format!("cron job {}", existing.id)));
        }
        let job = self.load_by_id(&existing.id).await?.ok_or_else(|| {
            ToolError::NotFound(format!("cron job {} disappeared after update", existing.id))
        })?;
        Ok(json!({ "updated": true, "job": job_to_json(&job) }))
    }

    async fn do_delete(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let id = parse_job_id(args, "delete")?;
        self.gate(
            &Action::CronDelete {
                id: id.to_string(),
                scope: args.scope.clone(),
            },
            ctx,
        )
        .await?;
        let job = self.load_visible_required(&id, &args.scope).await?;
        self.gate(
            &Action::CronDelete {
                id: job.id.to_string(),
                scope: job.scope.clone(),
            },
            ctx,
        )
        .await?;
        let deleted = self
            .store
            .delete(&job.id)
            .await
            .map_err(|err| store_unavailable("cron.delete", &err))?;
        if !deleted {
            return Err(ToolError::NotFound(format!("cron job {}", job.id)));
        }
        Ok(json!({ "deleted": true, "id": job.id.to_string() }))
    }

    async fn load_visible_required(
        &self,
        id: &CronJobId,
        scope: &crabgent_core::MemoryScope,
    ) -> Result<CronJob, ToolError> {
        self.load_by_id(id)
            .await?
            .filter(|job| scope.matches(&job.scope))
            .ok_or_else(|| ToolError::NotFound(format!("cron job {id}")))
    }

    async fn load_by_id(&self, id: &CronJobId) -> Result<Option<CronJob>, ToolError> {
        self.store
            .get(id)
            .await
            .map_err(|err| store_unavailable("cron.get", &err))
    }

    async fn gate(&self, action: &Action, ctx: &ToolCtx) -> Result<(), ToolError> {
        gate_tool_action(self.policy.as_ref(), ctx, action).await
    }
}

fn store_unavailable(op: &str, err: &StoreError) -> ToolError {
    crabgent_log::warn!(
        op = %op,
        error_kind = err.kind(),
        transient = err.is_transient(),
        "cron store unavailable",
    );
    ToolError::backend_unavailable(op, err)
}

/// Memory-scope schema with the cron-specific filter description attached.
fn cron_scope_schema() -> Value {
    let mut schema = crabgent_core::tool::memory_scope_schema();
    if let Value::Object(map) = &mut schema {
        map.insert(
            "description".to_string(),
            Value::String("Scope filter. Missing or empty means all cron jobs.".to_string()),
        );
    }
    schema
}

fn parse_job_id(args: &Args, op: &str) -> Result<CronJobId, ToolError> {
    let id = args
        .job_id
        .as_deref()
        .ok_or_else(|| ToolError::InvalidArgs(format!("job_id required for op={op}")))?;
    CronJobId::from_str(id).map_err(|err| ToolError::InvalidArgs(format!("job_id: {err}")))
}

fn page(args: &Args) -> Result<Page, ToolError> {
    let limit = args.limit.map_or(Ok(DEFAULT_SEARCH_LIMIT), |limit| {
        clamp_positive_limit(limit, MAX_SEARCH_LIMIT, "cron.list")
    })?;
    let offset = args.offset.unwrap_or(0);
    // `limit` (clamped to `MAX_SEARCH_LIMIT`) and `offset` are `u32`; widening
    // to `usize` is lossless on every supported (>= 32-bit) target.
    Ok(Page::new(limit as usize, offset as usize))
}

fn build_update(args: &Args) -> Result<CronJobUpdate, ToolError> {
    let mut update = CronJobUpdate {
        name: args.name.clone(),
        prompt: args.prompt.clone(),
        enabled: args.enabled,
        run_once: args.run_once,
        model_override: match args.model_override.clone() {
            FieldUpdate::Keep => None,
            FieldUpdate::Clear => Some(None),
            FieldUpdate::Set(value) => Some(Some(value)),
        },
        reasoning_effort_override: match args.reasoning_effort_override.clone() {
            FieldUpdate::Keep => None,
            FieldUpdate::Clear => Some(None),
            FieldUpdate::Set(value) => Some(Some(value)),
        },
        pre_command: match args.pre_command.clone() {
            FieldUpdate::Keep => None,
            FieldUpdate::Clear => Some(None),
            FieldUpdate::Set(value) => Some(Some(value)),
        },
        delivery_ctx: optional_delivery_ctx(args)?,
        ..Default::default()
    };
    if let Some(schedule) = &args.schedule {
        validate_schedule(schedule)?;
        let now = Utc::now();
        update.schedule = Some(schedule.clone());
        update.next_run = Some(first_next_run(schedule, now)?);
    }
    Ok(update)
}

fn delivery_ctx(args: &Args) -> Result<Value, ToolError> {
    match &args.delivery_ctx {
        Some(value) => validate_delivery_ctx(value).cloned(),
        None => Ok(json!({})),
    }
}

fn optional_delivery_ctx(args: &Args) -> Result<Option<Value>, ToolError> {
    let value = args
        .delivery_ctx
        .as_ref()
        .map(validate_delivery_ctx)
        .transpose()?;
    Ok(value.cloned())
}

fn validate_delivery_ctx(value: &Value) -> Result<&Value, ToolError> {
    if value.is_object() {
        Ok(value)
    } else {
        Err(ToolError::InvalidArgs(
            "delivery_ctx must be a JSON object".into(),
        ))
    }
}
