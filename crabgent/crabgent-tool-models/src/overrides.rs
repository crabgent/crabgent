//! Override write operations for the model registry tool.

use crabgent_core::Action;
use crabgent_core::error::ToolError;
use crabgent_core::model::{ModelId, ReasoningEffort};
use crabgent_core::tool::ToolCtx;
use crabgent_store::SessionId;
use crabgent_store::records::Session;
use serde_json::{Value, json};

use crate::args::{Args, Op};
use crate::tool::{
    ModelRegistryTool, map_effort_override_store_error, map_override_store_error, map_store_error,
};

impl ModelRegistryTool {
    pub(crate) async fn do_set_session(
        &self,
        ctx: &ToolCtx,
        args: &Args,
    ) -> Result<Value, ToolError> {
        let session_id = args
            .session_id_or_else(ctx.session_id.as_deref())
            .map_err(ToolError::InvalidArgs)?;
        let model = args
            .required_model()
            .map_err(ToolError::InvalidArgs)?
            .clone();
        self.validate_override_model(Op::SetSession, &model)?;
        let action = Action::ModelsSetSessionOverride {
            session_id: session_id.to_string(),
            model: model.clone(),
        };
        self.write_session_override(
            ctx,
            &session_id,
            &action,
            "models.set_session",
            SessionOverrideMutation::SetModel(model),
        )
        .await
    }

    pub(crate) async fn do_clear_session(
        &self,
        ctx: &ToolCtx,
        args: &Args,
    ) -> Result<Value, ToolError> {
        let session_id = args
            .session_id_or_else(ctx.session_id.as_deref())
            .map_err(ToolError::InvalidArgs)?;
        let action = Action::ModelsClearSessionOverride {
            session_id: session_id.to_string(),
        };
        self.write_session_override(
            ctx,
            &session_id,
            &action,
            "models.clear_session",
            SessionOverrideMutation::ClearModel,
        )
        .await
    }

    pub(crate) async fn do_set_session_effort(
        &self,
        ctx: &ToolCtx,
        args: &Args,
    ) -> Result<Value, ToolError> {
        let session_id = args
            .session_id_or_else(ctx.session_id.as_deref())
            .map_err(ToolError::InvalidArgs)?;
        let effort = args
            .required_reasoning_effort()
            .map_err(ToolError::InvalidArgs)?;
        let action = Action::ReasoningEffortSetSessionOverride {
            session_id: session_id.to_string(),
            effort,
        };
        self.write_session_override(
            ctx,
            &session_id,
            &action,
            "models.set_session_effort",
            SessionOverrideMutation::SetReasoningEffort(effort),
        )
        .await
    }

    pub(crate) async fn do_clear_session_effort(
        &self,
        ctx: &ToolCtx,
        args: &Args,
    ) -> Result<Value, ToolError> {
        let session_id = args
            .session_id_or_else(ctx.session_id.as_deref())
            .map_err(ToolError::InvalidArgs)?;
        let action = Action::ReasoningEffortClearSessionOverride {
            session_id: session_id.to_string(),
        };
        self.write_session_override(
            ctx,
            &session_id,
            &action,
            "models.clear_session_effort",
            SessionOverrideMutation::ClearReasoningEffort,
        )
        .await
    }

    pub(crate) async fn do_set_global(
        &self,
        ctx: &ToolCtx,
        args: &Args,
    ) -> Result<Value, ToolError> {
        let model = args
            .required_model()
            .map_err(ToolError::InvalidArgs)?
            .clone();
        self.validate_override_model(Op::SetGlobal, &model)?;
        let action = Action::ModelsSetGlobalOverride {
            model: model.clone(),
        };
        self.write_global_model_override(
            ctx,
            &action,
            "models.set_global",
            GlobalModelMutation::Set(model),
        )
        .await
    }

    pub(crate) async fn do_clear_global(&self, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let action = Action::ModelsClearGlobalOverride;
        self.write_global_model_override(
            ctx,
            &action,
            "models.clear_global",
            GlobalModelMutation::Clear,
        )
        .await
    }

