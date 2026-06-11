//! User-facing formatting for model tool output.

use crabgent_command::tool_wrap::stringify_tool_output;
use serde_json::Value;

use crate::parser::CliArgs;

const MAX_FORMATTED_MODELS: usize = 20;

pub fn format_reply(args: &CliArgs, output: &Value) -> String {
    match args {
        CliArgs::List => format_model_list(output),
        CliArgs::Get { .. } => format_model_get(output),
        CliArgs::Set { .. } => format_model_set(output),
    }
}

fn format_model_list(output: &Value) -> String {
    let Some(models) = output.get("models").and_then(Value::as_array) else {
        return stringify_tool_output(output);
    };
    let total = output
        .get("total")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(models.len());
    let shown = models.len().min(MAX_FORMATTED_MODELS);
    let mut lines = vec![format!("Models (showing {shown} of {total})")];
    for model in models.iter().take(MAX_FORMATTED_MODELS) {
        lines.push(format!(
            "- {} [{}] {}",
            str_field(model, "id"),
            str_field(model, "provider"),
            str_field(model, "display_name"),
        ));
    }
    if total > shown {
        lines.push(format!("... {} more models not shown", total - shown));
    }
    lines.join("\n")
}

fn format_model_get(output: &Value) -> String {
    let Some(model) = output.get("model") else {
        return stringify_tool_output(output);
    };
    let caps = model.get("caps").unwrap_or(&Value::Null);
    format!(
        "{} [{}]\n{}\ncapabilities: tools={} vision={} audio={} thinking={} prompt_cache={} max_input={} max_output={}",
        str_field(model, "id"),
        str_field(model, "provider"),
        str_field(model, "display_name"),
        bool_field(caps, "supports_tools"),
        bool_field(caps, "supports_vision"),
        bool_field(caps, "supports_audio"),
        bool_field(caps, "supports_thinking"),
        bool_field(caps, "supports_prompt_cache"),
        number_field(caps, "max_input_tokens"),
        number_field(caps, "max_output_tokens"),
    )
}

fn format_model_set(output: &Value) -> String {
    let model = str_field(output, "model");
    let session_id = str_field(output, "session_id");
    format!("Session model override set to {model} for session {session_id}.")
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("?")
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn number_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use crabgent_core::ModelId;
    use serde_json::json;

    use super::*;

    #[test]
    fn format_model_list_truncates_long_lists() {
        let models: Vec<Value> = (0..25)
            .map(|idx| {
                json!({
                    "id": format!("model-{idx}"),
                    "provider": "test",
                    "display_name": "Test Model",
                })
            })
            .collect();

        let text = format_reply(
            &CliArgs::List,
            &json!({
                "count": 25,
                "total": 25,
                "truncated": false,
                "models": models,
            }),
        );

        assert!(text.contains("Models (showing 20 of 25)"));
        assert!(text.contains("... 5 more models not shown"));
        assert!(!text.contains("model-24"));
    }

    #[test]
    fn format_model_get_includes_capabilities() {
        let text = format_reply(
            &CliArgs::Get {
                id: ModelId::new("sonnet"),
            },
            &json!({
                "model": {
                    "id": "claude-sonnet-4-6",
                    "provider": "anthropic",
                    "display_name": "Claude Sonnet 4.6",
                    "caps": {
                        "supports_tools": true,
                        "supports_vision": true,
                        "supports_audio": false,
                        "supports_thinking": true,
                        "supports_prompt_cache": true,
                        "max_input_tokens": 200_000,
                        "max_output_tokens": 8192
                    }
                }
            }),
        );

        assert!(text.contains("claude-sonnet-4-6 [anthropic]"));
        assert!(text.contains("tools=true"));
        assert!(text.contains("vision=true"));
        assert!(text.contains("max_input=200000"));
    }
}
