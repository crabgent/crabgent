//! [`TaskTool`] implementation.

use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use crabgent_core::model::{ModelTarget, ReasoningEffort, ResolvedSource};
use crabgent_core::tool::{
    Tool, ToolCtx, clamp_positive_limit, gate_tool_action, parse_args_with_context,
};
use crabgent_core::{
    Action, DEFAULT_SEARCH_LIMIT, Kernel, MAX_SEARCH_LIMIT, PolicyHook, ToolAccess, ToolError,
    ToolResult,
};
use crabgent_store::{Owner, Page, SessionId, StoreError, Task, TaskId, TaskStore};
use crabgent_task::{TaskExecutor, TaskRequest};
use serde_json::{Value, json};

use crate::args::{Args, ContextInjection, Op, ReasoningEffortArg};
use crate::depth::{DepthCheckError, ensure_depth_allowed};
use crate::{blocking, output};

pub const TOOL_NAME: &str = "task";
const DEPTH_DENIED: &str = "task.create denied: nested task depth limit reached";
const PARALLEL_DENIED: &str = "task.create denied: max parallel tasks reached";

const DESCRIPTION: &str = "Manage background tasks. Operations: `create`, \
    `list`, `get`, `cancel`. create can return immediately or block until the \
    task reaches a terminal status. If create omits `model`, the spawned task \
    inherits the current resolved model. If create omits `reasoning_effort`, \
    the task snapshots the current resolved effort only when the task model \
    advertises reasoning support. `reasoning_effort: null` clears inherited \
    effort. create can limit spawned task tools with `tool_access`. All \
    operations are policy-gated.";

pub struct TaskTool<S: TaskStore + 'static> {
    executor: Arc<TaskExecutor<S>>,
    kernel: Arc<OnceLock<Arc<Kernel>>>,
    store: Arc<S>,
    policy: Arc<dyn PolicyHook>,
}

impl<S: TaskStore + 'static> TaskTool<S> {
    /// Eager constructor: the spawn-target kernel is known at construction
    /// time. Tests and simple deployments use it.
    pub fn new(
        executor: Arc<TaskExecutor<S>>,
        kernel: Arc<Kernel>,
        store: Arc<S>,
        policy: Arc<dyn PolicyHook>,
    ) -> Self {
        let cell = Arc::new(OnceLock::from(kernel));
        Self {
            executor,
            kernel: cell,
            store,
            policy,
        }
    }

    /// Late-binding constructor for self-referential kernels: the tool can
    /// be embedded in the same kernel it spawns into, enabling true runtime
    /// nesting (sub-tasks themselves call `task.create`). Depth is capped
    /// by `TaskExecutor::max_depth` and the parent-chain walker, so
    /// recursion is bounded.
    ///
    /// The cell MUST be populated with the built kernel before any
    /// `execute()` call, otherwise `task.create` returns `ToolError::Execution`.
    /// Typical wiring:
    ///   1. `let cell = Arc::new(OnceLock::new());`
    ///   2. `let task_tool = TaskTool::new_lazy(executor, cell.clone(), store, policy);`
    ///   3. `let kernel = KernelBuilder::new()...add_tool(task_tool)...build();`
    ///   4. `cell.set(Arc::new(kernel)).ok();`
    pub const fn new_lazy(
        executor: Arc<TaskExecutor<S>>,
        kernel: Arc<OnceLock<Arc<Kernel>>>,
        store: Arc<S>,
        policy: Arc<dyn PolicyHook>,
    ) -> Self {
        Self {
            executor,
            kernel,
            store,
            policy,
        }
    }

    fn kernel(&self) -> Result<Arc<Kernel>, ToolError> {
        self.kernel.get().map(Arc::clone).ok_or_else(|| {
            crabgent_log::warn!("task tool kernel cell not initialised before execute");
            ToolError::Execution("task.create: kernel not initialised".to_owned())
        })
    }
}

