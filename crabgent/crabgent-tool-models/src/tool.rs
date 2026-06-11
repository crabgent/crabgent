//! [`ModelRegistryTool`] implementation.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use crabgent_core::error::ToolError;
use crabgent_core::model::{
    GlobalModelOverrideStore, GlobalReasoningEffortOverrideStore, ModelId, ModelInfo,
    ModelOverrideStoreError, ModelRegistry, ReasoningEffortOverrideStoreError,
    validate_model_override,
};
use crabgent_core::tool::{Tool, ToolCtx, gate_tool_action, op_schema, parse_args_with_context};
use crabgent_core::{Action, Kernel, PolicyHook};
use crabgent_store::records::Session;
use crabgent_store::traits::SessionStore;
use crabgent_store::{SessionId, StoreError};
use serde_json::{Value, json};

use crate::args::{Args, Op};

const TOOL_NAME: &str = "models";

const DESCRIPTION: &str = "Inspect and configure model selection. Operations: \
    `list` (catalog overview, optional provider filter, capped at 200 \
    entries), `get` (full detail for one id), `current` (current resolved \
    model, reasoning effort, and override sources), `set_session`/`clear_session` \
    (session-scoped model override), `set_global`/`clear_global` (global \
    model override), `set_session_effort`/`clear_session_effort`, and \
    `set_global_effort`/`clear_global_effort`. Use list/get before changing \
    model overrides; model writes only accept registered ids. Pricing is \
    reported when known. For session ops you may omit `session_id` to target \
    the current session.";

/// LLM-facing model registry inspection tool.
pub struct ModelRegistryTool {
    models: ModelSource,
    policy: Arc<dyn PolicyHook>,
    store: Arc<dyn SessionStore>,
    global_store: Arc<dyn GlobalModelOverrideStore>,
    global_effort_store: Arc<dyn GlobalReasoningEffortOverrideStore>,
}

pub enum ModelSource {
    Kernel(Arc<OnceLock<Arc<Kernel>>>),
    #[cfg(test)]
    Registry(Arc<ModelRegistry>),
}

impl ModelSource {
    pub(crate) fn registry(&self) -> Result<&ModelRegistry, ToolError> {
        match self {
            Self::Kernel(kernel) => kernel.get().map(|kernel| kernel.models()).ok_or_else(|| {
                ToolError::Execution(
                    "models tool kernel registry is not initialised before execute".to_owned(),
                )
            }),
            #[cfg(test)]
            Self::Registry(registry) => Ok(registry),
        }
    }

    pub(crate) fn list(&self) -> Result<Vec<&ModelInfo>, ToolError> {
        Ok(self.registry()?.list().collect())
    }

    pub(crate) fn get(&self, id: &ModelId) -> Result<Option<&ModelInfo>, ToolError> {
        Ok(self.registry()?.get(id))
    }
}

impl ModelRegistryTool {
    /// Eager constructor. The kernel is already built when the tool is
    /// constructed.
    pub fn new(
        kernel: Arc<Kernel>,
        policy: Arc<dyn PolicyHook>,
        store: Arc<dyn SessionStore>,
        global_store: Arc<dyn GlobalModelOverrideStore>,
        global_effort_store: Arc<dyn GlobalReasoningEffortOverrideStore>,
    ) -> Self {
        let cell = Arc::new(OnceLock::from(kernel));
        Self {
            models: ModelSource::Kernel(cell),
            policy,
            store,
            global_store,
            global_effort_store,
        }
    }

    /// Late-binding constructor for self-referential kernels. Same wiring
    /// rationale as `TaskTool::new_lazy`: the tool can be embedded in the
    /// kernel it inspects. The cell must be populated with the built
    /// kernel before any `execute()` call.
    pub const fn new_lazy(
        kernel: Arc<OnceLock<Arc<Kernel>>>,
        policy: Arc<dyn PolicyHook>,
        store: Arc<dyn SessionStore>,
        global_store: Arc<dyn GlobalModelOverrideStore>,
        global_effort_store: Arc<dyn GlobalReasoningEffortOverrideStore>,
    ) -> Self {
        Self {
            models: ModelSource::Kernel(kernel),
            policy,
            store,
            global_store,
            global_effort_store,
        }
    }
}

