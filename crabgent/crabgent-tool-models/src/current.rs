//! Current model and override-state read operation.

use std::str::FromStr;

use crabgent_core::Action;
use crabgent_core::error::ToolError;
use crabgent_core::tool::ToolCtx;
use crabgent_store::SessionId;
use serde_json::Value;

use crate::args::Args;
use crate::output::current_model_to_json;
use crate::tool::{ModelRegistryTool, map_effort_override_store_error, map_override_store_error};

impl ModelRegistryTool {
    pub(crate) async fn do_current(&self, ctx: &ToolCtx, args: &Args) -> Result<Value, ToolError> {
        let target = current_session_target(args, ctx.session_id.as_deref())?;
        self.gate(
            ctx,
            &Action::ModelsCurrent {
                session_id: target.policy_session_id().map(ToOwned::to_owned),
            },
        )
        .await?;
        self.gate(
            ctx,
            &Action::ReasoningEffortCurrent {
                session_id: target.policy_session_id().map(ToOwned::to_owned),
            },
        )
        .await?;
        let current = ctx.current_model.as_ref().ok_or_else(|| {
            ToolError::Execution("models.current: current model context unavailable".to_owned())
        })?;
        let current_effort = ctx.current_effort.as_ref().ok_or_else(|| {
            ToolError::Execution(
                "models.current: current reasoning effort context unavailable".to_owned(),
            )
        })?;
        let (session_override, session_effort_override) = match target.lookup_session_id() {
            Some(id) => {
                let session = self.load_session(id, "models.current").await?;
                (session.model_override, session.reasoning_effort_override)
            }
            None => (None, None),
        };
        let global_override = self
            .global_model_store()
            .get_global_model_override()
            .await
            .map_err(|err| map_override_store_error("models.current", &err))?;
        let global_effort_override = self
            .global_effort_store()
            .get_global_reasoning_effort_override()
            .await
            .map_err(|err| map_effort_override_store_error("models.current", &err))?;
        Ok(current_model_to_json(
            current,
            *current_effort,
            session_override.as_deref(),
            global_override.as_ref(),
            session_effort_override,
            global_effort_override,
        ))
    }
}

struct CurrentSessionTarget {
    policy_target: Option<String>,
    lookup_target: Option<SessionId>,
}

impl CurrentSessionTarget {
    fn policy_session_id(&self) -> Option<&str> {
        self.policy_target.as_deref()
    }

    const fn lookup_session_id(&self) -> Option<&SessionId> {
        self.lookup_target.as_ref()
    }
}

/// Resolve `op=current` session handling. Explicit `args.session_id`
/// is policy-relevant and wins. Otherwise fall back to
/// `ToolCtx::session_id` only for the internal current-session lookup,
/// so policies can distinguish "my current session" from a targeted
/// read of another session id.
fn current_session_target(
    args: &Args,
    fallback: Option<&str>,
) -> Result<CurrentSessionTarget, ToolError> {
    if let Some(id) = &args.session_id {
        return Ok(CurrentSessionTarget {
            policy_target: Some(id.to_string()),
            lookup_target: Some(id.clone()),
        });
    }
    let Some(raw) = fallback else {
        return Ok(CurrentSessionTarget {
            policy_target: None,
            lookup_target: None,
        });
    };
    let session_id = SessionId::from_str(raw).map_err(|err| {
        ToolError::InvalidArgs(format!(
            "models.current: invalid session id from context: {err}"
        ))
    })?;
    Ok(CurrentSessionTarget {
        policy_target: None,
        lookup_target: Some(session_id),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn args(value: serde_json::Value) -> Args {
        serde_json::from_value(value).expect("valid current args")
    }

    #[test]
    fn current_target_without_session_has_no_policy_or_lookup_target() {
        let target = current_session_target(&args(json!({"op": "current"})), None)
            .expect("target without session");

        assert_eq!(target.policy_session_id(), None);
        assert_eq!(target.lookup_session_id(), None);
    }

    #[test]
    fn current_target_rejects_malformed_context_session_id() {
        let err = current_session_target(&args(json!({"op": "current"})), Some("not-a-uuid"))
            .err()
            .expect("invalid context session");

        assert!(
            matches!(err, ToolError::InvalidArgs(message) if message.contains("invalid session id from context"))
        );
    }
}
