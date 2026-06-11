//! [`ConsolidationTool`] implementation.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::tool::{Tool, ToolCtx, parse_args_with_context};
use crabgent_memory_consolidation::{ConsolidationError, ConsolidationResult, ConsolidationRunner};
use serde_json::{Value, json};

use crate::args::{Args, Op};

const TOOL_NAME: &str = "consolidate_memory";

const DESCRIPTION: &str = "Run memory consolidation or inspect its checkpoint status. \
    Operations: `run` starts the configured consolidation pipeline for the supplied \
    memory scope; `status` reads the latest consolidation checkpoint for that scope.";

pub struct ConsolidationTool {
    runner: Arc<ConsolidationRunner>,
}

impl ConsolidationTool {
    pub const fn new(runner: Arc<ConsolidationRunner>) -> Self {
        Self { runner }
    }

    async fn run(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let token = ctx.cancel.clone().unwrap_or_default();
        let result = self
            .runner
            .run(&ctx.subject, args.scope.clone(), token)
            .await
            .map_err(consolidation_error)?;
        Ok(run_output(&args.scope, &result))
    }

    async fn status(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let Some(checkpoint) = self
            .runner
            .status(&ctx.subject, args.scope.clone())
            .await
            .map_err(consolidation_error)?
        else {
            return Ok(status_missing_output());
        };
        Ok(json!({
            "has_checkpoint": true,
            "last_run_at": checkpoint.last_run_at,
            "in_progress": checkpoint.in_progress,
            "sessions_processed_total": checkpoint.sessions_processed
        }))
    }
}

#[async_trait]
impl Tool for ConsolidationTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["op", "scope"],
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["run", "status"],
                    "description": "Operation to perform."
                },
                "scope": crabgent_core::tool::memory_scope_schema()
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: Args = parse_args_with_context(args, "consolidation args")?;
        match parsed.op {
            Op::Run => self.run(&parsed, ctx).await,
            Op::Status => self.status(&parsed, ctx).await,
        }
    }
}

fn status_missing_output() -> Value {
    json!({
        "has_checkpoint": false,
        "last_run_at": null,
        "in_progress": false,
        "sessions_processed_total": 0
    })
}

fn run_output(scope: &crabgent_core::MemoryScope, result: &ConsolidationResult) -> Value {
    json!({
        "scope": scope,
        "stats": {
            "sessions_processed": result.sessions_processed,
            "facts_extracted": result.facts_extracted,
            "memories_created": result.memories_created,
            "memories_updated": result.memories_updated,
            "conflicts_detected": result.conflicts_detected,
            "stale_archived": result.stale_archived
        }
    })
}

fn consolidation_error(err: ConsolidationError) -> ToolError {
    let mapped = map_consolidation_error(&err);
    drop(err);
    mapped
}

fn map_consolidation_error(err: &ConsolidationError) -> ToolError {
    if let Some(error) = immediate_consolidation_error(err) {
        return error;
    }
    runtime_consolidation_error(err)
}

fn immediate_consolidation_error(err: &ConsolidationError) -> Option<ToolError> {
    match err {
        ConsolidationError::Cancelled => Some(ToolError::Cancelled),
        ConsolidationError::Denied(msg) => Some(ToolError::Permission(msg.clone())),
        ConsolidationError::AlreadyRunning(_) => Some(ToolError::Execution(
            "consolidation: already running".into(),
        )),
        _ => None,
    }
}

fn runtime_consolidation_error(err: &ConsolidationError) -> ToolError {
    match err {
        ConsolidationError::Store(err) => store_consolidation_error(err),
        ConsolidationError::Provider(_) => provider_consolidation_error(),
        ConsolidationError::SubjectResolver(_) => subject_resolver_consolidation_error(),
        _ => unknown_consolidation_error(),
    }
}

fn store_consolidation_error(err: &crabgent_store::StoreError) -> ToolError {
    crabgent_log::warn!(
        error_kind = err.kind(),
        transient = err.is_transient(),
        "consolidation store unavailable"
    );
    ToolError::Execution("consolidation: store unavailable".into())
}

fn provider_consolidation_error() -> ToolError {
    crabgent_log::warn!(
        error_kind = "provider",
        "consolidation provider unavailable"
    );
    ToolError::Execution("consolidation: provider unavailable".into())
}

fn subject_resolver_consolidation_error() -> ToolError {
    crabgent_log::warn!(error_kind = "subject_resolver", "consolidation failed");
    ToolError::Execution("consolidation: error".into())
}

fn unknown_consolidation_error() -> ToolError {
    crabgent_log::warn!(error_kind = "unknown", "consolidation failed");
    ToolError::Execution("consolidation: error".into())
}
