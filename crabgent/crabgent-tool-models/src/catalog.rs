//! Catalog read operations for the model registry tool.

use crabgent_core::Action;
use crabgent_core::error::ToolError;
use crabgent_core::tool::ToolCtx;
use serde_json::{Value, json};

use crate::args::Args;
use crate::output::{MAX_LIST_MODELS, model_info_to_json};
use crate::tool::ModelRegistryTool;

impl ModelRegistryTool {
    pub(crate) async fn do_list(&self, ctx: &ToolCtx, args: &Args) -> Result<Value, ToolError> {
        self.gate(ctx, &Action::ModelList).await?;
        let mut all = self.model_source().list()?;
        all.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        let filtered: Vec<_> = match args.provider.as_deref() {
            Some(provider) => all
                .into_iter()
                .filter(|model| model.provider == provider)
                .collect(),
            None => all,
        };
        let total = filtered.len();
        let count = total.min(MAX_LIST_MODELS);
        let truncated = total > MAX_LIST_MODELS;
        let models: Vec<Value> = filtered
            .into_iter()
            .take(MAX_LIST_MODELS)
            .map(model_info_to_json)
            .collect();
        Ok(json!({
            "count": count,
            "total": total,
            "truncated": truncated,
            "models": models,
        }))
    }

    pub(crate) async fn do_get(&self, ctx: &ToolCtx, args: &Args) -> Result<Value, ToolError> {
        let id = args.required_id().map_err(ToolError::InvalidArgs)?;
        // Policy sees the user-supplied id or alias before registry lookup resolves it.
        self.gate(ctx, &Action::ModelGet { id: id.clone() }).await?;
        let Some(info) = self.model_source().get(id)? else {
            return Err(ToolError::NotFound(format!("model: {id}")));
        };
        Ok(json!({ "model": model_info_to_json(info) }))
    }
}
