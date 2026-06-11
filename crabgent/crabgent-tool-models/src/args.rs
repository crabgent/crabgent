//! Argument parsing for the model registry tool.

use std::str::FromStr;

use crabgent_core::{ModelId, ReasoningEffort};
use crabgent_store::SessionId;
use serde::{Deserialize, Deserializer};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    List,
    Get,
    Current,
    SetSession,
    ClearSession,
    SetSessionEffort,
    ClearSessionEffort,
    SetGlobal,
    ClearGlobal,
    SetGlobalEffort,
    ClearGlobalEffort,
}

impl Op {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Get => "get",
            Self::Current => "current",
            Self::SetSession => "set_session",
            Self::ClearSession => "clear_session",
            Self::SetSessionEffort => "set_session_effort",
            Self::ClearSessionEffort => "clear_session_effort",
            Self::SetGlobal => "set_global",
            Self::ClearGlobal => "clear_global",
            Self::SetGlobalEffort => "set_global_effort",
            Self::ClearGlobalEffort => "clear_global_effort",
        }
    }
}

impl crabgent_core::tool::ToolOp for Op {
    const JSON_VALUES: &'static [&'static str] = &[
        "list",
        "get",
        "current",
        "set_session",
        "clear_session",
        "set_session_effort",
        "clear_session_effort",
        "set_global",
        "clear_global",
        "set_global_effort",
        "clear_global_effort",
    ];

    fn as_str(self) -> &'static str {
        Self::as_str(self)
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Args {
    pub op: Op,
    #[serde(default, deserialize_with = "model_id_option")]
    pub id: Option<ModelId>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    #[serde(default, deserialize_with = "model_id_option")]
    pub model: Option<ModelId>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl Args {
    pub fn required_id(&self) -> Result<&ModelId, String> {
        self.id
            .as_ref()
            .ok_or_else(|| "models.get: missing required field 'id'".to_owned())
    }

    /// Resolve the target session id for an op that requires one.
    ///
    /// Returns the explicit `args.session_id` when present. Otherwise
    /// falls back to `fallback`, owned to keep the return type stable
    /// regardless of source. The fallback is intended to be
    /// `ToolCtx::session_id` parsed into a `SessionId`, so that LLM
    /// callers without self-knowledge can still target the current
    /// session.
    pub fn session_id_or_else(&self, fallback: Option<&str>) -> Result<SessionId, String> {
        if let Some(id) = self.session_id.clone() {
            return Ok(id);
        }
        match fallback {
            Some(raw) => SessionId::from_str(raw).map_err(|err| {
                format!(
                    "models.{}: invalid session id from context: {err}",
                    self.op.as_str()
                )
            }),
            None => Err(format!(
                "models.{}: missing required field 'session_id' (no current session in context)",
                self.op.as_str()
            )),
        }
    }

    pub fn required_model(&self) -> Result<&ModelId, String> {
        self.model.as_ref().ok_or_else(|| {
            format!(
                "models.{}: missing required field 'model'",
                self.op.as_str()
            )
        })
    }

    pub fn required_reasoning_effort(&self) -> Result<ReasoningEffort, String> {
        self.reasoning_effort.ok_or_else(|| {
            format!(
                "models.{}: missing required field 'reasoning_effort'",
                self.op.as_str()
            )
        })
    }
}

fn model_id_option<'de, D>(deserializer: D) -> Result<Option<ModelId>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|id| id.map(ModelId::new))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn args_op_serde_snake_case() {
        let list: Args = serde_json::from_value(json!({"op": "list"})).expect("list args");
        let get: Args = serde_json::from_value(json!({"op": "get"})).expect("get args");
        let current: Args = serde_json::from_value(json!({"op": "current"})).expect("current args");

        assert_eq!(list.op, Op::List);
        assert_eq!(get.op, Op::Get);
        assert_eq!(current.op, Op::Current);
    }

    #[test]
    fn args_get_missing_id_returns_error() {
        let args: Args = serde_json::from_value(json!({"op": "get"})).expect("get args");

        let err = args.required_id().expect_err("missing id");

        assert!(err.contains("missing required field 'id'"));
    }

    #[test]
    fn args_list_accepts_provider_filter() {
        let args: Args = serde_json::from_value(json!({"op": "list", "provider": "anthropic"}))
            .expect("list args");

        assert_eq!(args.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn args_list_without_provider_accepted() {
        let args: Args = serde_json::from_value(json!({"op": "list"})).expect("list args");

        assert_eq!(args.provider, None);
    }

    #[test]
    fn args_session_and_model_are_typed() {
        let session_id = SessionId::new();
        let args: Args = serde_json::from_value(json!({
            "op": "set_session",
            "session_id": session_id.to_string(),
            "model": " sonnet "
        }))
        .expect("set_session args");

        assert_eq!(args.op, Op::SetSession);
        assert_eq!(
            args.session_id_or_else(None).expect("test result"),
            session_id
        );
        assert_eq!(
            args.required_model().expect("test result").as_str(),
            "sonnet"
        );
    }

    #[test]
    fn args_session_id_or_else_uses_explicit_when_present() {
        let explicit = SessionId::new();
        let fallback = SessionId::new();
        let args: Args = serde_json::from_value(json!({
            "op": "set_session",
            "session_id": explicit.to_string(),
            "model": "sonnet"
        }))
        .expect("set_session args");

        assert_eq!(
            args.session_id_or_else(Some(&fallback.to_string()))
                .expect("test result"),
            explicit
        );
    }

    #[test]
    fn args_session_id_or_else_uses_fallback_when_missing() {
        let fallback = SessionId::new();
        let args: Args = serde_json::from_value(json!({
            "op": "set_session",
            "model": "sonnet"
        }))
        .expect("set_session args");

        assert_eq!(
            args.session_id_or_else(Some(&fallback.to_string()))
                .expect("test result"),
            fallback
        );
    }

    #[test]
    fn args_session_id_or_else_errors_when_no_explicit_and_no_fallback() {
        let args: Args = serde_json::from_value(json!({
            "op": "clear_session"
        }))
        .expect("clear_session args");

        let err = args
            .session_id_or_else(None)
            .expect_err("missing session id without fallback");

        assert!(err.contains("models.clear_session"));
        assert!(err.contains("no current session in context"));
    }

    #[test]
    fn args_session_id_or_else_rejects_malformed_fallback() {
        let args: Args = serde_json::from_value(json!({
            "op": "set_session",
            "model": "sonnet"
        }))
        .expect("set_session args");

        let err = args
            .session_id_or_else(Some("not-a-uuid"))
            .expect_err("invalid fallback");

        assert!(err.contains("models.set_session"));
        assert!(err.contains("invalid session id from context"));
    }

    #[test]
    fn args_set_global_missing_model_returns_error() {
        let args: Args =
            serde_json::from_value(json!({"op": "set_global"})).expect("set_global args");

        let err = args.required_model().expect_err("missing model");

        assert!(err.contains("missing required field 'model'"));
    }
}