#[async_trait]
impl<S> Tool for TaskTool<S>
where
    S: TaskStore + 'static,
{
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
                    "enum": ["create", "list", "get", "cancel"],
                    "description": "Operation to perform."
                },
                "task_id": {"type": "string", "description": "Task id for op=get/cancel."},
                "owner": {"type": ["string", "null"], "description": "Owner scope for create/list/get/cancel."},
                "name": {"type": "string", "description": "Short display name for op=create."},
                "prompt": {"type": "string", "description": "Task prompt for op=create."},
                "model": {
                    "description": "Optional explicit model target for op=create (e.g. 'claude-sonnet-4-6'). Omit to inherit the current resolved model.",
                    "oneOf": [
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
                "reasoning_effort": {
                    "type": ["string", "null"],
                    "enum": ["none", "low", "medium", "high", "xhigh", null],
                    "description": "Optional explicit reasoning effort for op=create. Omit to inherit the current resolved effort when the task model supports it. Set null to clear inherited effort. Not every model accepts every level (e.g. xhigh needs gpt-5.2+)."
                },
                "tool_access": {
                    "type": ["object", "null"],
                    "description": "Optional tool access for op=create. Omit for all registered tools. Use mode=none for no tools, or mode=only with tool names.",
                    "required": ["mode"],
                    "properties": {
                        "mode": {"type": "string", "enum": ["all", "none", "only"]},
                        "tools": {
                            "type": "array",
                            "items": {"type": "string"}
                        }
                    }
                },
                "name": {"type": ["string", "null"], "description": "Optional UI label. Not persisted in V1."},
                "block": {"type": "boolean", "default": false},
                "timeout_secs": {"type": ["integer", "null"], "minimum": 1},
                "context": {
                    "type": ["object", "null"],
                    "properties": {
                        "context_messages": {"type": ["array", "null"], "items": {"type": "object"}},
                        "parent_session_id": {
                            "type": ["string", "null"],
                            "description": "Opaque kernel SessionId (UUIDv7 string). Leave null unless you have a valid SessionId from a prior task.get/task.list response. Channel/thread identifiers are NOT valid here."
                        },
                        "context_mode": {"type": ["string", "null"]},
                        "system_prompt": {"type": ["string", "null"]}
                    }
                },
                "limit": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_LIMIT},
                "offset": {"type": "integer", "minimum": 0}
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: Args = parse_args_with_context(args, "task args")?;
        match parsed.op {
            Op::Create => self.do_create(&parsed, ctx).await,
            Op::List => self.do_list(&parsed, ctx).await,
            Op::Get => self.do_get(&parsed, ctx).await,
            Op::Cancel => self.do_cancel(&parsed, ctx).await,
        }
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        match self.execute(args, ctx).await {
            Ok(output) => Ok(ToolResult::success(output)),
            Err(ToolError::NotFound(reason)) => {
                Ok(ToolResult::soft_error(json!({ "error": reason })))
            }
            Err(err) => Err(err),
        }
    }
}

impl<S> TaskTool<S>
where
    S: TaskStore + 'static,
{
    async fn do_create(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let owner = task_owner(args, ctx);
        self.gate(
            &Action::TaskCreate {
                owner: Some(owner.clone()),
            },
            ctx,
        )
        .await?;
        if self.executor.max_parallel() == 0 {
            return Err(ToolError::Permission(PARALLEL_DENIED.to_owned()));
        }
        let parent_task_id = parse_parent_task_id(ctx);
        ensure_depth_allowed(
            &*self.store,
            parent_task_id.as_ref(),
            self.executor.max_depth(),
        )
        .await
        .map_err(map_depth_error)?;
        let kernel = self.kernel()?;
        let req = build_request(args, owner, parent_task_id, ctx, &kernel)?
            .with_subject(ctx.subject.clone());
        validate_tool_access(&req.tool_access, &kernel)?;
        if args.block {
            let task = self
                .executor
                .spawn_blocking(kernel, req, blocking::timeout(args.timeout_secs))
                .await
                .map_err(|err| backend_unavailable("task.create", &err))?;
            return Ok(output::create_terminal(&task));
        }
        let id = self
            .executor
            .spawn(kernel, req)
            .await
            .map_err(|err| backend_unavailable("task.create", &err))?;
        Ok(output::create_running(&id))
    }

    async fn do_list(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let owner = args.owner();
        self.gate(
            &Action::TaskList {
                owner: owner.clone(),
            },
            ctx,
        )
        .await?;
        let tasks = self
            .store
            .list_by_owner(owner.as_ref(), page(args)?)
            .await
            .map_err(|err| store_unavailable("list", &err))?;
        Ok(output::list(&tasks))
    }

    async fn do_get(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let id = parse_task_id(args, "get")?;
        let requested_owner = args.owner();
        self.gate(
            &Action::TaskGet {
                id: id.to_string(),
                owner: requested_owner.clone(),
            },
            ctx,
        )
        .await?;
        let task = self
            .load_visible_required(&id, requested_owner.as_ref(), "get")
            .await?;
        self.gate(
            &Action::TaskGet {
                id: task.id.to_string(),
                owner: Some(task.owner.clone()),
            },
            ctx,
        )
        .await?;
        Ok(output::get(&task))
    }

    async fn do_cancel(&self, args: &Args, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let id = parse_task_id(args, "cancel")?;
        let requested_owner = args.owner();
        self.gate(
            &Action::TaskCancel {
                id: id.to_string(),
                owner: requested_owner.clone(),
            },
            ctx,
        )
        .await?;
        let task = self
            .load_visible_required(&id, requested_owner.as_ref(), "cancel")
            .await?;
        self.gate(
            &Action::TaskCancel {
                id: task.id.to_string(),
                owner: Some(task.owner.clone()),
            },
            ctx,
        )
        .await?;
        let cancelled = self.executor.cancel(&task.id).await;
        let status = if cancelled {
            let wait_for = self.executor.shutdown_grace() + Duration::from_millis(50);
            blocking::wait_after_cancel_for(&*self.store, &task.id, wait_for)
                .await?
                .status
        } else {
            task.status
        };
        Ok(output::cancel(&task.id, cancelled, status))
    }

    async fn load_visible_required(
        &self,
        id: &TaskId,
        owner: Option<&Owner>,
        op: &str,
    ) -> Result<Task, ToolError> {
        self.store
            .get(id)
            .await
            .map_err(|err| store_unavailable(op, &err))?
            .filter(|task| owner.is_none_or(|owner| task.owner == *owner))
            .ok_or_else(|| ToolError::NotFound(format!("task {id}")))
    }

    async fn gate(&self, action: &Action, ctx: &ToolCtx) -> Result<(), ToolError> {
        gate_tool_action(self.policy.as_ref(), ctx, action).await
    }
}

