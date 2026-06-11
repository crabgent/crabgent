use crabgent_core::{LlmRequest, ToolChoice};
use serde_json::{Map, Value, json};

use super::{should_include_server_side_tool_invocations, tools_to_google};

/// Inserts the Gemini `tools` array and the merged `toolConfig` into `body`.
/// Tools are omitted when empty; `toolConfig` is omitted when neither the
/// server-side invocation flag nor a function-calling config applies.
pub fn insert_tools_and_config(body: &mut Map<String, Value>, req: &LlmRequest) {
    let tools = tools_to_google(req);
    let tools_present = !tools.is_empty();
    if tools_present {
        body.insert("tools".to_owned(), Value::Array(tools));
    }
    if let Some(tc) = build_tool_config(
        should_include_server_side_tool_invocations(req),
        req.tool_choice.as_ref(),
        tools_present,
    ) {
        body.insert("toolConfig".to_owned(), tc);
    }
}

/// Maps a provider-neutral tool choice to Gemini `functionCallingConfig`.
pub fn function_calling_config(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({"mode": "AUTO"}),
        ToolChoice::Any => json!({"mode": "ANY"}),
        ToolChoice::None => json!({"mode": "NONE"}),
        ToolChoice::Tool(name) => json!({"mode": "ANY", "allowedFunctionNames": [name]}),
    }
}

/// Assembles the Gemini `toolConfig`, merging the server-side tool-invocation
/// flag with the optional function-calling config. `None` when neither applies.
pub fn build_tool_config(
    include_server_side: bool,
    tool_choice: Option<&ToolChoice>,
    tools_present: bool,
) -> Option<Value> {
    let mut cfg = Map::new();
    if include_server_side {
        cfg.insert("includeServerSideToolInvocations".to_owned(), json!(true));
    }
    if tools_present && let Some(tc) = tool_choice {
        cfg.insert(
            "functionCallingConfig".to_owned(),
            function_calling_config(tc),
        );
    }
    if cfg.is_empty() {
        None
    } else {
        Some(Value::Object(cfg))
    }
}