#[async_trait]
impl Tool for ModelRegistryTool {
    fn name(&self) -> &'static str {
        TOOL_NAME
    }

    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        // Convention (matches crabgent-tool-cron/calendar/memory/consolidation):
        // top-level `required` lists only `op`; per-op required fields are
        // enforced by serde on the typed `Args` (see `Args::required_id`,
        // `required_model`, `required_reasoning_effort`) and advertised to the
        // LLM via the per-property "REQUIRED for op=..." descriptions below.
        // No JSON-schema `if/then` machine: no other workspace tool uses one,
        // and serde already rejects calls that omit a required field for an op.
        json!({
            "type": "object",
            "required": ["op"],
            "properties": {
                "op": op_schema::<Op>("Operation to perform."),
                "id": {
                    "type": ["string", "null"],
                    "description": "REQUIRED for op=get. Canonical id or alias of the model."
                },
                "session_id": {
                    "type": ["string", "null"],
                    "description": "Target session id. Optional: if omitted, the kernel-supplied current session is used. Pass an explicit id only to target a different session."
                },
                "model": {
                    "type": ["string", "null"],
                    "description": "REQUIRED for op=set_session/set_global. Must be a registered model id or alias."
                },
                "reasoning_effort": {
                    "type": ["string", "null"],
                    "enum": ["none", "low", "medium", "high", "xhigh", null],
                    "description": "REQUIRED for op=set_session_effort/set_global_effort."
                },
                "provider": {
                    "type": ["string", "null"],
                    "description": "Optional provider filter for op=list (exact match against ModelInfo.provider)."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: Args = parse_args_with_context(args, "models args")?;
        match parsed.op {
            Op::List => self.do_list(ctx, &parsed).await,
            Op::Get => self.do_get(ctx, &parsed).await,
            Op::Current => self.do_current(ctx, &parsed).await,
            Op::SetSession => self.do_set_session(ctx, &parsed).await,
            Op::ClearSession => self.do_clear_session(ctx, &parsed).await,
            Op::SetSessionEffort => self.do_set_session_effort(ctx, &parsed).await,
            Op::ClearSessionEffort => self.do_clear_session_effort(ctx, &parsed).await,
            Op::SetGlobal => self.do_set_global(ctx, &parsed).await,
            Op::ClearGlobal => self.do_clear_global(ctx).await,
            Op::SetGlobalEffort => self.do_set_global_effort(ctx, &parsed).await,
            Op::ClearGlobalEffort => self.do_clear_global_effort(ctx).await,
        }
    }
}

impl ModelRegistryTool {
    pub(crate) const fn model_source(&self) -> &ModelSource {
        &self.models
    }

    pub(crate) fn session_store(&self) -> &dyn SessionStore {
        self.store.as_ref()
    }

    pub(crate) fn global_model_store(&self) -> &dyn GlobalModelOverrideStore {
        self.global_store.as_ref()
    }

    pub(crate) fn global_effort_store(&self) -> &dyn GlobalReasoningEffortOverrideStore {
        self.global_effort_store.as_ref()
    }

    pub(crate) fn validate_override_model(&self, op: Op, model: &ModelId) -> Result<(), ToolError> {
        if validate_model_override(self.models.registry()?, op.as_str(), model).is_ok() {
            return Ok(());
        }
        Err(ToolError::InvalidArgs(format!(
            "models.{}: unknown model override '{model}'",
            op.as_str()
        )))
    }

    pub(crate) async fn load_session(
        &self,
        id: &SessionId,
        context: &str,
    ) -> Result<Session, ToolError> {
        self.store
            .load(id)
            .await
            .map_err(|err| map_store_error(context, &err))?
            .ok_or_else(|| ToolError::NotFound(format!("session: {id}")))
    }

    pub(crate) async fn gate(&self, ctx: &ToolCtx, action: &Action) -> Result<(), ToolError> {
        gate_tool_action(self.policy.as_ref(), ctx, action).await
    }
}

pub fn map_store_error(context: &str, err: &StoreError) -> ToolError {
    crabgent_log::warn!(
        op = %context,
        error_kind = err.kind(),
        transient = err.is_transient(),
        "models store unavailable"
    );
    ToolError::backend_unavailable(context, err)
}

pub fn map_override_store_error(context: &str, err: &ModelOverrideStoreError) -> ToolError {
    log_override_store_error(context, err);
    classify_override_store_error(context, err)
}

/// Only operator-level faults (store outage, or a future unmapped variant we
/// fail closed on) need an operator log. LLM-recoverable variants (`NotFound`,
/// bad session id) surface to the model as `ToolResult::soft_error`, so they
/// need no warn-level breadcrumb here.
fn log_override_store_error(context: &str, err: &ModelOverrideStoreError) {
    if !is_recoverable_override_error(err) {
        let error_kind = model_override_store_error_kind(err);
        crabgent_log::warn!(op = %context, error_kind, "models override store unavailable");
    }
}

/// Map the store error to its tool-facing variant. `Backend` and any future
/// non-exhaustive variant fail closed to an opaque operator fault, so no
/// backend detail leaks via `backend_unavailable`.
fn classify_override_store_error(context: &str, err: &ModelOverrideStoreError) -> ToolError {
    match err {
        ModelOverrideStoreError::NotFound { kind, id } => {
            ToolError::NotFound(format!("{context}: {kind} not found: {id}"))
        }
        ModelOverrideStoreError::InvalidSessionId(reason) => {
            ToolError::InvalidArgs(format!("{context}: invalid session id: {reason}"))
        }
        _ => ToolError::backend_unavailable(context, err),
    }
}

const fn is_recoverable_override_error(err: &ModelOverrideStoreError) -> bool {
    matches!(
        err,
        ModelOverrideStoreError::NotFound { .. } | ModelOverrideStoreError::InvalidSessionId(_)
    )
}

const fn model_override_store_error_kind(err: &ModelOverrideStoreError) -> &'static str {
    match err {
        ModelOverrideStoreError::Backend(_) => "backend",
        ModelOverrideStoreError::NotFound { .. } => "not_found",
        ModelOverrideStoreError::InvalidSessionId(_) => "invalid_session_id",
        // `ModelOverrideStoreError` is `#[non_exhaustive]`; future variants
        // fall back to a labelled kind until mapped explicitly above.
        _ => "unknown",
    }
}

pub fn map_effort_override_store_error(
    context: &str,
    err: &ReasoningEffortOverrideStoreError,
) -> ToolError {
    crabgent_log::warn!(
        op = %context,
        error_kind = reasoning_effort_override_store_error_kind(err),
        "models reasoning effort override store unavailable"
    );
    ToolError::backend_unavailable(context, err)
}

const fn reasoning_effort_override_store_error_kind(
    err: &ReasoningEffortOverrideStoreError,
) -> &'static str {
    match err {
        ReasoningEffortOverrideStoreError::Backend(_) => "backend",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use crabgent_core::policy::AllowAllPolicy;
    use crabgent_core::{Subject, Tool};
    use crabgent_store::memory::{MemoryGlobalOverrideStore, MemorySessionStore};

    use super::*;

    fn empty_registry_tool() -> ModelRegistryTool {
        ModelRegistryTool {
            models: ModelSource::Registry(Arc::new(ModelRegistry::new())),
            policy: Arc::new(AllowAllPolicy),
            store: Arc::new(MemorySessionStore::default()),
            global_store: Arc::new(MemoryGlobalOverrideStore::default()),
            global_effort_store: Arc::new(MemoryGlobalOverrideStore::default()),
        }
    }

    #[tokio::test]
    async fn list_empty_registry_returns_empty() {
        let result = empty_registry_tool()
            .execute(json!({"op": "list"}), &ToolCtx::new(Subject::new("alice")))
            .await
            .expect("list");

        assert_eq!(result["count"], 0);
        assert_eq!(result["total"], 0);
        assert_eq!(result["truncated"], false);
        assert_eq!(result["models"], json!([]));
    }

    #[test]
    fn parameters_schema_describes_model_tool_inputs() {
        let schema = empty_registry_tool().parameters_schema();

        assert_eq!(schema["required"], json!(["op"]));
        assert!(schema["properties"]["op"].is_object());
        assert_eq!(
            schema["properties"]["reasoning_effort"]["enum"],
            json!(["none", "low", "medium", "high", "xhigh", null])
        );
        assert!(schema["properties"].get("provider").is_some());
    }

    #[tokio::test]
    async fn lazy_tool_errors_when_kernel_cell_is_uninitialised() {
        let global_store: Arc<MemoryGlobalOverrideStore> =
            Arc::new(MemoryGlobalOverrideStore::default());
        let tool = ModelRegistryTool::new_lazy(
            Arc::new(OnceLock::new()),
            Arc::new(AllowAllPolicy),
            Arc::new(MemorySessionStore::default()),
            global_store.clone(),
            global_store,
        );

        let err = tool
            .execute(json!({"op": "list"}), &ToolCtx::new(Subject::new("alice")))
            .await
            .expect_err("uninitialised kernel cell");

        assert!(
            matches!(err, ToolError::Execution(message) if message.contains("kernel registry is not initialised"))
        );
    }

    #[test]
    fn backend_error_mappers_return_opaque_tool_errors() {
        let store_err = StoreError::backend("dsn=secret");
        let model_err = ModelOverrideStoreError::backend("dsn=secret");
        let effort_err = ReasoningEffortOverrideStoreError::backend("dsn=secret");

        assert_opaque_backend_error(map_store_error("models.store", &store_err));
        assert_opaque_backend_error(map_override_store_error("models.global", &model_err));
        assert_opaque_backend_error(map_effort_override_store_error(
            "models.effort",
            &effort_err,
        ));
    }

    fn assert_opaque_backend_error(err: ToolError) {
        let ToolError::Execution(message) = err else {
            panic!("expected execution error");
        };
        assert!(message.contains("backend unavailable"));
        assert!(!message.contains("secret"));
    }

    #[test]
    fn override_not_found_maps_to_recoverable_not_found() {
        let err = ModelOverrideStoreError::NotFound {
            kind: "session",
            id: "missing-session".to_owned(),
        };

        let mapped = map_override_store_error("models.set_session", &err);

        // Must NOT be an infrastructure (Execution) fault: a missing target is
        // LLM-recoverable and soft-mapped by the run loop.
        assert!(
            matches!(&mapped, ToolError::NotFound(message) if message.contains("missing-session")),
            "expected NotFound, got {mapped:?}"
        );
        assert!(!matches!(mapped, ToolError::Execution(_)));
        assert_eq!(model_override_store_error_kind(&err), "not_found");
    }

    #[test]
    fn override_invalid_session_id_maps_to_invalid_args() {
        let err = ModelOverrideStoreError::InvalidSessionId("not-a-uuid".to_owned());

        let mapped = map_override_store_error("models.clear_session", &err);

        assert!(
            matches!(&mapped, ToolError::InvalidArgs(message) if message.contains("not-a-uuid")),
            "expected InvalidArgs, got {mapped:?}"
        );
        assert!(!matches!(mapped, ToolError::Execution(_)));
        assert_eq!(model_override_store_error_kind(&err), "invalid_session_id");
    }

    #[test]
    fn override_backend_error_kind_is_backend() {
        let err = ModelOverrideStoreError::backend("dsn=secret");

        assert_eq!(model_override_store_error_kind(&err), "backend");
    }
}
