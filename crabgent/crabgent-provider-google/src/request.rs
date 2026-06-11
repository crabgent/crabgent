//! Request body mapping for the Google Gemini API.

use std::collections::HashMap;

use crabgent_core::{
    LlmRequest, ProjectedContent, ProjectedToolCall, ProjectedTurn, ReasoningEffort,
    project_conversation,
};
use serde_json::{Map, Value, json};

mod tool_config;
mod value;
use value::value_to_string;

const GEMINI_SCHEMA_META_KEYS: &[&str] = &["$schema", "$id", "$comment", "$defs", "$ref"];
const GEMINI_ACCEPTED_FORMATS: &[&str] = &["date-time", "uuid"];
const SCHEMA_COMBINERS: &[&str] = &["oneOf", "anyOf"];

pub fn build_generate_content_body(req: &LlmRequest) -> Value {
    build_generate_content_body_inner(req, None)
}

pub fn build_generate_content_body_with_cache(req: &LlmRequest, cached_content: &str) -> Value {
    build_generate_content_body_inner(req, Some(cached_content))
}

pub fn build_cached_content_body(req: &LlmRequest) -> Option<Value> {
    let tools = tools_to_google(req);
    if req.system_prompt.is_none() && tools.is_empty() {
        return None;
    }

    let mut body = Map::new();
    body.insert(
        "model".to_owned(),
        json!(google_model_resource(req.model.as_str())),
    );
    body.insert("ttl".to_owned(), json!("3600s"));
    if let Some(system) = &req.system_prompt {
        body.insert(
            "systemInstruction".to_owned(),
            json!({"parts": [{"text": system}]}),
        );
    }
    if !tools.is_empty() {
        body.insert("tools".to_owned(), Value::Array(tools));
    }
    if should_include_server_side_tool_invocations(req) {
        body.insert(
            "toolConfig".to_owned(),
            json!({"includeServerSideToolInvocations": true}),
        );
    }
    Some(Value::Object(body))
}

fn build_generate_content_body_inner(req: &LlmRequest, cached_content: Option<&str>) -> Value {
    let mut body = Map::new();
    if let Some(cache_name) = cached_content {
        body.insert("cachedContent".to_owned(), json!(cache_name));
    } else if let Some(system) = &req.system_prompt {
        body.insert(
            "systemInstruction".to_owned(),
            json!({"parts": [{"text": system}]}),
        );
    }
    let messages = project_conversation(&req.messages);
    body.insert(
        "contents".to_owned(),
        Value::Array(transform_messages(&messages)),
    );
    let generation_config = generation_config(req);
    if !generation_config.is_empty() {
        body.insert(
            "generationConfig".to_owned(),
            Value::Object(generation_config),
        );
    }
    if cached_content.is_none() {
        tool_config::insert_tools_and_config(&mut body, req);
    }
    Value::Object(body)
}

fn google_model_resource(model: &str) -> String {
    format!("models/{}", model.trim_start_matches("models/"))
}

fn transform_messages(messages: &[ProjectedTurn]) -> Vec<Value> {
    let mut tool_call_names = HashMap::new();
    messages
        .iter()
        .filter_map(|message| transform_message(message, &mut tool_call_names))
        .collect()
}

fn generation_config(req: &LlmRequest) -> Map<String, Value> {
    let mut config = Map::new();
    if let Some(max_tokens) = req.max_tokens {
        config.insert("maxOutputTokens".to_owned(), json!(max_tokens));
    }
    if let Some(temperature) = req.temperature {
        config.insert("temperature".to_owned(), json!(temperature));
    }
    if !req.stop_sequences.is_empty() {
        config.insert("stopSequences".to_owned(), json!(req.stop_sequences));
    }
    if let Some(effort) = req.reasoning_effort
        && model_supports_thinking(req.model.as_str())
    {
        config.insert(
            "thinkingConfig".to_owned(),
            json!({
                "thinkingBudget": thinking_budget(effort),
                "includeThoughts": true
            }),
        );
    }
    config
}

fn transform_message(
    message: &ProjectedTurn,
    tool_call_names: &mut HashMap<String, String>,
) -> Option<Value> {
    match message {
        ProjectedTurn::Assistant { text, tool_calls } => {
            let parts = assistant_parts(text, tool_calls, tool_call_names);
            (!parts.is_empty()).then(|| json!({"role": "model", "parts": parts}))
        }
        ProjectedTurn::ToolResult {
            call_id,
            output,
            is_error,
        } => Some(json!({
            "role": "user",
            "parts": [tool_result_part(call_id, output.clone(), *is_error, tool_call_names)]
        })),
        ProjectedTurn::User { content, raw } => {
            Some(json!({"role": "user", "parts": user_parts(content.as_ref(), raw)}))
        }
        ProjectedTurn::Unknown {
            role: Some(_), raw, ..
        } => Some(json!({"role": "user", "parts": user_parts_from_raw(raw)})),
        // System, ChannelOutbound, ProviderBlock, Unknown { role: None },
        // and future non-exhaustive variants are all no-ops for Google wire.
        _ => None,
    }
}