pub(crate) fn store_unavailable(op: &str, err: &StoreError) -> ToolError {
    let op = format!("task.{op}");
    crabgent_log::warn!(
        op = %op,
        error_kind = err.kind(),
        transient = err.is_transient(),
        "task store unavailable",
    );
    ToolError::backend_unavailable(op, err)
}

fn backend_unavailable(
    op: &str,
    err: &(impl std::fmt::Debug + std::fmt::Display + ?Sized),
) -> ToolError {
    crabgent_log::warn!(
        op = %op,
        error_kind = "executor",
        "task backend unavailable",
    );
    ToolError::backend_unavailable(op, err)
}

fn build_request(
    args: &Args,
    owner: Owner,
    parent_task_id: Option<TaskId>,
    ctx: &ToolCtx,
    kernel: &Kernel,
) -> Result<TaskRequest, ToolError> {
    let prompt = args.required_prompt().map_err(ToolError::InvalidArgs)?;
    let mut req = match args.model.clone() {
        Some(model) => TaskRequest::try_new(owner, model, prompt)
            .map_err(|err| ToolError::InvalidArgs(format!("task.create: {err}")))?,
        None => request_from_current_model(owner, prompt, ctx)?,
    };
    if let Some(parent) = parent_task_id {
        req = req.with_parent_task(parent);
    }
    if let Some(name) = args.name.as_ref().filter(|name| !name.trim().is_empty()) {
        req = req.with_name(name.trim().to_owned());
    }
    if let Some(context) = &args.context {
        req = apply_context(req, context)?;
    }
    if let Some(access) = &args.tool_access {
        req = req.with_tool_access(access.clone());
    }
    req = apply_reasoning_effort(req, args.reasoning_effort, ctx, kernel)?;
    Ok(req)
}

fn apply_reasoning_effort(
    req: TaskRequest,
    arg: ReasoningEffortArg,
    ctx: &ToolCtx,
    kernel: &Kernel,
) -> Result<TaskRequest, ToolError> {
    match arg {
        ReasoningEffortArg::Clear | ReasoningEffortArg::Set(ReasoningEffort::Disabled) => {
            Ok(req.with_reasoning_effort(ReasoningEffort::Disabled))
        }
        ReasoningEffortArg::Set(effort) => {
            if !task_model_supports_reasoning(&req, kernel) {
                return Err(ToolError::InvalidArgs(format!(
                    "task.create: model '{}' does not support reasoning_effort",
                    task_model_target(&req)
                )));
            }
            Ok(req.with_reasoning_effort(effort))
        }
        ReasoningEffortArg::Inherit => {
            if !task_model_supports_reasoning(&req, kernel) {
                return Ok(req);
            }
            if let Some(effort) = ctx
                .current_effort
                .as_ref()
                .and_then(|current| current.effort)
            {
                return Ok(req.with_reasoning_effort(effort));
            }
            Ok(req)
        }
    }
}

