//! Wire format between the subprocess hook adapter and the script it
//! invokes. One JSON object per call on stdin, one JSON `Decision` on
//! stdout. The script gets a stable schema regardless of which kernel
//! event triggered the call.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The single JSON object written to the subprocess on stdin.
///
/// `event` names the hook callback (`"before_llm"`, `"on_event"`, ...).
/// `ctx` carries the run id and subject id. `payload` is the
/// event-specific payload, serialized from the relevant kernel type.
#[derive(Debug, Serialize)]
pub struct HookInput<'a> {
    pub event: &'static str,
    pub ctx: HookCtx<'a>,
    pub payload: Value,
}

#[derive(Debug, Serialize)]
pub struct HookCtx<'a> {
    pub run_id: &'a str,
    pub subject_id: &'a str,
}

/// Decision returned by the subprocess on stdout. Mirrors
/// `crabgent_core::Decision` but uses `Value` for the replacement payload
/// since the subprocess speaks JSON, not Rust types.
#[derive(Debug, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum HookOutput {
    Continue,
    Replace { value: Value },
    Deny { reason: String },
}

/// What the adapter does when the subprocess fails (spawn error, time
/// out, malformed output, non-zero exit). `Strict` is the default and
/// surfaces as `Decision::Deny` so the kernel fails closed. `Lenient`
/// logs the error and continues, useful for advisory hooks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FailureMode {
    #[default]
    Strict,
    Lenient,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn input_serializes_event_ctx_payload() {
        let inp = HookInput {
            event: "before_llm",
            ctx: HookCtx {
                run_id: "r1",
                subject_id: "u1",
            },
            payload: json!({"model": "claude"}),
        };
        let v = serde_json::to_value(&inp).expect("ser");
        assert_eq!(v["event"], "before_llm");
        assert_eq!(v["ctx"]["run_id"], "r1");
        assert_eq!(v["ctx"]["subject_id"], "u1");
        assert_eq!(v["payload"]["model"], "claude");
    }

    #[test]
    fn output_continue_deserializes() {
        let out: HookOutput = serde_json::from_str(r#"{"decision":"continue"}"#).expect("de");
        assert!(matches!(out, HookOutput::Continue));
    }

    #[test]
    fn output_replace_carries_value() {
        let out: HookOutput =
            serde_json::from_str(r#"{"decision":"replace","value":{"x":1}}"#).expect("de");
        match out {
            HookOutput::Replace { value } => assert_eq!(value["x"], 1),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn output_deny_carries_reason() {
        let out: HookOutput =
            serde_json::from_str(r#"{"decision":"deny","reason":"nope"}"#).expect("de");
        match out {
            HookOutput::Deny { reason } => assert_eq!(reason, "nope"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn failure_mode_default_is_strict() {
        assert_eq!(FailureMode::default(), FailureMode::Strict);
    }
}