fn user_parts(content: Option<&ProjectedContent>, raw: &Value) -> Vec<Value> {
    match content {
        Some(ProjectedContent::Raw(content)) => vec![json!({"text": value_to_string(content)})],
        Some(ProjectedContent::Blocks(blocks)) => blocks
            .iter()
            .filter_map(projected_user_block_to_part)
            .collect(),
        None | Some(_) => vec![json!({"text": raw.to_string()})],
    }
}

fn user_parts_from_raw(message: &Value) -> Vec<Value> {
    let Some(content) = message.get("content") else {
        return vec![json!({"text": message.to_string()})];
    };
    let Some(blocks) = content.as_array() else {
        return vec![json!({"text": value_to_string(content)})];
    };
    blocks.iter().filter_map(raw_user_block_to_part).collect()
}

fn projected_user_block_to_part(block: &ProjectedContent) -> Option<Value> {
    match block {
        ProjectedContent::Text { text, .. } => Some(json!({"text": text})),
        ProjectedContent::Image { mime, data, .. } => {
            Some(json!({"inlineData": {"mimeType": mime, "data": data}}))
        }
        ProjectedContent::Other(raw) => raw_audio_block_to_part(raw),
        ProjectedContent::Raw(_) | ProjectedContent::Blocks(_) => None,
    }
}

fn raw_user_block_to_part(block: &Value) -> Option<Value> {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"text": text})),
        Some("image") => {
            let mime = block.get("mime").and_then(Value::as_str)?;
            let data = block.get("data").and_then(Value::as_str)?;
            Some(json!({"inlineData": {"mimeType": mime, "data": data}}))
        }
        Some("audio") => raw_audio_block_to_part(block),
        _ => None,
    }
}

fn raw_audio_block_to_part(block: &Value) -> Option<Value> {
    let mime = block.get("mime").and_then(Value::as_str)?;
    let data = block.get("data").and_then(Value::as_str)?;
    Some(json!({"inlineData": {"mimeType": mime, "data": data}}))
}

fn assistant_parts(
    text: &str,
    calls: &[ProjectedToolCall],
    tool_call_names: &mut HashMap<String, String>,
) -> Vec<Value> {
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(json!({"text": text}));
    }
    parts.extend(
        calls
            .iter()
            .filter_map(|call| tool_call_part(call, tool_call_names)),
    );
    parts
}

fn tool_call_part(
    call: &ProjectedToolCall,
    tool_call_names: &mut HashMap<String, String>,
) -> Option<Value> {
    let name = call.name.as_deref()?;
    tool_call_names.insert(call.id.clone(), name.to_owned());

    let mut function_call = Map::new();
    function_call.insert("id".to_owned(), json!(call.id));
    function_call.insert("name".to_owned(), json!(name));
    function_call.insert("args".to_owned(), call.args.clone());

    let mut part = Map::new();
    part.insert("functionCall".to_owned(), Value::Object(function_call));
    if let Some(signature) = &call.thought_signature {
        part.insert("thoughtSignature".to_owned(), json!(signature));
    }
    Some(Value::Object(part))
}

fn tool_result_part(
    call_id: &str,
    output: Value,
    is_error: bool,
    tool_call_names: &HashMap<String, String>,
) -> Value {
    let name = tool_call_names.get(call_id).map_or(call_id, String::as_str);
    json!({
        "functionResponse": {
            "id": call_id,
            "name": name,
            "response": function_response_payload(output, is_error),
        }
    })
}

fn function_response_payload(output: Value, is_error: bool) -> Value {
    if is_error {
        json!({"error": output})
    } else if output.is_object() {
        output
    } else {
        json!({"result": output})
    }
}

fn tools_to_google(req: &LlmRequest) -> Vec<Value> {
    let web_search = req.web_search.enabled;
    let has_tools = !req.tools.is_empty();
    if !has_tools && !web_search {
        return Vec::new();
    }
    let mut elements = Vec::new();
    if has_tools {
        let declarations: Vec<Value> = req
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": sanitize_schema_for_gemini(&tool.input_schema),
                })
            })
            .collect();
        elements.push(json!({"functionDeclarations": declarations}));
    }
    if web_search {
        elements.push(json!({"google_search": {}}));
    }
    elements
}

const fn should_include_server_side_tool_invocations(req: &LlmRequest) -> bool {
    !req.tools.is_empty() && req.web_search.enabled
}

const fn thinking_budget(effort: ReasoningEffort) -> i64 {
    match effort {
        ReasoningEffort::Disabled | ReasoningEffort::Low => 0,
        ReasoningEffort::Medium => 8_000,
        ReasoningEffort::High => 24_000,
        ReasoningEffort::XHigh => 32_000,
    }
}

fn model_supports_thinking(model: &str) -> bool {
    let id = model.trim_start_matches("models/");
    id.starts_with("gemini-2.5") || id.starts_with("gemini-3")
}

fn sanitize_schema_for_gemini(schema: &Value) -> Value {
    sanitize_schema_value(schema)
}

fn sanitize_schema_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => sanitize_schema_object(object),
        Value::Array(values) => Value::Array(values.iter().map(sanitize_schema_value).collect()),
        _ => value.clone(),
    }
}