    pub(crate) async fn do_set_global_effort(
        &self,
        ctx: &ToolCtx,
        args: &Args,
    ) -> Result<Value, ToolError> {
        let effort = args
            .required_reasoning_effort()
            .map_err(ToolError::InvalidArgs)?;
        let action = Action::ReasoningEffortSetGlobalOverride { effort };
        self.write_global_effort_override(
            ctx,
            &action,
            "models.set_global_effort",
            GlobalEffortMutation::Set(effort),
        )
        .await
    }

    pub(crate) async fn do_clear_global_effort(&self, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let action = Action::ReasoningEffortClearGlobalOverride;
        self.write_global_effort_override(
            ctx,
            &action,
            "models.clear_global_effort",
            GlobalEffortMutation::Clear,
        )
        .await
    }

    async fn write_session_override(
        &self,
        ctx: &ToolCtx,
        session_id: &SessionId,
        action: &Action,
        context: &str,
        mutation: SessionOverrideMutation,
    ) -> Result<Value, ToolError> {
        self.gate(ctx, action).await?;
        let mut session = self.load_session(session_id, context).await?;
        mutation.apply(&mut session);
        self.session_store()
            .save(&session)
            .await
            .map_err(|err| map_store_error(context, &err))?;
        Ok(mutation.output(session_id))
    }

    async fn write_global_model_override(
        &self,
        ctx: &ToolCtx,
        action: &Action,
        context: &str,
        mutation: GlobalModelMutation,
    ) -> Result<Value, ToolError> {
        self.gate(ctx, action).await?;
        match &mutation {
            GlobalModelMutation::Set(model) => {
                self.global_model_store()
                    .set_global_model_override(model)
                    .await
                    .map_err(|err| map_override_store_error(context, &err))?;
            }
            GlobalModelMutation::Clear => {
                self.global_model_store()
                    .clear_global_model_override()
                    .await
                    .map_err(|err| map_override_store_error(context, &err))?;
            }
        }
        Ok(mutation.output())
    }

    async fn write_global_effort_override(
        &self,
        ctx: &ToolCtx,
        action: &Action,
        context: &str,
        mutation: GlobalEffortMutation,
    ) -> Result<Value, ToolError> {
        self.gate(ctx, action).await?;
        match mutation {
            GlobalEffortMutation::Set(effort) => {
                self.global_effort_store()
                    .set_global_reasoning_effort_override(effort)
                    .await
                    .map_err(|err| map_effort_override_store_error(context, &err))?;
                Ok(json!({ "reasoning_effort": effort.as_str() }))
            }
            GlobalEffortMutation::Clear => {
                self.global_effort_store()
                    .clear_global_reasoning_effort_override()
                    .await
                    .map_err(|err| map_effort_override_store_error(context, &err))?;
                Ok(json!({ "reasoning_effort": Value::Null }))
            }
        }
    }
}

enum SessionOverrideMutation {
    SetModel(ModelId),
    ClearModel,
    SetReasoningEffort(ReasoningEffort),
    ClearReasoningEffort,
}

impl SessionOverrideMutation {
    fn apply(&self, session: &mut Session) {
        match self {
            Self::SetModel(model) => session.model_override = Some(model.to_string()),
            Self::ClearModel => session.model_override = None,
            Self::SetReasoningEffort(effort) => session.reasoning_effort_override = Some(*effort),
            Self::ClearReasoningEffort => session.reasoning_effort_override = None,
        }
    }

    fn output(&self, session_id: &SessionId) -> Value {
        match self {
            Self::SetModel(model) => json!({
                "session_id": session_id.to_string(),
                "model": model.as_str(),
            }),
            Self::ClearModel => json!({
                "session_id": session_id.to_string(),
                "model": Value::Null,
            }),
            Self::SetReasoningEffort(effort) => json!({
                "session_id": session_id.to_string(),
                "reasoning_effort": effort.as_str(),
            }),
            Self::ClearReasoningEffort => json!({
                "session_id": session_id.to_string(),
                "reasoning_effort": Value::Null,
            }),
        }
    }
}

enum GlobalModelMutation {
    Set(ModelId),
    Clear,
}

impl GlobalModelMutation {
    fn output(&self) -> Value {
        match self {
            Self::Set(model) => json!({ "model": model.as_str() }),
            Self::Clear => json!({ "model": Value::Null }),
        }
    }
}

enum GlobalEffortMutation {
    Set(ReasoningEffort),
    Clear,
}
