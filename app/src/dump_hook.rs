//! Hook that dumps the full `LlmRequest` and `LlmResponse`
//! payloads through `crabgent_log` so we can see exactly what the
//! configured provider sends and receives without patching the
//! upstream provider crates.
//!
//! Activated when `CRABGENT_DUMP_LLM_PAYLOAD` is set to a non-empty value
//! at process start. Disabled otherwise (the hook stays in the chain
//! but short-circuits without serialising).

use async_trait::async_trait;
use crabgent_core::{Decision, Hook, LlmRequest, LlmResponse, RunCtx};
use crabgent_log::info;
use serde::Serialize;
use serde_json::Value;

pub struct DumpPayloadHook {
    enabled: bool,
}

impl DumpPayloadHook {
    #[must_use]
    pub fn from_env() -> Self {
        let enabled =
            std::env::var("CRABGENT_DUMP_LLM_PAYLOAD").is_ok_and(|value| !value.trim().is_empty());
        Self { enabled }
    }
}

#[async_trait]
impl Hook for DumpPayloadHook {
    async fn before_llm(&self, req: &LlmRequest, ctx: &RunCtx) -> Decision<LlmRequest> {
        if self.enabled {
            match redacted_json_string(req) {
                Ok(payload) => info!(
                    run_id = %ctx.run_id,
                    model = %req.model,
                    payload = %payload,
                    "dump: llm request",
                ),
                Err(err) => info!(
                    run_id = %ctx.run_id,
                    error = %err,
                    "dump: llm request serialize failed",
                ),
            }
        }
        Decision::Continue
    }

    async fn after_llm(
        &self,
        _req: &LlmRequest,
        resp: &LlmResponse,
        ctx: &RunCtx,
    ) -> Decision<LlmResponse> {
        if self.enabled {
            match redacted_json_string(resp) {
                Ok(payload) => info!(
                    run_id = %ctx.run_id,
                    model = %resp.model,
                    payload = %payload,
                    "dump: llm response",
                ),
                Err(err) => info!(
                    run_id = %ctx.run_id,
                    error = %err,
                    "dump: llm response serialize failed",
                ),
            }
        }
        Decision::Continue
    }
}

fn redacted_json_string<T: Serialize>(value: &T) -> serde_json::Result<String> {
    let mut value = serde_json::to_value(value)?;
    redact_large_inline_bytes(&mut value);
    serde_json::to_string(&value)
}

fn redact_large_inline_bytes(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let key_lower = key.to_ascii_lowercase();
                if key_lower.contains("base64") {
                    *value = redaction_value(value);
                } else {
                    redact_large_inline_bytes(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_large_inline_bytes(value);
            }
        }
        Value::String(text) if looks_like_large_base64(text) => {
            *value = redaction_value(value);
        }
        _ => {}
    }
}

fn redaction_value(value: &Value) -> Value {
    let len = value.as_str().map_or(0, str::len);
    Value::String(format!("[REDACTED base64 len={len}]"))
}

fn looks_like_large_base64(text: &str) -> bool {
    const MIN_BASE64_LEN: usize = 8192;
    if text.len() < MIN_BASE64_LEN {
        return false;
    }
    text.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'\r' | b'\n'))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::redacted_json_string;

    #[test]
    fn redacted_json_string_hides_content_base64() {
        let payload = json!({
            "tool_calls": [{
                "name": "channel_upload",
                "args": {
                    "content_base64": "aGk=",
                    "filename": "a.txt"
                }
            }]
        });

        let rendered = redacted_json_string(&payload).expect("json");

        assert!(rendered.contains("[REDACTED base64 len=4]"));
        assert!(!rendered.contains("aGk="));
        assert!(rendered.contains("a.txt"));
    }
}