fn sanitize_schema_object(object: &Map<String, Value>) -> Value {
    if let Some((keyword, branch)) = single_non_null_schema_branch(object) {
        let mut out = match sanitize_schema_value(branch) {
            Value::Object(branch_object) => branch_object,
            branch_value => {
                let mut branch_object = Map::new();
                branch_object.insert("const".to_owned(), branch_value);
                branch_object
            }
        };
        for (key, value) in object {
            if key == keyword {
                continue;
            }
            insert_sanitized_schema_field(&mut out, key, value);
        }
        return Value::Object(out);
    }

    let mut out = Map::new();
    for (key, value) in object {
        insert_sanitized_schema_field(&mut out, key, value);
    }
    Value::Object(out)
}

fn insert_sanitized_schema_field(out: &mut Map<String, Value>, key: &str, value: &Value) {
    if GEMINI_SCHEMA_META_KEYS.contains(&key) {
        return;
    }

    match key {
        "type" => insert_sanitized_type(out, value),
        "additionalProperties" => insert_sanitized_additional_properties(out, value),
        "format" => insert_sanitized_format(out, value),
        "properties" => insert_sanitized_properties(out, value),
        "nullable" => {
            if out.get("nullable") == Some(&Value::Bool(true)) && value == &Value::Bool(false) {
                return;
            }
            out.insert(key.to_owned(), value.clone());
        }
        _ => {
            out.insert(key.to_owned(), sanitize_schema_value(value));
        }
    }
}

fn insert_sanitized_type(out: &mut Map<String, Value>, value: &Value) {
    let Value::Array(types) = value else {
        out.insert("type".to_owned(), value.clone());
        return;
    };

    let (concrete_types, nullable) = split_schema_type_array(types);
    insert_concrete_schema_type(out, &concrete_types);
    if nullable {
        out.insert("nullable".to_owned(), Value::Bool(true));
    }
}

fn split_schema_type_array(types: &[Value]) -> (Vec<&str>, bool) {
    let mut concrete_types = Vec::new();
    let mut nullable = false;
    for schema_type in types.iter().filter_map(Value::as_str) {
        if schema_type == "null" {
            nullable = true;
        } else {
            concrete_types.push(schema_type);
        }
    }
    (concrete_types, nullable)
}

fn insert_concrete_schema_type(out: &mut Map<String, Value>, concrete_types: &[&str]) {
    let Some(selected_type) = concrete_types.first() else {
        crabgent_log::debug!(
            "dropped Gemini-incompatible JSON schema type array without concrete type"
        );
        return;
    };
    log_dropped_schema_types(selected_type, concrete_types);
    out.insert("type".to_owned(), json!(selected_type));
}

fn log_dropped_schema_types(selected_type: &str, concrete_types: &[&str]) {
    if concrete_types.len() <= 1 {
        return;
    }
    let dropped_types = concrete_types.get(1..).unwrap_or_default();
    crabgent_log::debug!(
        selected_type = %selected_type,
        dropped_types = ?dropped_types,
        "collapsed Gemini-incompatible JSON schema type union"
    );
}

fn insert_sanitized_additional_properties(out: &mut Map<String, Value>, value: &Value) {
    match value {
        Value::Bool(true) => {
            out.insert("additionalProperties".to_owned(), Value::Bool(true));
        }
        Value::Object(_) => {
            out.insert(
                "additionalProperties".to_owned(),
                sanitize_schema_value(value),
            );
        }
        _ => {}
    }
}

fn insert_sanitized_format(out: &mut Map<String, Value>, value: &Value) {
    let Some(format) = value.as_str() else {
        return;
    };
    if GEMINI_ACCEPTED_FORMATS.contains(&format) {
        out.insert("format".to_owned(), value.clone());
    }
}

fn insert_sanitized_properties(out: &mut Map<String, Value>, value: &Value) {
    let Value::Object(properties) = value else {
        out.insert("properties".to_owned(), sanitize_schema_value(value));
        return;
    };

    let mut out_properties = Map::new();
    for (name, property_schema) in properties {
        out_properties.insert(name.clone(), sanitize_schema_value(property_schema));
    }
    out.insert("properties".to_owned(), Value::Object(out_properties));
}

fn single_non_null_schema_branch(object: &Map<String, Value>) -> Option<(&'static str, &Value)> {
    for keyword in SCHEMA_COMBINERS {
        let Some(branches) = object.get(*keyword).and_then(Value::as_array) else {
            continue;
        };
        let non_null_branches: Vec<&Value> = branches
            .iter()
            .filter(|branch| !is_null_schema_branch(branch))
            .collect();
        if let [branch] = non_null_branches.as_slice() {
            return Some((keyword, branch));
        }
    }
    None
}

fn is_null_schema_branch(value: &Value) -> bool {
    let Some(schema_type) = value.get("type") else {
        return false;
    };
    match schema_type {
        Value::String(schema_type) => schema_type == "null",
        Value::Array(types) => types
            .iter()
            .filter_map(Value::as_str)
            .all(|schema_type| schema_type == "null"),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