fn task_model_supports_reasoning(req: &TaskRequest, kernel: &Kernel) -> bool {
    kernel
        .models()
        .resolve(task_model_target(req))
        .is_ok_and(|info| info.caps.reasoning_effort.is_some())
}

fn task_model_target(req: &TaskRequest) -> &ModelTarget {
    req.explicit_model.as_ref().unwrap_or(&req.model)
}

fn validate_tool_access(access: &ToolAccess, kernel: &Kernel) -> Result<(), ToolError> {
    match access {
        ToolAccess::All | ToolAccess::None => Ok(()),
        ToolAccess::Only { tools } => {
            if tools.is_empty() {
                return Err(ToolError::InvalidArgs(
                    "task.create: tool_access.only requires at least one tool; use mode=none for no tools"
                        .to_owned(),
                ));
            }
            for name in tools {
                if name.trim() != name || name.is_empty() {
                    return Err(ToolError::InvalidArgs(
                        "task.create: tool_access contains an invalid tool name".to_owned(),
                    ));
                }
                if kernel.tool(name).is_none() {
                    return Err(ToolError::InvalidArgs(format!(
                        "task.create: unknown tool '{name}' in tool_access"
                    )));
                }
            }
            Ok(())
        }
    }
}

fn request_from_current_model(
    owner: Owner,
    prompt: String,
    ctx: &ToolCtx,
) -> Result<TaskRequest, ToolError> {
    let current = ctx.current_model.as_ref().ok_or_else(|| {
        ToolError::InvalidArgs(
            "task.create: missing field 'model' and current model context unavailable".to_owned(),
        )
    })?;
    let mut req = TaskRequest::try_new_default(
        owner,
        ModelTarget::new(current.info.provider.clone(), current.info.id.clone()),
        prompt,
    )
    .map_err(|err| ToolError::InvalidArgs(format!("task.create: {err}")))?;
    if current.source == ResolvedSource::SessionOverride {
        req = req.with_session_model_override(current.info.id.clone());
    }
    Ok(req)
}

fn map_depth_error(err: DepthCheckError) -> ToolError {
    match err {
        DepthCheckError::Limit(_) => ToolError::Permission(DEPTH_DENIED.to_owned()),
        DepthCheckError::Store(err) => store_unavailable("create", &err),
    }
}

fn task_owner(args: &Args, ctx: &ToolCtx) -> Owner {
    args.owner()
        .unwrap_or_else(|| Owner::new(ctx.subject.id().to_owned()))
}

fn parse_parent_task_id(ctx: &ToolCtx) -> Option<TaskId> {
    ctx.subject
        .attr(crabgent_task::TASK_ID_ATTR)
        .and_then(|value| TaskId::from_str(value).ok())
}

fn parse_task_id(args: &Args, op: &str) -> Result<TaskId, ToolError> {
    let id = args.required_task_id().map_err(ToolError::InvalidArgs)?;
    TaskId::from_str(id).map_err(|err| ToolError::InvalidArgs(format!("task.{op}: {err}")))
}

fn page(args: &Args) -> Result<Page, ToolError> {
    let limit = args.limit.map_or(Ok(DEFAULT_SEARCH_LIMIT), |limit| {
        clamp_positive_limit(limit, MAX_SEARCH_LIMIT, "task.list")
    })?;
    let offset = args.offset.unwrap_or(0);
    // `limit` (clamped to `MAX_SEARCH_LIMIT`) and `offset` are `u32`; widening
    // to `usize` is lossless on every supported (>= 32-bit) target.
    Ok(Page::new(limit as usize, offset as usize))
}

fn apply_context(
    mut req: TaskRequest,
    context: &ContextInjection,
) -> Result<TaskRequest, ToolError> {
    if let Some(messages) = &context.context_messages {
        req = req.with_messages(messages.clone());
    }
    if let Some(session_id) = &context.parent_session_id {
        let id = SessionId::from_str(session_id)
            .map_err(|err| ToolError::InvalidArgs(format!("parent_session_id: {err}")))?;
        req = req.with_parent_session(id);
    }
    if let Some(mode) = &context.context_mode {
        req = req.with_context_mode(mode.clone());
    }
    if let Some(prompt) = &context.system_prompt {
        req = req.with_system_prompt(prompt.clone());
    }
    Ok(req)
}
